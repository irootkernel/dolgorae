//! Tier-1 structured driver for Codex app-server (`codex app-server --stdio`).
//!
//! - **Zero secrets in `argv`**: `mcp_servers` (including `DOLGORAE_ACTOR_TOKEN`)
//!   are passed exclusively via stdin `thread/start` / `thread/resume` `config.mcp_servers`.
//! - **OS sandbox + policy**: Native approval policy is never `"never"`. Under
//!   Codex `on-request`, command approval prompts request sandbox escape or network
//!   access (`os_sandboxed = false`). Under `AccessMode::ReadOnly`, filesystem write
//!   permission escalations are hard-denied regardless of `ApprovalPolicy`.
//! - **Deadlock-free & zombie-free PGID teardown (zero `unsafe`)**: Child runs in
//!   `process_group(0)`. `close()` never blocks on stdin before signaling PGID.
//!   `Drop` offloads reaping and awaits `/bin/kill` via `.status()` (zero zombies).
//!   Grandchildren calling `setsid(2)` escape PGID signaling.

mod wire;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, AutoDecision, DriverCapabilities, DriverError,
    DriverSession, EventReceiver, EventSender, FileChangeEntry, InteractionAnswer, InteractionKind,
    NormalizedEvent, ResumeSpec, SessionSpec, TurnAck, TurnInput, evaluate_interaction,
    event_channel,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

use crate::wire::{
    IncomingFrame, NativePendingKind, PendingInteraction, build_reply, build_thread_params,
    build_turn_start_params, extract_changes, id_key, map_notification, map_server_request,
    parse_incoming_line, requests_filesystem_write, s,
};

/// Stable canonical driver identifier for Codex app-server.
pub const DRIVER_ID: &str = "codex-app";

const EVENT_CAP: usize = 256;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// Driver implementation for `codex app-server --stdio`.
#[derive(Debug, Clone)]
pub struct CodexAppDriver {
    binary: PathBuf,
}

/// Convenience alias for `CodexAppDriver`.
pub type CodexDriver = CodexAppDriver;

impl Default for CodexAppDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAppDriver {
    pub fn new() -> Self {
        let binary = std::env::var_os("DOLGORAE_CODEX_BIN")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("codex"));
        Self { binary }
    }

    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary }
    }

    async fn launch(
        &self,
        resume_id: Option<String>,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        let mut child = Command::new(&self.binary)
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| {
                DriverError::Unavailable(format!("spawn '{}': {e}", self.binary.display()))
            })?;

        let pgid = child.id();
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(DriverError::Protocol("missing child stdio".into()));
        };
        let stderr = child.stderr.take();
        let (event_tx, event_rx) = event_channel(EVENT_CAP);
        let shared = Arc::new(SharedState {
            stdin: Mutex::new(Some(stdin)),
            next_rpc_id: AtomicU64::new(1),
            pending_rpc: Mutex::new(HashMap::new()),
            state: Mutex::new(SessionState::default()),
            expect_schema: AtomicBool::new(spec.output_schema.is_some()),
        });

        let mut tasks = Vec::new();
        if let Some(pipe) = stderr {
            let st = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let t = line.trim();
                    if !t.is_empty() {
                        tracing::debug!(driver = DRIVER_ID, stderr = t, "codex stderr");
                        st.state.lock().await.last_stderr = Some(t.to_owned());
                    }
                }
            }));
        }

        let ctx = ReaderContext {
            shared: Arc::clone(&shared),
            event_tx: event_tx.clone(),
            workspace: spec.workspace.clone(),
            access: spec.access,
            policy: spec.approval_policy,
        };
        tasks.push(tokio::spawn(run_stdout_reader(stdout, ctx)));

        let handshake: Result<String, DriverError> = async {
            let init = json!({
                "clientInfo": { "name": "dolgorae", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true },
            });
            let _ = send_rpc(&shared, "initialize", init).await?;
            write_line(&shared, &json!({ "method": "initialized" })).await?;
            let method = if resume_id.is_some() {
                "thread/resume"
            } else {
                "thread/start"
            };
            let res = send_rpc(
                &shared,
                method,
                build_thread_params(resume_id.as_deref(), &spec),
            )
            .await?;
            res.get("thread")
                .and_then(|t| s(t, &["id"]))
                .or_else(|| s(&res, &["threadId"]))
                .map(str::to_owned)
                .or(resume_id)
                .ok_or_else(|| DriverError::Protocol("thread response missing id".into()))
        }
        .await;

        let provider_session_id = match handshake {
            Ok(id) => id,
            Err(err) => {
                terminate_pgid(pgid, Some(child)).await;
                for t in tasks {
                    t.abort();
                }
                return Err(err);
            }
        };

        Ok(Box::new(CodexSession {
            provider_session_id,
            session_effort: spec.effort,
            session_schema: spec.output_schema,
            shared,
            event_tx,
            event_rx: Some(event_rx),
            pgid,
            child: std::sync::Mutex::new(Some(child)),
            closed: AtomicBool::new(false),
            tasks: Mutex::new(tasks),
        }))
    }
}

