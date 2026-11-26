//! Unified Session/Actor/Delegation/Mailbox orchestration engine.
//!
//! `CoreEngine` is the sole authority for session lifecycles, monotonic
//! cursor/revision stamps, optimistic concurrency, delegation invariants,
//! scoped actor authentication, mailbox isolation, wait-graph cycle checks,
//! workspace writer leases, and bounded artifact management.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, DriverSession, EventReceiver, InteractionAnswer,
    McpServerInjection, NormalizedEvent, ResumeSpec, SessionSpec, ToolActivityStatus, TurnInput,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::artifacts::ArtifactStore;
use crate::error::CoreError;
use crate::roles;
use crate::schema::validate_json_schema;
use crate::store::{
    NewDurableEvent, NewMailboxRow, NewSessionRow, ResumeSessionUpdate, Store, generate_actor_token,
};
use crate::types::{
    ActorScope, AnswerInteractionRequest, AnswerInteractionResponse, ArtifactRef, CallerContext,
    CapabilitiesSnapshot, CoreConfig, CreateSessionRequest, Cursor, InteractionList,
    InteractionStatus, MailboxMessage, MailboxPage, PostMailboxRequest, ReplayResult,
    ResumeSessionRequest, RoleDetail, RoleSummary, SchemaValidationResult, SessionEvent,
    SessionFilter, SessionSnapshot, SessionStatus, SizeBounds, SubmitTurnRequest, TimelinePage,
    TurnAccepted, WaitMode, WaitSessionsRequest, WaitSessionsResponse, WriterLease, now_rfc3339,
};

const MIN_BROADCAST_CAPACITY: usize = 4;

type LiveAnswerHandle = (Arc<Box<dyn DriverSession>>, Arc<tokio::sync::Mutex<()>>);

struct LiveSession {
    driver_session: Arc<Box<dyn DriverSession>>,
    answer_lock: Arc<tokio::sync::Mutex<()>>,
    generation: u64,
    drain_handle: JoinHandle<()>,
}

#[derive(Default)]
struct WaitGraphState {
    entries: HashMap<u64, (String, Vec<String>)>,
}

struct WaitGraph {
    next_id: AtomicU64,
    state: Mutex<WaitGraphState>,
}

impl WaitGraph {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            state: Mutex::new(WaitGraphState::default()),
        }
    }

    fn register(
        self: &Arc<Self>,
        waiter: String,
        targets: Vec<String>,
    ) -> Result<WaitGuard, CoreError> {
        if targets.iter().any(|t| t == &waiter) {
            return Err(CoreError::delegation_cycle(format!(
                "session '{waiter}' cannot wait on itself"
            )));
        }

        let mut guard = self
            .state
            .lock()
            .map_err(|_| CoreError::internal("wait graph lock poisoned"))?;

        // Build adjacency map from existing edges and run DFS from each target.
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for (src, dsts) in guard.entries.values() {
            let entry = adj.entry(src.as_str()).or_default();
            for dst in dsts {
                entry.push(dst.as_str());
            }
        }

        let mut visited: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = targets.iter().map(String::as_str).collect();
        while let Some(node) = stack.pop() {
            if node == waiter.as_str() {
                return Err(CoreError::delegation_cycle(format!(
                    "cyclic wait detected involving session '{waiter}'"
                )));
            }
            if !visited.insert(node) {
                continue;
            }
            if let Some(nexts) = adj.get(node) {
                stack.extend(nexts.iter().copied());
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        guard.entries.insert(id, (waiter, targets));
        Ok(WaitGuard {
            graph: Arc::clone(self),
            id,
        })
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut guard) = self.state.lock() {
            guard.entries.remove(&id);
        }
    }
}

struct WaitGuard {
    graph: Arc<WaitGraph>,
    id: u64,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.graph.unregister(self.id);
    }
}

struct EngineInner {
    config: CoreConfig,
    store: Store,
    artifacts: ArtifactStore,
    drivers: HashMap<String, Arc<dyn AgentDriver>>,
    live_sessions: Mutex<HashMap<String, LiveSession>>,
    channels: Mutex<HashMap<String, broadcast::Sender<SessionEvent>>>,
    wait_graph: Arc<WaitGraph>,
    wait_notify: Arc<Notify>,
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.live_sessions.lock() {
            for (_, live) in guard.drain() {
                live.drain_handle.abort();
            }
        }
    }
}

impl EngineInner {
    fn broadcast_sender(&self, session_id: &str) -> broadcast::Sender<SessionEvent> {
        let cap = self
            .config
            .event_broadcast_capacity
            .max(MIN_BROADCAST_CAPACITY);
        let mut guard = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .entry(session_id.to_owned())
            .or_insert_with(|| broadcast::channel(cap).0)
            .clone()
    }

    fn emit_live(&self, session_id: &str, event: SessionEvent) {
        let tx = {
            let guard = self.channels.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(session_id).cloned()
        };
        if let Some(sender) = tx {
            let _ = sender.send(event);
        }
    }

    fn get_live_driver(&self, session_id: &str) -> Option<(Arc<Box<dyn DriverSession>>, u64)> {
        let guard = self.live_sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(session_id)
            .map(|l| (Arc::clone(&l.driver_session), l.generation))
    }

    fn get_live_answer_handle(&self, session_id: &str) -> Option<LiveAnswerHandle> {
        let guard = self.live_sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(session_id)
            .map(|l| (Arc::clone(&l.driver_session), Arc::clone(&l.answer_lock)))
    }

    fn install_live_session(
        self: &Arc<Self>,
        session_id: String,
        generation: u64,
        root_session_id: String,
        native_output_schema: bool,
        mut driver_session: Box<dyn DriverSession>,
    ) {
        let event_rx = driver_session.take_events();
        let driver_arc = Arc::new(driver_session);
        let _ = self.broadcast_sender(&session_id);

        let weak = Arc::downgrade(self);
        let sid_clone = session_id.clone();
        let drain_handle = tokio::spawn(async move {
            if let Some(rx) = event_rx {
                drain_driver_events(
                    weak,
                    sid_clone,
                    root_session_id,
                    generation,
                    native_output_schema,
                    rx,
                )
                .await;
            }
        });

        let mut guard = self.live_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = guard.insert(
            session_id,
            LiveSession {
                driver_session: driver_arc,
                answer_lock: Arc::new(tokio::sync::Mutex::new(())),
                generation,
                drain_handle,
            },
        ) {
            prev.drain_handle.abort();
        }
    }

    fn remove_live_if_generation(&self, session_id: &str, generation: u64) {
        let mut guard = self.live_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .get(session_id)
            .is_some_and(|live| live.generation == generation)
        {
            guard.remove(session_id);
        }
    }

    fn take_live_session(&self, session_id: &str) -> Option<LiveSession> {
        let mut guard = self.live_sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(session_id)
    }
}

/// Core orchestration engine for Dolgorae v2.
pub struct CoreEngine {
    inner: Arc<EngineInner>,
}