#[async_trait]
impl AgentDriver for CodexAppDriver {
    fn driver_id(&self) -> &'static str {
        DRIVER_ID
    }

    async fn probe(&self) -> Result<DriverCapabilities, DriverError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (available, cli_version, unavailable_reason) =
            match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
                Ok(Ok(out)) if out.status.success() => {
                    let so = String::from_utf8_lossy(&out.stdout);
                    let se = String::from_utf8_lossy(&out.stderr);
                    let v = if so.trim().is_empty() {
                        se.trim()
                    } else {
                        so.trim()
                    };
                    (true, (!v.is_empty()).then(|| v.to_owned()), None)
                }
                Ok(Ok(out)) => {
                    let e = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    let r = if e.is_empty() {
                        format!("exit {}", out.status)
                    } else {
                        e
                    };
                    (false, None, Some(r))
                }
                Ok(Err(e)) => (
                    false,
                    None,
                    Some(format!("exec '{}': {e}", self.binary.display())),
                ),
                Err(_) => (false, None, Some("probe timed out".into())),
            };
        Ok(DriverCapabilities {
            driver_id: DRIVER_ID.into(),
            cli_version,
            available,
            unavailable_reason,
            models: Vec::new(),
            supports_effort: true,
            supports_interactive_approval: true,
            native_output_schema: true,
            supports_mcp_injection: true,
            supports_resume: true,
        })
    }

    async fn start_session(
        &self,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        self.launch(None, spec).await
    }

    async fn resume_session(&self, r: ResumeSpec) -> Result<Box<dyn DriverSession>, DriverError> {
        let spec = SessionSpec {
            workspace: r.workspace,
            role_instructions: r.role_instructions,
            model: r.model,
            effort: r.effort,
            access: r.access,
            approval_policy: r.approval_policy,
            output_schema: r.output_schema,
            mcp_servers: r.mcp_servers,
        };
        self.launch(Some(r.provider_session_id), spec).await
    }
}

#[derive(Default)]
struct SessionState {
    pending: HashMap<String, PendingInteraction>,
    cached_changes: HashMap<String, Vec<FileChangeEntry>>,
    active_turn: Option<String>,
    last_stderr: Option<String>,
}

struct SharedState {
    stdin: Mutex<Option<ChildStdin>>,
    next_rpc_id: AtomicU64,
    pending_rpc: Mutex<HashMap<String, oneshot::Sender<Result<Value, Value>>>>,
    state: Mutex<SessionState>,
    expect_schema: AtomicBool,
}

struct ReaderContext {
    shared: Arc<SharedState>,
    event_tx: EventSender,
    workspace: PathBuf,
    access: AccessMode,
    policy: ApprovalPolicy,
}

/// Active `codex app-server --stdio` session.
pub struct CodexSession {
    provider_session_id: String,
    session_effort: Option<String>,
    session_schema: Option<Value>,
    shared: Arc<SharedState>,
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
    pgid: Option<u32>,
    child: std::sync::Mutex<Option<Child>>,
    closed: AtomicBool,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

#[async_trait]
impl DriverSession for CodexSession {
    fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    fn take_events(&mut self) -> Option<EventReceiver> {
        self.event_rx.take()
    }

    async fn send_turn(&self, input: TurnInput) -> Result<TurnAck, DriverError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DriverError::ProcessExited("codex session closed".into()));
        }
        let effort = input.effort.as_deref().or(self.session_effort.as_deref());
        let schema = input
            .output_schema_override
            .as_ref()
            .or(self.session_schema.as_ref());
        self.shared
            .expect_schema
            .store(schema.is_some(), Ordering::SeqCst);

        let params = build_turn_start_params(&self.provider_session_id, &input, effort, schema);
        let res = send_rpc(&self.shared, "turn/start", params).await?;
        let turn_id = res
            .get("turn")
            .and_then(|t| s(t, &["id"]))
            .or_else(|| s(&res, &["turnId"]))
            .map(str::to_owned)
            .ok_or_else(|| DriverError::Protocol("turn/start missing turn id".into()))?;
        self.shared.state.lock().await.active_turn = Some(turn_id.clone());
        Ok(TurnAck { turn_id })
    }

    async fn interrupt_turn(&self, turn_id: &str) -> Result<(), DriverError> {
        let p = json!({ "threadId": self.provider_session_id, "turnId": turn_id });
        let _ = send_rpc(&self.shared, "turn/interrupt", p).await?;
        Ok(())
    }

    async fn answer_interaction(&self, a: InteractionAnswer) -> Result<(), DriverError> {
        let pending = {
            let mut st = self.shared.state.lock().await;
            let Some(e) = st.pending.get(&a.interaction_id) else {
                return Err(DriverError::InvalidConfig(format!(
                    "interaction '{}' not pending",
                    a.interaction_id
                )));
            };
            if !e.allowed.contains(&a.decision_id) {
                return Err(DriverError::InvalidConfig(format!(
                    "invalid decision '{}'",
                    a.decision_id
                )));
            }
            st.pending.remove(&a.interaction_id).expect("checked")
        };
        let reply = build_reply(&pending, &a.decision_id, &a.answers, a.payload.as_ref());
        write_line(&self.shared, &reply).await?;
        let ev = NormalizedEvent::InteractionResolved {
            interaction_id: a.interaction_id,
            turn_id: pending.turn_id,
            decision_id: Some(a.decision_id),
        };
        let _ = self.event_tx.send(ev).await;
        Ok(())
    }

    async fn close(&self) -> Result<(), DriverError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Never block on stdin mutex before signaling PGID.
        if let Ok(mut g) = self.shared.stdin.try_lock() {
            g.take();
        }
        let child = self.child.lock().ok().and_then(|mut g| g.take());
        terminate_pgid(self.pgid, child).await;
        for h in self.tasks.lock().await.drain(..) {
            let ah = h.abort_handle();
            if tokio::time::timeout(Duration::from_millis(200), h)
                .await
                .is_err()
            {
                ah.abort();
            }
        }
        Ok(())
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let pgid = self.pgid;
        let child = self.child.lock().ok().and_then(|mut g| g.take());
        let _ = std::thread::Builder::new()
            .name("codex-reaper".into())
            .spawn(move || reap_pgid_blocking(pgid, child));
    }
}

async fn write_line(shared: &SharedState, frame: &Value) -> Result<(), DriverError> {
    let mut buf = serde_json::to_string(frame)
        .map_err(|e| DriverError::Protocol(format!("json encode: {e}")))?;
    buf.push('\n');
    let mut g = shared.stdin.lock().await;
    let stdin = g
        .as_mut()
        .ok_or_else(|| DriverError::ProcessExited("codex stdin closed".into()))?;
    stdin.write_all(buf.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn send_rpc(shared: &SharedState, method: &str, params: Value) -> Result<Value, DriverError> {
    let id = shared.next_rpc_id.fetch_add(1, Ordering::SeqCst);
    let key = id.to_string();
    let (tx, rx) = oneshot::channel();
    shared.pending_rpc.lock().await.insert(key.clone(), tx);
    if let Err(err) = write_line(
        shared,
        &json!({ "id": id, "method": method, "params": params }),
    )
    .await
    {
        shared.pending_rpc.lock().await.remove(&key);
        return Err(err);
    }
    match tokio::time::timeout(RPC_TIMEOUT, rx).await {
        Ok(Ok(Ok(val))) => Ok(val),
        Ok(Ok(Err(e))) => Err(DriverError::Protocol(format!("rpc '{method}': {e}"))),
        Ok(Err(_)) => Err(DriverError::ProcessExited(format!(
            "exited awaiting '{method}'"
        ))),
        Err(_) => {
            shared.pending_rpc.lock().await.remove(&key);
            Err(DriverError::Timeout(format!("timeout awaiting '{method}'")))
        }
    }
}

async fn run_stdout_reader(stdout: tokio::process::ChildStdout, ctx: ReaderContext) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(frame) = parse_incoming_line(t) else {
            tracing::warn!(driver = DRIVER_ID, "malformed json line");
            continue;
        };
        match frame {
            IncomingFrame::Response(id, result) => {
                if let Some(w) = ctx.shared.pending_rpc.lock().await.remove(&id) {
                    let _ = w.send(result);
                }
            }
            IncomingFrame::ServerRequest(raw_id, key, method, params) => {
                handle_server_request(&ctx, raw_id, key, &method, &params).await;
            }
            IncomingFrame::Notification(method, params) => {
                handle_notification(&ctx, &method, &params).await;
            }
            IncomingFrame::Unrecognized(val) => {
                let ev = NormalizedEvent::Unknown {
                    event_type: "unrecognized_frame".into(),
                    payload: Some(val),
                };
                let _ = ctx.event_tx.send(ev).await;
            }
        }
    }
    ctx.shared.pending_rpc.lock().await.clear();
    let reason = {
        let mut st = ctx.shared.state.lock().await;
        st.pending.clear();
        st.cached_changes.clear();
        st.last_stderr
            .clone()
            .or_else(|| Some("codex stdout closed".into()))
    };
    let _ = ctx
        .event_tx
        .send(NormalizedEvent::SessionClosed { reason })
        .await;
}