impl CoreEngine {
    /// Initializes SQLite persistence, content-addressed artifact storage,
    /// driver registry, and runs deterministic startup crash recovery.
    pub async fn new(
        config: CoreConfig,
        drivers: Vec<Arc<dyn AgentDriver>>,
    ) -> Result<Self, CoreError> {
        let artifacts = ArtifactStore::open(&config.home_dir)?;
        let store = Store::open(&config.home_dir, config.max_retained_events_per_session)?;
        let _ = store.recover_on_startup().await?;

        let mut driver_map = HashMap::new();
        for drv in drivers {
            driver_map.insert(drv.driver_id().to_owned(), drv);
        }

        Ok(Self {
            inner: Arc::new(EngineInner {
                config,
                store,
                artifacts,
                drivers: driver_map,
                live_sessions: Mutex::new(HashMap::new()),
                channels: Mutex::new(HashMap::new()),
                wait_graph: Arc::new(WaitGraph::new()),
                wait_notify: Arc::new(Notify::new()),
            }),
        })
    }

    /// Probes all registered drivers and returns daemon capabilities and bounds.
    pub async fn capabilities(&self) -> Result<CapabilitiesSnapshot, CoreError> {
        let mut driver_caps = Vec::with_capacity(self.inner.drivers.len());
        let mut ids: Vec<&String> = self.inner.drivers.keys().collect();
        ids.sort();
        for id in ids {
            let drv = &self.inner.drivers[id];
            match drv.probe().await {
                Ok(caps) => driver_caps.push(caps),
                Err(err) => driver_caps.push(dolgorae_spi::DriverCapabilities {
                    driver_id: id.clone(),
                    cli_version: None,
                    available: false,
                    unavailable_reason: Some(err.to_string()),
                    models: Vec::new(),
                    supports_effort: false,
                    supports_interactive_approval: false,
                    native_output_schema: false,
                    supports_mcp_injection: false,
                    supports_resume: false,
                }),
            }
        }
        Ok(CapabilitiesSnapshot {
            api_version: "v2".to_owned(),
            bounds: SizeBounds::default(),
            drivers: driver_caps,
        })
    }

    /// Lists resolved roles for an optional workspace (`<workspace>/.dolgorae/roles`
    /// shadowing `<home_dir>/roles`).
    pub fn list_roles(&self, workspace: Option<&Path>) -> Result<Vec<RoleSummary>, CoreError> {
        roles::list_roles(&self.inner.config.home_dir, workspace)
    }

    /// Loads one resolved role by name.
    pub fn get_role(&self, name: &str, workspace: Option<&Path>) -> Result<RoleDetail, CoreError> {
        roles::load_role(&self.inner.config.home_dir, workspace, name)
    }

    /// Verifies a scoped actor bearer token using constant-time digest comparison
    /// and active session/root scope binding.
    pub async fn authenticate_actor_token(&self, token: &str) -> Result<ActorScope, CoreError> {
        self.inner.store.verify_actor_token(token).await
    }

    /// Mints a fresh scoped actor bearer token for an active session (ExternalHost only).
    pub async fn mint_actor_token(
        &self,
        caller: &CallerContext,
        session_id: &str,
    ) -> Result<Zeroizing<String>, CoreError> {
        if !matches!(caller, CallerContext::ExternalHost) {
            return Err(CoreError::forbidden_scope(
                "only external host callers may mint actor tokens directly",
            ));
        }
        self.inner
            .store
            .mint_actor_token_for_session(session_id)
            .await
    }