async fn handle_server_request(
    ctx: &ReaderContext,
    raw_id: Value,
    key: String,
    method: &str,
    params: &Value,
) {
    let (active, cached) = {
        let st = ctx.shared.state.lock().await;
        let c = if method == "item/fileChange/requestApproval" {
            s(params, &["itemId", "item_id"]).and_then(|id| st.cached_changes.get(id).cloned())
        } else {
            None
        };
        (st.active_turn.clone(), c)
    };

    let Some((request, pending)) = map_server_request(
        raw_id.clone(),
        key,
        method,
        params,
        active.as_deref(),
        cached,
    ) else {
        let err = json!({ "id": raw_id, "error": { "code": -32601, "message": "unsupported server request" } });
        let _ = write_line(&ctx.shared, &err).await;
        let ev = NormalizedEvent::Unknown {
            event_type: method.into(),
            payload: Some(params.clone()),
        };
        let _ = ctx.event_tx.send(ev).await;
        return;
    };

    let (iid, tid) = (request.interaction_id.clone(), request.turn_id.clone());
    let ro_perm = ctx.access == AccessMode::ReadOnly
        && matches!(pending.native_kind, NativePendingKind::Permissions(ref p) if requests_filesystem_write(p));
    let mut decision = if ro_perm {
        AutoDecision::Deny {
            reason: "workspace access is read_only".into(),
        }
    } else {
        evaluate_interaction(&request.kind, ctx.access, ctx.policy, &ctx.workspace, false)
    };
    if matches!(decision, AutoDecision::Allow)
        && let Some(root) = s(params, &["grantRoot", "grant_root"])
    {
        let e = FileChangeEntry {
            path: root.into(),
            move_path: None,
            kind: "grant_root".into(),
            unified_diff: None,
        };
        let chk = InteractionKind::FileChangeApproval {
            changes: vec![e],
            reason: None,
        };
        decision = evaluate_interaction(&chk, ctx.access, ctx.policy, &ctx.workspace, false);
    }

    match decision {
        AutoDecision::Allow | AutoDecision::Deny { .. } => {
            let did = if matches!(decision, AutoDecision::Allow) {
                "allow_once"
            } else {
                "deny_once"
            };
            let _ = ctx
                .event_tx
                .send(NormalizedEvent::InteractionRequested { request })
                .await;
            let _ = write_line(
                &ctx.shared,
                &build_reply(&pending, did, &BTreeMap::new(), None),
            )
            .await;
            let ev = NormalizedEvent::InteractionResolved {
                interaction_id: iid,
                turn_id: tid,
                decision_id: Some(did.into()),
            };
            let _ = ctx.event_tx.send(ev).await;
        }
        AutoDecision::Ask => {
            ctx.shared.state.lock().await.pending.insert(iid, pending);
            let _ = ctx
                .event_tx
                .send(NormalizedEvent::InteractionRequested { request })
                .await;
        }
    }
}

async fn handle_notification(ctx: &ReaderContext, method: &str, params: &Value) {
    if method == "serverRequest/resolved" {
        if let Some(key) = params
            .get("requestId")
            .or_else(|| params.get("id"))
            .and_then(id_key)
        {
            let resolved = {
                let mut st = ctx.shared.state.lock().await;
                let iid = st
                    .pending
                    .iter()
                    .find(|(_, p)| p.key == key)
                    .map(|(k, _)| k.clone());
                iid.map(|id| {
                    let tid = st
                        .pending
                        .remove(&id)
                        .map(|p| p.turn_id)
                        .or_else(|| s(params, &["turnId", "turn_id"]).map(Into::into))
                        .unwrap_or_else(|| "unknown".into());
                    (id, tid)
                })
            };
            if let Some((interaction_id, turn_id)) = resolved {
                let ev = NormalizedEvent::InteractionResolved {
                    interaction_id,
                    turn_id,
                    decision_id: None,
                };
                let _ = ctx.event_tx.send(ev).await;
            }
        }
        return;
    }

    let active = {
        let mut st = ctx.shared.state.lock().await;
        let item = params.get("item").unwrap_or(params);
        let id = s(item, &["id", "itemId", "item_id"]);
        match method {
            "item/started" if s(item, &["type"]) == Some("fileChange") => {
                if let Some(id) = id {
                    let changes = extract_changes(item);
                    if !changes.is_empty() {
                        st.cached_changes.insert(id.into(), changes);
                    }
                }
            }
            "item/completed" => {
                if let Some(id) = id {
                    st.cached_changes.remove(id);
                }
            }
            "turn/started" => {
                let tid = params
                    .get("turn")
                    .and_then(|t| s(t, &["id"]))
                    .or_else(|| s(params, &["turnId"]));
                if let Some(t) = tid {
                    st.active_turn = Some(t.into());
                }
            }
            _ => {}
        }
        st.active_turn.clone()
    };

    let expect = ctx.shared.expect_schema.load(Ordering::SeqCst);
    if let Some(event) = map_notification(method, params, active.as_deref(), expect) {
        if matches!(event, NormalizedEvent::TurnCompleted { .. }) {
            let mut st = ctx.shared.state.lock().await;
            st.active_turn = None;
            st.cached_changes.clear();
        }
        let _ = ctx.event_tx.send(event).await;
    }
}

fn pgid_kill(sig: &str, pgid: u32) -> std::process::Command {
    let mut c = std::process::Command::new("/bin/kill");
    c.args([sig, "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

async fn terminate_pgid(pgid: Option<u32>, mut child: Option<Child>) {
    if let Some(id) = pgid {
        let _ = Command::from(pgid_kill("-TERM", id)).status().await;
    }
    if let Some(ref mut proc) = child {
        if tokio::time::timeout(CLOSE_GRACE, proc.wait())
            .await
            .is_err()
        {
            if let Some(id) = pgid {
                let _ = Command::from(pgid_kill("-KILL", id)).status().await;
            }
            let _ = proc.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), proc.wait()).await;
        }
    } else if let Some(id) = pgid {
        tokio::time::sleep(CLOSE_GRACE).await;
        let _ = Command::from(pgid_kill("-KILL", id)).status().await;
    }
}

fn reap_pgid_blocking(pgid: Option<u32>, mut child: Option<Child>) {
    if let Some(id) = pgid {
        let _ = pgid_kill("-TERM", id).status();
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < deadline {
        if let Some(ref mut c) = child
            && matches!(c.try_wait(), Ok(Some(_)))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Some(id) = pgid {
        let _ = pgid_kill("-KILL", id).status();
    }
    if let Some(ref mut c) = child {
        let _ = c.start_kill();
        for _ in 0..10 {
            if matches!(c.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_is_codex_app() {
        assert_eq!(
            CodexAppDriver::with_binary(PathBuf::from("codex")).driver_id(),
            "codex-app"
        );
    }

    #[test]
    fn parses_frames_without_jsonrpc_field() {
        let resp = parse_incoming_line(r#"{"id":1,"result":{"userAgent":"codex/0.172"}}"#)
            .expect("parse resp");
        assert!(matches!(resp, IncomingFrame::Response(ref id, Ok(_)) if id == "1"));
        let notif = parse_incoming_line(
            r#"{"method":"item/agentMessage/delta","params":{"turnId":"t1","delta":"hi"}}"#,
        )
        .expect("parse notif");
        assert!(
            matches!(notif, IncomingFrame::Notification(ref m, _) if m == "item/agentMessage/delta")
        );
    }

    #[test]
    fn approval_policy_never_maps_to_never() {
        for p in [
            ApprovalPolicy::Interactive,
            ApprovalPolicy::AutoDeny,
            ApprovalPolicy::AutoApproveScoped,
        ] {
            assert_ne!(wire::approval_policy_wire_value(p), "never");
        }
    }
}