    /// Creates a root session or delegated child actor, enforcing Invariant 4,
    /// role trust boundaries, writer leases, and immediate event-stream draining.
    pub async fn create_session(
        &self,
        caller: &CallerContext,
        req: CreateSessionRequest,
    ) -> Result<SessionSnapshot, CoreError> {
        let canon_ws = canonicalize_workspace(&req.workspace)?;
        let canon_ws_str = path_to_string(&canon_ws)?;

        let parent_id = match caller {
            CallerContext::ExternalHost => req.parent_session_id.clone(),
            CallerContext::ScopedActor(scope) => {
                if let Some(ref requested_parent) = req.parent_session_id
                    && requested_parent != &scope.session_id
                {
                    return Err(CoreError::forbidden_scope(
                        "scoped actor can only delegate children under its own session_id",
                    ));
                }
                if !canon_ws.starts_with(&scope.workspace) {
                    return Err(CoreError::forbidden_scope(format!(
                        "workspace '{}' escapes actor workspace '{}'",
                        canon_ws.display(),
                        scope.workspace.display()
                    )));
                }
                Some(scope.session_id.clone())
            }
        };

        let session_id = format!("sess-{}", Uuid::now_v7());
        let (root_session_id, depth, policy, parent_record) = if let Some(ref pid) = parent_id {
            let parent = self.inner.store.get_session_record(pid).await?;
            if matches!(
                parent.snapshot.status,
                SessionStatus::Closed | SessionStatus::Failed
            ) {
                return Err(CoreError::snapshot_conflict(
                    "cannot delegate under a closed or failed parent session",
                ));
            }
            let next_depth = parent.snapshot.depth.saturating_add(1);
            if next_depth > parent.snapshot.policy.max_delegation_depth {
                return Err(CoreError::delegation_depth_exceeded(format!(
                    "delegation depth {next_depth} exceeds max_delegation_depth {}",
                    parent.snapshot.policy.max_delegation_depth
                )));
            }
            if let Some(req_policy) = req.policy {
                req_policy.validate()?;
                if req_policy.max_delegation_depth > parent.snapshot.policy.max_delegation_depth
                    || (req_policy.allow_actor_collaboration
                        && !parent.snapshot.policy.allow_actor_collaboration)
                {
                    return Err(CoreError::forbidden_scope(
                        "child session cannot weaken parent delegation policy",
                    ));
                }
            }
            (
                parent.snapshot.root_session_id.clone(),
                next_depth,
                parent.snapshot.policy,
                Some(parent),
            )
        } else {
            let policy = req.policy.unwrap_or_default();
            policy.validate()?;
            (session_id.clone(), 0u8, policy, None)
        };

        let loaded_role = match req.role.as_deref() {
            Some(role_name) => Some(roles::load_role(
                &self.inner.config.home_dir,
                Some(&canon_ws),
                role_name,
            )?),
            None => None,
        };

        let driver_id = req
            .driver
            .clone()
            .or_else(|| loaded_role.as_ref().and_then(|r| r.summary.driver.clone()))
            .ok_or_else(|| {
                CoreError::bad_request("driver must be specified or provided by role")
            })?;

        let driver = self.inner.drivers.get(&driver_id).cloned().ok_or_else(|| {
            CoreError::driver_unavailable(format!("unknown driver '{driver_id}'"))
        })?;

        let caps = driver.probe().await?;
        if !caps.available {
            return Err(CoreError::driver_unavailable(
                caps.unavailable_reason
                    .unwrap_or_else(|| format!("driver '{driver_id}' is unavailable")),
            ));
        }

        let access = req
            .access
            .or_else(|| loaded_role.as_ref().map(|r| r.summary.access))
            .unwrap_or(AccessMode::ReadOnly);

        // Role trust boundary & non-escalation check.
        if let CallerContext::ScopedActor(scope) = caller
            && scope.access == AccessMode::ReadOnly
            && access == AccessMode::WorkspaceWrite
        {
            return Err(CoreError::forbidden_scope(
                "read_only scoped actor cannot escalate child access to workspace_write",
            ));
        }
        if let Some(ref parent) = parent_record {
            let parent_ws = Path::new(&parent.snapshot.workspace);
            if !canon_ws.starts_with(parent_ws) {
                return Err(CoreError::forbidden_scope(format!(
                    "child workspace '{}' escapes parent workspace '{}'",
                    canon_ws.display(),
                    parent_ws.display()
                )));
            }
            if parent.snapshot.access == AccessMode::ReadOnly
                && access == AccessMode::WorkspaceWrite
            {
                return Err(CoreError::forbidden_scope(
                    "child of read_only parent cannot escalate access to workspace_write",
                ));
            }
        }

        // Ancestor (role, canonical_workspace) re-entry cycle detection.
        if let (Some(pid), Some(role_name)) = (parent_id.as_deref(), req.role.as_deref()) {
            let ancestors = self.inner.store.get_ancestors(pid).await?;
            for anc in ancestors {
                if matches!(anc.status, SessionStatus::Active | SessionStatus::Idle)
                    && anc.role.as_deref() == Some(role_name)
                    && anc.workspace == canon_ws_str
                {
                    return Err(CoreError::delegation_cycle(format!(
                        "delegation cycle: ancestor '{}' already owns role '{}' on workspace '{}'",
                        anc.session_id, role_name, canon_ws_str
                    )));
                }
            }
        }

        // Optional single-writer lease conflict check.
        if access == AccessMode::WorkspaceWrite {
            self.inner
                .store
                .check_writer_lease_conflict(&canon_ws_str, None)
                .await?;
        }

        let approval_policy = req
            .approval_policy
            .or_else(|| loaded_role.as_ref().map(|r| r.summary.approval_policy))
            .unwrap_or(ApprovalPolicy::Interactive);

        if let Some(ref parent) = parent_record {
            check_approval_policy_escalation(parent.snapshot.approval_policy, approval_policy)?;
        }

        let model = req
            .model
            .clone()
            .or_else(|| loaded_role.as_ref().and_then(|r| r.summary.model.clone()));
        let effort = req
            .effort
            .clone()
            .or_else(|| loaded_role.as_ref().and_then(|r| r.summary.effort.clone()));
        let output_schema = req
            .output_schema
            .clone()
            .or_else(|| loaded_role.as_ref().and_then(|r| r.output_schema.clone()));
        let role_instructions = loaded_role.as_ref().map(|r| r.instructions.clone());
        let role_snapshot_sha256 = loaded_role
            .as_ref()
            .map(|r| r.summary.snapshot_sha256.clone());

        let mut actor_token_hash = None;
        let mut mcp_servers = Vec::new();
        if req.allow_delegation || parent_id.is_some() {
            let (raw_token, hash) = generate_actor_token()?;
            actor_token_hash = Some(hash);
            if caps.supports_mcp_injection
                && let Some(ref bin_path) = self.inner.config.mcp_bin_path
            {
                mcp_servers.push(build_scoped_mcp_injection(
                    bin_path,
                    self.inner.config.socket_path.as_deref(),
                    &session_id,
                    &root_session_id,
                    &raw_token,
                ));
            }
        }

        let spec = SessionSpec {
            workspace: canon_ws.clone(),
            role_instructions: role_instructions.clone(),
            model: model.clone(),
            effort: effort.clone(),
            access,
            approval_policy,
            output_schema: if caps.native_output_schema {
                output_schema.clone()
            } else {
                None
            },
            mcp_servers,
        };

        let driver_session = driver.start_session(spec).await?;
        let provider_session_id = driver_session.provider_session_id().to_owned();

        let (snapshot, created_ev) = match self
            .inner
            .store
            .insert_session(NewSessionRow {
                session_id: session_id.clone(),
                root_session_id: root_session_id.clone(),
                parent_session_id: parent_id,
                depth,
                policy,
                allow_delegation: req.allow_delegation,
                role: req.role,
                role_snapshot_sha256,
                role_instructions,
                driver: driver_id,
                provider_session_id,
                model,
                effort,
                workspace: canon_ws_str,
                access,
                approval_policy,
                output_schema,
                actor_token_hash,
            })
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                let _ = driver_session.close().await;
                return Err(err);
            }
        };

        self.inner.install_live_session(
            session_id.clone(),
            snapshot.session_generation,
            root_session_id,
            caps.native_output_schema,
            driver_session,
        );
        self.inner.emit_live(&session_id, created_ev);
        Ok(snapshot)
    }

    /// Fetches the current coherent snapshot of a session.
    pub async fn get_session(
        &self,
        caller: &CallerContext,
        session_id: &str,
    ) -> Result<SessionSnapshot, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        Ok(rec.snapshot)
    }

    /// Lists sessions visible to `caller`, automatically scoped to the actor's
    /// `root_session_id` when invoked by a `ScopedActor`.
    pub async fn list_sessions(
        &self,
        caller: &CallerContext,
        mut filter: SessionFilter,
    ) -> Result<Vec<SessionSnapshot>, CoreError> {
        if let CallerContext::ScopedActor(scope) = caller {
            if let Some(ref requested_root) = filter.root_session_id
                && requested_root != &scope.root_session_id
            {
                return Err(CoreError::forbidden_scope(
                    "scoped actor cannot query sessions outside its root_session_id",
                ));
            }
            filter.root_session_id = Some(scope.root_session_id.clone());
        }
        self.inner.store.list_sessions(filter).await
    }

    /// Resumes an idle/detached session, bumping `session_generation`, rotating
    /// any delegating actor token, re-injecting scoped MCP, and immediately
    /// attaching the driver event drain task.
    pub async fn resume_session(
        &self,
        caller: &CallerContext,
        session_id: &str,
        req: ResumeSessionRequest,
    ) -> Result<SessionSnapshot, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_direct_child(caller, &rec.snapshot, "resume_session")?;
        if rec.snapshot.status == SessionStatus::Closed {
            return Err(CoreError::snapshot_conflict(
                "cannot resume a closed session",
            ));
        }

        let driver = self
            .inner
            .drivers
            .get(&rec.snapshot.driver)
            .cloned()
            .ok_or_else(|| {
                CoreError::driver_unavailable(format!("unknown driver '{}'", rec.snapshot.driver))
            })?;

        let caps = driver.probe().await?;
        if !caps.available {
            return Err(CoreError::driver_unavailable(
                caps.unavailable_reason
                    .unwrap_or_else(|| format!("driver '{}' is unavailable", rec.snapshot.driver)),
            ));
        }

        let access = req.access.unwrap_or(rec.snapshot.access);
        if let CallerContext::ScopedActor(scope) = caller
            && scope.access == AccessMode::ReadOnly
            && access == AccessMode::WorkspaceWrite
        {
            return Err(CoreError::forbidden_scope(
                "read_only scoped actor cannot escalate session to workspace_write on resume",
            ));
        }
        if access == AccessMode::WorkspaceWrite {
            self.inner
                .store
                .check_writer_lease_conflict(&rec.snapshot.workspace, Some(session_id))
                .await?;
        }

        let model = req.model.or_else(|| rec.snapshot.model.clone());
        let effort = req.effort.or_else(|| rec.snapshot.effort.clone());
        let approval_policy = req.approval_policy.unwrap_or(rec.snapshot.approval_policy);
        if matches!(caller, CallerContext::ScopedActor(_)) {
            check_approval_policy_escalation(rec.snapshot.approval_policy, approval_policy)?;
        }

        // If the session is currently live, cleanly close the old driver handle before resuming.
        if let Some(old_live) = self.inner.take_live_session(session_id) {
            old_live.drain_handle.abort();
            let _ = old_live.driver_session.close().await;
        }

        let mut new_actor_token_hash = None;
        let mut mcp_servers = Vec::new();
        if rec.allow_delegation || rec.snapshot.parent_session_id.is_some() {
            let (raw_token, hash) = generate_actor_token()?;
            new_actor_token_hash = Some(hash);
            if caps.supports_mcp_injection
                && let Some(ref bin_path) = self.inner.config.mcp_bin_path
            {
                mcp_servers.push(build_scoped_mcp_injection(
                    bin_path,
                    self.inner.config.socket_path.as_deref(),
                    session_id,
                    &rec.snapshot.root_session_id,
                    &raw_token,
                ));
            }
        }

        let spec = ResumeSpec {
            provider_session_id: rec.provider_session_id.clone(),
            workspace: PathBuf::from(&rec.snapshot.workspace),
            role_instructions: rec.role_instructions.clone(),
            model: model.clone(),
            effort: effort.clone(),
            access,
            approval_policy,
            output_schema: if caps.native_output_schema {
                rec.output_schema.clone()
            } else {
                None
            },
            mcp_servers,
        };

        let driver_session = driver.resume_session(spec).await?;
        let new_provider_sid = driver_session.provider_session_id().to_owned();

        let (snapshot, resumed_evs) = self
            .inner
            .store
            .update_session_resumed(ResumeSessionUpdate {
                session_id: session_id.to_owned(),
                provider_session_id: new_provider_sid,
                model,
                effort,
                access,
                approval_policy,
                new_actor_token_hash,
            })
            .await?;

        self.inner.install_live_session(
            session_id.to_owned(),
            snapshot.session_generation,
            snapshot.root_session_id.clone(),
            caps.native_output_schema,
            driver_session,
        );
        for ev in resumed_evs {
            self.inner.emit_live(session_id, ev);
        }
        Ok(snapshot)
    }

    /// Gracefully closes a session, revoking actor tokens, expiring pending
    /// interactions, releasing writer leases, and waking any waiters.
    pub async fn close_session(
        &self,
        caller: &CallerContext,
        session_id: &str,
    ) -> Result<SessionSnapshot, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_direct_child(caller, &rec.snapshot, "close_session")?;

        if let Some(live) = self.inner.take_live_session(session_id) {
            live.drain_handle.abort();
            let _ = live.driver_session.close().await;
        }

        let (snap, evs) = self
            .inner
            .store
            .close_or_fail_session(session_id, SessionStatus::Closed, Some("closed".to_owned()))
            .await?;

        for event in evs {
            self.inner.emit_live(session_id, event);
        }
        self.inner.wait_notify.notify_waiters();
        Ok(snap)
    }

    /// Gracefully detaches all live driver sessions on daemon shutdown (SIGINT/SIGTERM),
    /// closing driver processes (PGID teardown) while transitioning active sessions
    /// to resumable `Idle` state (bumping `session_generation`, revoking tokens,
    /// expiring pending interactions, and releasing writer leases).
    pub async fn shutdown_all_sessions(&self) {
        let drained: Vec<(String, LiveSession)> = {
            let mut guard = self
                .inner
                .live_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.drain().collect()
        };

        for (sid, live) in drained {
            live.drain_handle.abort();
            let _ = live.driver_session.close().await;
            let _ = self.inner.store.detach_session_on_shutdown(&sid).await;
        }
        self.inner.wait_notify.notify_waiters();
    }

    /// Submits a new turn to an active session, rejecting concurrent active
    /// turns with `409 SNAPSHOT_CONFLICT`.
    pub async fn submit_turn(
        &self,
        caller: &CallerContext,
        session_id: &str,
        req: SubmitTurnRequest,
    ) -> Result<TurnAccepted, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_direct_child(caller, &rec.snapshot, "submit_turn")?;

        let (driver_session, _) = self.inner.get_live_driver(session_id).ok_or_else(|| {
            CoreError::snapshot_conflict("session is not attached to a live driver")
        })?;

        let (inline_text, input_artifact) =
            match self.inner.artifacts.spill_inline_text(req.text.clone())? {
                (inline, Some(art)) => {
                    self.inner
                        .store
                        .record_artifact(art.clone(), Some(rec.snapshot.root_session_id.clone()))
                        .await?;
                    (inline, Some(art))
                }
                other => other,
            };

        let placeholder = format!("pending-{}", Uuid::now_v7());
        self.inner
            .store
            .reserve_active_turn(
                session_id,
                &placeholder,
                inline_text.clone(),
                input_artifact.clone(),
                req.output_schema_override.clone(),
            )
            .await?;

        let ack = match driver_session
            .send_turn(TurnInput {
                text: req.text,
                effort: req.effort,
                attachments: req.attachments,
                output_schema_override: req.output_schema_override.clone(),
            })
            .await
        {
            Ok(ack) => ack,
            Err(err) => {
                let _ = self
                    .inner
                    .store
                    .release_turn_reservation(session_id, &placeholder)
                    .await;
                return Err(err.into());
            }
        };

        let (revision, head_cursor, maybe_ev) = self
            .inner
            .store
            .finalize_turn_start(
                session_id,
                &ack.turn_id,
                Some(&placeholder),
                inline_text,
                input_artifact,
                req.output_schema_override,
            )
            .await?;

        if let Some(ev) = maybe_ev {
            self.inner.emit_live(session_id, ev);
        }
        Ok(TurnAccepted {
            session_id: session_id.to_owned(),
            turn_id: ack.turn_id,
            revision,
            head_cursor,
        })
    }

    /// Requests interruption of the active turn on a live session.
    pub async fn interrupt_turn(
        &self,
        caller: &CallerContext,
        session_id: &str,
        turn_id: &str,
    ) -> Result<(), CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_direct_child(caller, &rec.snapshot, "interrupt_turn")?;
        if rec.snapshot.active_turn_id.as_deref() != Some(turn_id) {
            return Err(CoreError::not_found(format!(
                "active turn '{turn_id}' not found on session '{session_id}'"
            )));
        }
        let (driver_session, _) = self.inner.get_live_driver(session_id).ok_or_else(|| {
            CoreError::snapshot_conflict("session is not attached to a live driver")
        })?;
        driver_session.interrupt_turn(turn_id).await?;
        Ok(())
    }

    /// Returns a bounded (`<= 4 MiB`) timeline page with distinct `highest_cursor`
    /// and opaque `next_page_token`.
    pub async fn get_timeline(
        &self,
        caller: &CallerContext,
        session_id: &str,
        page_token: Option<String>,
        limit: usize,
    ) -> Result<TimelinePage, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        self.inner
            .store
            .list_timeline(session_id, page_token, limit)
            .await
    }

    /// Replays durable events strictly after `after` (or signals `RefreshRequired`
    /// on retention gap / future cursor) and returns a live broadcast receiver
    /// subscribed BEFORE the replay query so no live events are missed.
    pub async fn subscribe_events(
        &self,
        caller: &CallerContext,
        session_id: &str,
        after: Option<Cursor>,
    ) -> Result<(ReplayResult, broadcast::Receiver<SessionEvent>), CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;

        let rx = self.inner.broadcast_sender(session_id).subscribe();
        let replay = match after {
            Some(cursor) => self.inner.store.replay_events(session_id, cursor).await?,
            None => ReplayResult::Events(Vec::new()),
        };
        Ok((replay, rx))
    }

    /// Lists interactions for a session with current monotonic revision stamps.
    pub async fn list_interactions(
        &self,
        caller: &CallerContext,
        session_id: &str,
        status: Option<InteractionStatus>,
    ) -> Result<InteractionList, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        self.inner.store.list_interactions(session_id, status).await
    }

    /// Answers a pending interaction with strict optimistic concurrency checks:
    /// serializes per session, validates read-only BEFORE driver delivery, and
    /// commits durable resolution ONLY AFTER the driver accepts the decision.
    ///
    /// Scoped actors are NEVER authorized to answer self or child approvals.
    pub async fn answer_interaction(
        &self,
        caller: &CallerContext,
        session_id: &str,
        interaction_id: &str,
        req: AnswerInteractionRequest,
    ) -> Result<AnswerInteractionResponse, CoreError> {
        if !matches!(caller, CallerContext::ExternalHost) {
            return Err(CoreError::forbidden_scope(
                "scoped actors cannot answer interactive approval requests",
            ));
        }

        let (driver_session, answer_lock) = self
            .inner
            .get_live_answer_handle(session_id)
            .ok_or_else(|| {
                CoreError::snapshot_conflict("session is not attached to a live driver")
            })?;

        let _serial = answer_lock.lock().await;

        let answer_json = self
            .inner
            .store
            .validate_interaction_answer(session_id, interaction_id, req.clone())
            .await?;

        let spi_answer = InteractionAnswer {
            interaction_id: interaction_id.to_owned(),
            decision_id: req.decision_id.clone(),
            answers: req.answers.clone(),
            payload: req.payload.clone(),
        };

        driver_session.answer_interaction(spi_answer).await?;

        let (resp, maybe_ev) = self
            .inner
            .store
            .commit_interaction_answer(session_id, interaction_id, req, answer_json)
            .await?;

        if let Some(ev) = maybe_ev {
            self.inner.emit_live(session_id, ev);
        }
        Ok(resp)
    }

    /// Delivers a durable mailbox message, enforcing tree isolation, parent-child
    /// vs. peer collaboration rules, and >256 KiB artifact spilling.
    pub async fn post_mailbox_message(
        &self,
        caller: &CallerContext,
        target_session_id: &str,
        req: PostMailboxRequest,
    ) -> Result<MailboxMessage, CoreError> {
        let target = self
            .inner
            .store
            .get_session_record(target_session_id)
            .await?;

        let from_session_id = match caller {
            CallerContext::ExternalHost => req.from_session_id.clone(),
            CallerContext::ScopedActor(scope) => {
                if let Some(ref f) = req.from_session_id
                    && f != &scope.session_id
                {
                    return Err(CoreError::forbidden_scope(
                        "scoped actor cannot spoof from_session_id",
                    ));
                }
                Some(scope.session_id.clone())
            }
        };

        if let Some(ref sender_id) = from_session_id {
            let sender = self.inner.store.get_session_record(sender_id).await?;
            if sender.snapshot.root_session_id != target.snapshot.root_session_id {
                return Err(CoreError::forbidden_scope(
                    "cross-tree mailbox delivery is forbidden",
                ));
            }
            let is_self = sender_id == target_session_id;
            let is_direct_parent_child = target.snapshot.parent_session_id.as_deref()
                == Some(sender_id.as_str())
                || sender.snapshot.parent_session_id.as_deref() == Some(target_session_id);

            if !is_self
                && !is_direct_parent_child
                && !target.snapshot.policy.allow_actor_collaboration
            {
                return Err(CoreError::forbidden_scope(
                    "peer actor mailbox collaboration is disabled by session policy",
                ));
            }
        }

        // Validate any referenced artifact IDs against caller scope.
        // When ExternalHost attaches artifacts to a session mailbox, grant the
        // target session tree read scope so scoped actors can read them.
        let required_root = match caller {
            CallerContext::ExternalHost => None,
            CallerContext::ScopedActor(scope) => Some(scope.root_session_id.as_str()),
        };
        for aid in &req.artifact_ids {
            let _ = self
                .inner
                .store
                .get_artifact_meta(aid, required_root)
                .await?;
            if matches!(caller, CallerContext::ExternalHost) {
                self.inner
                    .store
                    .grant_artifact_scope(aid, &target.snapshot.root_session_id)
                    .await?;
            }
        }

        let (inline_body, artifact) = self.inner.artifacts.spill_inline_text(req.body)?;
        if let Some(ref art) = artifact {
            self.inner
                .store
                .record_artifact(art.clone(), Some(target.snapshot.root_session_id.clone()))
                .await?;
        }

        let (msg, ev) = self
            .inner
            .store
            .post_mailbox_message(NewMailboxRow {
                message_id: format!("msg-{}", Uuid::now_v7()),
                session_id: target_session_id.to_owned(),
                from_session_id,
                subject: req.subject,
                inline_body,
                artifact,
                artifact_ids: req.artifact_ids,
            })
            .await?;

        self.inner.emit_live(target_session_id, ev);
        self.inner.wait_notify.notify_waiters();
        Ok(msg)
    }

    /// Lists durable mailbox messages for a session with bounded pagination.
    pub async fn list_mailbox_messages(
        &self,
        caller: &CallerContext,
        session_id: &str,
        page_token: Option<String>,
        limit: usize,
    ) -> Result<MailboxPage, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        self.inner
            .store
            .list_mailbox_messages(session_id, page_token, limit)
            .await
    }

    /// Stores a streaming artifact (<= 64 MiB) and records its tree scope.
    pub async fn create_artifact<R: Read + Send + 'static>(
        &self,
        caller: &CallerContext,
        media_type: String,
        reader: R,
    ) -> Result<ArtifactRef, CoreError> {
        let artifacts = self.inner.artifacts.clone();
        let art = tokio::task::spawn_blocking(move || artifacts.put_reader(&media_type, reader))
            .await
            .map_err(|e| CoreError::internal(format!("artifact write task failed: {e}")))??;

        let root_scope = match caller {
            CallerContext::ExternalHost => None,
            CallerContext::ScopedActor(scope) => Some(scope.root_session_id.clone()),
        };
        self.inner
            .store
            .record_artifact(art.clone(), root_scope)
            .await?;
        Ok(art)
    }

    /// Convenience wrapper storing a byte vector as an immutable artifact.
    pub async fn create_artifact_bytes(
        &self,
        caller: &CallerContext,
        media_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, CoreError> {
        self.create_artifact(caller, media_type.into(), std::io::Cursor::new(bytes))
            .await
    }

    /// Reads a bounded byte slice (<= 4 MiB) of an artifact, enforcing cross-tree
    /// isolation for scoped actors.
    pub async fn read_artifact(
        &self,
        caller: &CallerContext,
        artifact_id: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<(ArtifactRef, Vec<u8>), CoreError> {
        let required_root = match caller {
            CallerContext::ExternalHost => None,
            CallerContext::ScopedActor(scope) => Some(scope.root_session_id.as_str()),
        };
        let meta = self
            .inner
            .store
            .get_artifact_meta(artifact_id, required_root)
            .await?;
        let artifacts = self.inner.artifacts.clone();
        let sha = meta.artifact_ref.sha256.clone();
        let bytes =
            tokio::task::spawn_blocking(move || artifacts.read_slice(&sha, offset, max_bytes))
                .await
                .map_err(|e| CoreError::internal(format!("artifact read task failed: {e}")))??;
        Ok((meta.artifact_ref, bytes))
    }

    /// Waits for `Any` or `All` target sessions to settle (complete a turn or
    /// close/fail), detecting cyclic wait graphs (`409 DELEGATION_CYCLE`) before
    /// blocking and cleaning up graph edges via RAII on completion or timeout.
    pub async fn wait_sessions(
        &self,
        caller: &CallerContext,
        req: WaitSessionsRequest,
    ) -> Result<WaitSessionsResponse, CoreError> {
        if req.session_ids.is_empty() {
            return Err(CoreError::bad_request("session_ids must not be empty"));
        }
        if req.timeout_ms == 0 || req.timeout_ms > 300_000 {
            return Err(CoreError::bad_request(
                "timeout_ms must be between 1 and 300000",
            ));
        }

        let waiter_id = match caller {
            CallerContext::ExternalHost => req.waiter_session_id.clone(),
            CallerContext::ScopedActor(scope) => {
                if let Some(ref w) = req.waiter_session_id
                    && w != &scope.session_id
                {
                    return Err(CoreError::forbidden_scope(
                        "scoped actor cannot specify another waiter_session_id",
                    ));
                }
                Some(scope.session_id.clone())
            }
        };

        let waiter_and_baseline = match &waiter_id {
            Some(w) => {
                let baseline = self.inner.store.get_waiter_mailbox_cursor(w).await?;
                Some((w.clone(), baseline))
            }
            None => None,
        };

        let initial_states = self
            .inner
            .store
            .get_wait_target_states(req.session_ids.clone(), waiter_and_baseline.clone())
            .await?;
        for state in &initial_states {
            authorize_same_tree(caller, &state.snapshot)?;
        }

        let _wait_guard = match waiter_id {
            Some(waiter) => Some(
                self.inner
                    .wait_graph
                    .register(waiter, req.session_ids.clone())?,
            ),
            None => None,
        };

        let deadline = tokio::time::Instant::now() + Duration::from_millis(req.timeout_ms);
        loop {
            let notified = self.inner.wait_notify.notified();
            tokio::pin!(notified);

            let states = self
                .inner
                .store
                .get_wait_target_states(req.session_ids.clone(), waiter_and_baseline.clone())
                .await?;
            let completed_ids: Vec<String> = states
                .iter()
                .filter(|st| is_target_settled(st))
                .map(|st| st.snapshot.session_id.clone())
                .collect();

            let done = match req.mode {
                WaitMode::Any => !completed_ids.is_empty(),
                WaitMode::All => completed_ids.len() == states.len(),
            };
            if done {
                return Ok(WaitSessionsResponse {
                    completed_session_ids: completed_ids,
                    timed_out: false,
                    sessions: states.into_iter().map(|s| s.snapshot).collect(),
                });
            }

            if tokio::time::Instant::now() >= deadline {
                return Ok(WaitSessionsResponse {
                    completed_session_ids: completed_ids,
                    timed_out: true,
                    sessions: states.into_iter().map(|s| s.snapshot).collect(),
                });
            }

            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => {
                    let final_states = self
                        .inner
                        .store
                        .get_wait_target_states(req.session_ids.clone(), waiter_and_baseline.clone())
                        .await?;
                    let final_completed: Vec<String> = final_states
                        .iter()
                        .filter(|st| is_target_settled(st))
                        .map(|st| st.snapshot.session_id.clone())
                        .collect();
                    let final_done = match req.mode {
                        WaitMode::Any => !final_completed.is_empty(),
                        WaitMode::All => final_completed.len() == final_states.len(),
                    };
                    return Ok(WaitSessionsResponse {
                        completed_session_ids: final_completed,
                        timed_out: !final_done,
                        sessions: final_states.into_iter().map(|s| s.snapshot).collect(),
                    });
                }
            }
        }
    }

    /// Acquires an optional single-writer lease on a canonical workspace path.
    pub async fn acquire_writer_lease(
        &self,
        caller: &CallerContext,
        workspace: &Path,
        session_id: &str,
    ) -> Result<WriterLease, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        let canon = canonicalize_workspace(workspace)?;
        authorize_lease_workspace(caller, &canon, &rec.snapshot)?;
        if rec.snapshot.access != AccessMode::WorkspaceWrite {
            return Err(CoreError::forbidden_scope(
                "read_only session cannot hold a workspace writer lease",
            ));
        }
        if let CallerContext::ScopedActor(scope) = caller
            && scope.access == AccessMode::ReadOnly
        {
            return Err(CoreError::forbidden_scope(
                "read_only scoped actor cannot acquire a workspace writer lease",
            ));
        }
        let ws_str = path_to_string(&canon)?;
        self.inner
            .store
            .acquire_writer_lease(&ws_str, session_id)
            .await
    }

    /// Atomically transfers an active workspace single-writer lease between two sessions.
    pub async fn transfer_writer_lease(
        &self,
        caller: &CallerContext,
        workspace: &Path,
        from_session_id: &str,
        to_session_id: &str,
    ) -> Result<WriterLease, CoreError> {
        let from_rec = self.inner.store.get_session_record(from_session_id).await?;
        let to_rec = self.inner.store.get_session_record(to_session_id).await?;
        authorize_self_or_direct_child(caller, &from_rec.snapshot)?;
        authorize_self_or_direct_child(caller, &to_rec.snapshot)?;
        let canon = canonicalize_workspace(workspace)?;
        authorize_lease_workspace(caller, &canon, &from_rec.snapshot)?;
        authorize_lease_workspace(caller, &canon, &to_rec.snapshot)?;
        if to_rec.snapshot.access != AccessMode::WorkspaceWrite {
            return Err(CoreError::forbidden_scope(
                "cannot transfer writer lease to a read_only target session",
            ));
        }
        let ws_str = path_to_string(&canon)?;
        self.inner
            .store
            .transfer_writer_lease(&ws_str, from_session_id, to_session_id)
            .await
    }

    /// Releases a workspace single-writer lease held by `session_id`.
    pub async fn release_writer_lease(
        &self,
        caller: &CallerContext,
        workspace: &Path,
        session_id: &str,
    ) -> Result<bool, CoreError> {
        let rec = self.inner.store.get_session_record(session_id).await?;
        authorize_self_or_direct_child(caller, &rec.snapshot)?;
        let canon = canonicalize_workspace(workspace)?;
        authorize_lease_workspace(caller, &canon, &rec.snapshot)?;
        let ws_str = path_to_string(&canon)?;
        self.inner
            .store
            .release_writer_lease(&ws_str, session_id)
            .await
    }
}

const MAX_CONSECUTIVE_STORE_ERRORS: u32 = 3;

async fn drain_driver_events(
    weak: Weak<EngineInner>,
    session_id: String,
    root_session_id: String,
    generation: u64,
    native_output_schema: bool,
    mut rx: EventReceiver,
) {
    let mut explicitly_closed = false;
    let mut consecutive_store_errors: u32 = 0;

    while let Some(event) = rx.recv().await {
        let Some(inner) = weak.upgrade() else {
            return;
        };

        let mut store_failed = false;

        match event {
            NormalizedEvent::TurnStarted { turn_id } => {
                match inner
                    .store
                    .finalize_turn_start(&session_id, &turn_id, None, None, None, None)
                    .await
                {
                    Ok((_, _, maybe_ev)) => {
                        consecutive_store_errors = 0;
                        if let Some(ev) = maybe_ev {
                            inner.emit_live(&session_id, ev);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on turn_started");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::TextDelta {
                turn_id,
                item_id,
                delta,
            } => {
                // Ephemeral live-only event: no SQLite write, no revision advance.
                inner.emit_live(
                    &session_id,
                    SessionEvent {
                        cursor: None,
                        event_type: "text_delta".to_owned(),
                        turn_id: Some(turn_id),
                        created_at: now_rfc3339(),
                        payload: json!({
                            "item_id": item_id,
                            "delta": delta,
                        }),
                    },
                );
            }
            NormalizedEvent::ToolActivity {
                turn_id,
                item_id,
                tool_name,
                status,
                summary,
                command,
                exit_status,
                paths,
            } => {
                if status == ToolActivityStatus::Delta {
                    // Ephemeral live-only streaming tool output.
                    inner.emit_live(
                        &session_id,
                        SessionEvent {
                            cursor: None,
                            event_type: "tool_delta".to_owned(),
                            turn_id: Some(turn_id),
                            created_at: now_rfc3339(),
                            payload: json!({
                                "item_id": item_id,
                                "tool_name": tool_name,
                                "summary": summary,
                            }),
                        },
                    );
                } else {
                    let (inline_summary, artifact) = match summary {
                        Some(s) => match inner.artifacts.spill_inline_text(s) {
                            Ok((inline, Some(art))) => {
                                if let Err(err) = inner
                                    .store
                                    .record_artifact(art.clone(), Some(root_session_id.clone()))
                                    .await
                                {
                                    tracing::warn!(session_id = %session_id, %err, "failed to record tool artifact");
                                }
                                (inline, Some(art))
                            }
                            Ok(pair) => pair,
                            Err(err) => {
                                tracing::warn!(session_id = %session_id, %err, "failed to spill tool summary");
                                (None, None)
                            }
                        },
                        None => (None, None),
                    };
                    match inner
                        .store
                        .append_durable_event(NewDurableEvent {
                            session_id: session_id.clone(),
                            event_type: "tool_activity".to_owned(),
                            turn_id: Some(turn_id),
                            inline_text: inline_summary.clone(),
                            payload: json!({
                                "item_id": item_id,
                                "tool_name": tool_name,
                                "status": status,
                                "summary": inline_summary,
                                "command": command,
                                "exit_status": exit_status,
                                "paths": paths,
                            }),
                            artifact,
                            bump_interaction_revision: false,
                        })
                        .await
                    {
                        Ok(ev) => {
                            consecutive_store_errors = 0;
                            inner.emit_live(&session_id, ev);
                        }
                        Err(err) => {
                            tracing::warn!(session_id = %session_id, %err, "store error on tool_activity");
                            store_failed = true;
                        }
                    }
                }
            }
            NormalizedEvent::AssistantMessage {
                turn_id,
                item_id,
                text,
                mut structured_output,
            } => {
                let mut schema_validation: Option<SchemaValidationResult> = None;
                if !native_output_schema {
                    match inner
                        .store
                        .get_turn_output_schema(&session_id, &turn_id)
                        .await
                    {
                        Ok(Some(schema)) => {
                            let candidate = structured_output
                                .clone()
                                .or_else(|| serde_json::from_str::<Value>(&text).ok());
                            match candidate {
                                Some(val) => {
                                    let result = validate_json_schema(&schema, &val);
                                    structured_output = Some(val);
                                    schema_validation = Some(result);
                                }
                                None => {
                                    schema_validation = Some(SchemaValidationResult {
                                        validated: false,
                                        errors: vec![
                                            "assistant message is not valid JSON for requested output_schema"
                                                .to_owned(),
                                        ],
                                    });
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(session_id = %session_id, %err, "failed to load turn output_schema");
                        }
                    }
                }

                let (inline_text, artifact) = match inner.artifacts.spill_inline_text(text) {
                    Ok((inline, Some(art))) => {
                        if let Err(err) = inner
                            .store
                            .record_artifact(art.clone(), Some(root_session_id.clone()))
                            .await
                        {
                            tracing::warn!(session_id = %session_id, %err, "failed to record assistant artifact");
                        }
                        (inline, Some(art))
                    }
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "failed to spill assistant message");
                        (None, None)
                    }
                };

                let mut payload = json!({
                    "item_id": item_id,
                    "structured_output": structured_output,
                });
                if let Some(val_res) = schema_validation {
                    payload["schema_validation"] = json!(val_res);
                }

                match inner
                    .store
                    .append_durable_event(NewDurableEvent {
                        session_id: session_id.clone(),
                        event_type: "assistant_message".to_owned(),
                        turn_id: Some(turn_id),
                        inline_text,
                        payload,
                        artifact,
                        bump_interaction_revision: false,
                    })
                    .await
                {
                    Ok(ev) => {
                        consecutive_store_errors = 0;
                        inner.emit_live(&session_id, ev);
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on assistant_message");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::InteractionRequested { request } => {
                match inner
                    .store
                    .record_interaction_requested(&session_id, request)
                    .await
                {
                    Ok(Some((_, ev))) => {
                        consecutive_store_errors = 0;
                        inner.emit_live(&session_id, ev);
                    }
                    Ok(None) => {
                        consecutive_store_errors = 0;
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on interaction_requested");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::InteractionResolved {
                interaction_id,
                turn_id,
                decision_id,
            } => {
                match inner
                    .store
                    .resolve_interaction_from_driver(
                        &session_id,
                        &interaction_id,
                        &turn_id,
                        decision_id,
                    )
                    .await
                {
                    Ok(maybe_ev) => {
                        consecutive_store_errors = 0;
                        if let Some(ev) = maybe_ev {
                            inner.emit_live(&session_id, ev);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on interaction_resolved");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::Usage { turn_id, metrics } => {
                match inner
                    .store
                    .append_durable_event(NewDurableEvent {
                        session_id: session_id.clone(),
                        event_type: "usage".to_owned(),
                        turn_id,
                        inline_text: None,
                        payload: json!({ "metrics": metrics }),
                        artifact: None,
                        bump_interaction_revision: false,
                    })
                    .await
                {
                    Ok(ev) => {
                        consecutive_store_errors = 0;
                        inner.emit_live(&session_id, ev);
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on usage");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::TurnCompleted {
                turn_id,
                status,
                error,
            } => {
                match inner
                    .store
                    .complete_turn(&session_id, &turn_id, status, error)
                    .await
                {
                    Ok(ev) => {
                        consecutive_store_errors = 0;
                        inner.emit_live(&session_id, ev);
                        inner.wait_notify.notify_waiters();
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on turn_completed");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::Error {
                turn_id,
                message,
                will_retry,
            } => {
                match inner
                    .store
                    .append_durable_event(NewDurableEvent {
                        session_id: session_id.clone(),
                        event_type: "error".to_owned(),
                        turn_id,
                        inline_text: Some(message.clone()),
                        payload: json!({
                            "message": message,
                            "will_retry": will_retry,
                        }),
                        artifact: None,
                        bump_interaction_revision: false,
                    })
                    .await
                {
                    Ok(ev) => {
                        consecutive_store_errors = 0;
                        inner.emit_live(&session_id, ev);
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on error event");
                        store_failed = true;
                    }
                }
            }
            NormalizedEvent::SessionClosed { reason } => {
                explicitly_closed = true;
                inner.remove_live_if_generation(&session_id, generation);
                match inner
                    .store
                    .close_or_fail_session(&session_id, SessionStatus::Failed, reason)
                    .await
                {
                    Ok((_, evs)) => {
                        for ev in evs {
                            inner.emit_live(&session_id, ev);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(session_id = %session_id, %err, "store error on session_closed");
                    }
                }
                inner.wait_notify.notify_waiters();
                break;
            }
            NormalizedEvent::Unknown {
                event_type,
                payload,
            } => {
                inner.emit_live(
                    &session_id,
                    SessionEvent {
                        cursor: None,
                        event_type: "unknown".to_owned(),
                        turn_id: None,
                        created_at: now_rfc3339(),
                        payload: json!({
                            "original_event_type": event_type,
                            "payload": payload,
                        }),
                    },
                );
            }
        }

        if store_failed {
            consecutive_store_errors += 1;
            if consecutive_store_errors >= MAX_CONSECUTIVE_STORE_ERRORS {
                tracing::error!(
                    session_id = %session_id,
                    consecutive_store_errors,
                    "failing session after repeated durable store errors"
                );
                explicitly_closed = true;
                inner.remove_live_if_generation(&session_id, generation);
                let _ = inner
                    .store
                    .close_or_fail_session(
                        &session_id,
                        SessionStatus::Failed,
                        Some("repeated durable store failure".to_owned()),
                    )
                    .await;
                inner.wait_notify.notify_waiters();
                break;
            }
        }
    }

    if !explicitly_closed && let Some(inner) = weak.upgrade() {
        let still_current = inner
            .get_live_driver(&session_id)
            .is_some_and(|(_, g)| g == generation);
        if still_current {
            inner.remove_live_if_generation(&session_id, generation);
            match inner
                .store
                .close_or_fail_session(
                    &session_id,
                    SessionStatus::Failed,
                    Some("driver event channel closed".to_owned()),
                )
                .await
            {
                Ok((_, evs)) => {
                    for ev in evs {
                        inner.emit_live(&session_id, ev);
                    }
                }
                Err(err) => {
                    tracing::warn!(session_id = %session_id, %err, "store error closing dropped driver session");
                }
            }
            inner.wait_notify.notify_waiters();
        }
    }
}

fn authorize_direct_child(
    caller: &CallerContext,
    target: &SessionSnapshot,
    operation: &str,
) -> Result<(), CoreError> {
    match caller {
        CallerContext::ExternalHost => Ok(()),
        CallerContext::ScopedActor(scope) => {
            if target.parent_session_id.as_deref() == Some(scope.session_id.as_str()) {
                Ok(())
            } else {
                Err(CoreError::forbidden_scope(format!(
                    "scoped actor '{}' may only invoke {operation} on its direct children, not '{}'",
                    scope.session_id, target.session_id
                )))
            }
        }
    }
}

fn authorize_self_or_direct_child(
    caller: &CallerContext,
    target: &SessionSnapshot,
) -> Result<(), CoreError> {
    match caller {
        CallerContext::ExternalHost => Ok(()),
        CallerContext::ScopedActor(scope) => {
            if target.session_id == scope.session_id
                || target.parent_session_id.as_deref() == Some(scope.session_id.as_str())
            {
                Ok(())
            } else {
                Err(CoreError::forbidden_scope(format!(
                    "scoped actor '{}' may only inspect itself or its direct children, not '{}'",
                    scope.session_id, target.session_id
                )))
            }
        }
    }
}

fn authorize_same_tree(caller: &CallerContext, target: &SessionSnapshot) -> Result<(), CoreError> {
    match caller {
        CallerContext::ExternalHost => Ok(()),
        CallerContext::ScopedActor(scope) => {
            if target.root_session_id == scope.root_session_id {
                Ok(())
            } else {
                Err(CoreError::forbidden_scope(format!(
                    "session '{}' is outside actor root session tree",
                    target.session_id
                )))
            }
        }
    }
}

fn check_approval_policy_escalation(
    parent_policy: ApprovalPolicy,
    requested: ApprovalPolicy,
) -> Result<(), CoreError> {
    if matches!(
        parent_policy,
        ApprovalPolicy::Interactive | ApprovalPolicy::AutoDeny
    ) && requested == ApprovalPolicy::AutoApproveScoped
    {
        return Err(CoreError::forbidden_scope(
            "child session cannot escalate approval_policy to auto_approve_scoped",
        ));
    }
    Ok(())
}

fn authorize_lease_workspace(
    caller: &CallerContext,
    canon_workspace: &Path,
    session: &SessionSnapshot,
) -> Result<(), CoreError> {
    let session_ws = Path::new(&session.workspace);
    if !canon_workspace.starts_with(session_ws) {
        return Err(CoreError::forbidden_scope(format!(
            "lease workspace '{}' is outside session '{}' workspace '{}'",
            canon_workspace.display(),
            session.session_id,
            session_ws.display()
        )));
    }
    if let CallerContext::ScopedActor(scope) = caller
        && !canon_workspace.starts_with(&scope.workspace)
    {
        return Err(CoreError::forbidden_scope(format!(
            "lease workspace '{}' is outside scoped actor workspace '{}'",
            canon_workspace.display(),
            scope.workspace.display()
        )));
    }
    Ok(())
}

fn is_target_settled(state: &crate::store::WaitTargetState) -> bool {
    if state.has_new_mailbox_for_waiter {
        return true;
    }
    match state.snapshot.status {
        SessionStatus::Closed | SessionStatus::Failed | SessionStatus::Idle => true,
        SessionStatus::Active => {
            state.snapshot.active_turn_id.is_none() && state.completed_turn_count > 0
        }
    }
}

fn canonicalize_workspace(path: &Path) -> Result<PathBuf, CoreError> {
    if !path.exists() {
        return Err(CoreError::bad_request(format!(
            "workspace '{}' does not exist",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(CoreError::bad_request(format!(
            "workspace '{}' is not a directory",
            path.display()
        )));
    }
    fs::canonicalize(path)
        .map_err(|e| CoreError::bad_request(format!("failed to canonicalize workspace: {e}")))
}

fn path_to_string(path: &Path) -> Result<String, CoreError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| CoreError::bad_request("workspace path must be valid UTF-8"))
}

fn build_scoped_mcp_injection(
    bin_path: &Path,
    socket_path: Option<&Path>,
    session_id: &str,
    root_session_id: &str,
    raw_token: &Zeroizing<String>,
) -> McpServerInjection {
    let mut env = BTreeMap::new();
    env.insert("DOLGORAE_ACTOR_TOKEN".to_owned(), raw_token.to_string());
    env.insert("DOLGORAE_SESSION_ID".to_owned(), session_id.to_owned());
    env.insert(
        "DOLGORAE_ROOT_SESSION_ID".to_owned(),
        root_session_id.to_owned(),
    );
    let mut args = vec!["mcp".to_owned(), "--scoped".to_owned()];
    if let Some(sock) = socket_path
        && let Some(s) = sock.to_str()
    {
        env.insert("DOLGORAE_SOCKET".to_owned(), s.to_owned());
        args.push("--socket".to_owned());
        args.push(s.to_owned());
    }
    McpServerInjection {
        name: "dolgorae".to_owned(),
        command: bin_path.to_path_buf(),
        args,
        env,
    }
}
