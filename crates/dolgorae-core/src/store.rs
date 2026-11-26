//! Dedicated single-writer SQLite persistence layer for `dolgorae-core`.
//!
//! All `rusqlite::Connection` operations execute exclusively on a single
//! background OS thread communicating over a bounded `sync_channel` and
//! `tokio::sync::oneshot` replies. No SQLite call ever runs on a Tokio worker
//! thread and no transaction is ever held across an `.await` point.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use dolgorae_spi::{
    AccessMode, ApprovalPolicy, DecisionOption, InteractionKind, InteractionRequest,
    TurnTerminalStatus,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use zeroize::Zeroizing;

use crate::artifacts::ensure_dir_0700;
use crate::error::CoreError;
use crate::roles::hex_lower;
use crate::types::{
    ActorScope, AnswerInteractionRequest, AnswerInteractionResponse, ArtifactRef, Cursor,
    InteractionList, InteractionRecord, InteractionStatus, MailboxMessage, MailboxPage,
    ReplayResult, SessionEvent, SessionFilter, SessionPolicy, SessionSnapshot, SessionStatus,
    TimelineItem, TimelinePage, WriterLease, now_rfc3339,
};
use crate::{MAX_INLINE_BYTES, MAX_PAGE_BYTES};

const SCHEMA_VERSION: i32 = 1;
const DB_CHANNEL_CAPACITY: usize = 1024;

type DbJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Full persisted session metadata row used internally by `CoreEngine`.
#[derive(Debug, Clone)]
pub(crate) struct SessionRecord {
    pub snapshot: SessionSnapshot,
    pub allow_delegation: bool,
    pub provider_session_id: String,
    pub role_instructions: Option<String>,
    pub output_schema: Option<Value>,
    pub earliest_retained_cursor: u64,
}

/// Input bundle for inserting a new durable session row.
#[derive(Debug, Clone)]
pub(crate) struct NewSessionRow {
    pub session_id: String,
    pub root_session_id: String,
    pub parent_session_id: Option<String>,
    pub depth: u8,
    pub policy: SessionPolicy,
    pub allow_delegation: bool,
    pub role: Option<String>,
    pub role_snapshot_sha256: Option<String>,
    pub role_instructions: Option<String>,
    pub driver: String,
    pub provider_session_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub workspace: String,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    pub output_schema: Option<Value>,
    pub actor_token_hash: Option<String>,
}

/// Input bundle for updating a resumed session row.
#[derive(Debug, Clone)]
pub(crate) struct ResumeSessionUpdate {
    pub session_id: String,
    pub provider_session_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    pub new_actor_token_hash: Option<String>,
}

/// Input bundle for inserting a durable mailbox message.
#[derive(Debug, Clone)]
pub(crate) struct NewMailboxRow {
    pub message_id: String,
    pub session_id: String,
    pub from_session_id: Option<String>,
    pub subject: Option<String>,
    pub inline_body: Option<String>,
    pub artifact: Option<ArtifactRef>,
    pub artifact_ids: Vec<String>,
}

/// Input bundle for recording a durable session event.
#[derive(Debug, Clone)]
pub(crate) struct NewDurableEvent {
    pub session_id: String,
    pub event_type: String,
    pub turn_id: Option<String>,
    pub inline_text: Option<String>,
    pub payload: Value,
    pub artifact: Option<ArtifactRef>,
    pub bump_interaction_revision: bool,
}

/// Stored metadata for an immutable artifact.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactMeta {
    pub artifact_ref: ArtifactRef,
}

/// Outcome of inspecting a session for `wait_sessions` settlement.
#[derive(Debug, Clone)]
pub(crate) struct WaitTargetState {
    pub snapshot: SessionSnapshot,
    pub completed_turn_count: u64,
    pub has_new_mailbox_for_waiter: bool,
}

struct StoreInner {
    tx: Mutex<Option<tokio_mpsc::Sender<DbJob>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
    max_retained_events: u64,
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.tx.lock() {
            let _ = guard.take();
        }
        if let Ok(mut guard) = self.handle.lock()
            && let Some(handle) = guard.take()
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

/// Asynchronous handle to the single-writer SQLite persistence thread.
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}

impl Store {
    /// Opens `<home_dir>/state.db` on a dedicated single-writer OS thread.
    pub fn open(home_dir: &Path, max_retained_events: u64) -> Result<Self, CoreError> {
        ensure_dir_0700(home_dir)?;
        let db_path = home_dir.join("state.db");
        let (tx, mut rx) = tokio_mpsc::channel::<DbJob>(DB_CHANNEL_CAPACITY);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<(), CoreError>>(1);

        let handle = thread::Builder::new()
            .name("dolgorae-sqlite-writer".to_owned())
            .spawn(move || {
                let mut conn = match open_and_init_db(&db_path) {
                    Ok(c) => {
                        let _ = ready_tx.send(Ok(()));
                        c
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .map_err(|e| {
                CoreError::internal(format!("failed to spawn sqlite writer thread: {e}"))
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(StoreInner {
                    tx: Mutex::new(Some(tx)),
                    handle: Mutex::new(Some(handle)),
                    max_retained_events,
                }),
            }),
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(err)
            }
            Err(_) => {
                let _ = handle.join();
                Err(CoreError::internal(
                    "sqlite writer thread exited before initialization completed",
                ))
            }
        }
    }

    async fn call<F, R>(&self, f: F) -> Result<R, CoreError>
    where
        F: FnOnce(&mut Connection) -> Result<R, CoreError> + Send + 'static,
        R: Send + 'static,
    {
        let sender = {
            let guard = self
                .inner
                .tx
                .lock()
                .map_err(|_| CoreError::internal("sqlite writer lock poisoned"))?;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| CoreError::internal("sqlite store is closed"))?
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        let job: DbJob = Box::new(move |conn| {
            let res = f(conn);
            let _ = reply_tx.send(res);
        });

        sender
            .send(job)
            .await
            .map_err(|_| CoreError::internal("sqlite writer thread terminated"))?;

        reply_rx
            .await
            .map_err(|_| CoreError::internal("sqlite writer dropped reply channel"))?
    }

    /// Runs daemon startup crash recovery: transitions previously active
    /// sessions to `idle`, marks in-flight turns `interrupted_unknown`, expires
    /// pending interactions (emitting durable `interaction_expired` events),
    /// bumps `session_generation`, revokes active actor tokens, releases stale
    /// writer leases, and appends `session_recovered`.
    pub async fn recover_on_startup(&self) -> Result<u64, CoreError> {
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();

            let active_ids: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT session_id FROM sessions WHERE status = 'active'")
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(db_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
            };

            let mut recovered = 0u64;
            for sid in &active_ids {
                tx.execute(
                    "DELETE FROM turns WHERE session_id = ?1 AND status = 'reserved'",
                    params![sid],
                )
                .map_err(db_err)?;

                let interrupted_turns = tx
                    .execute(
                        "UPDATE turns SET status = 'interrupted_unknown', completed_at = ?1
                         WHERE session_id = ?2 AND status = 'running'",
                        params![now, sid],
                    )
                    .map_err(db_err)?;

                let expired_evs = expire_pending_interactions_tx(
                    &tx,
                    max_ret,
                    sid,
                    &now,
                    "daemon_startup_recovery",
                )?;
                let expired_interactions = expired_evs.len();

                tx.execute(
                    "UPDATE actor_tokens SET revoked_at = ?1
                     WHERE session_id = ?2 AND revoked_at IS NULL",
                    params![now, sid],
                )
                .map_err(db_err)?;

                tx.execute(
                    "DELETE FROM writer_leases WHERE holder_session_id = ?1",
                    params![sid],
                )
                .map_err(db_err)?;

                tx.execute(
                    "UPDATE sessions
                     SET status = 'idle',
                         active_turn_id = NULL,
                         session_generation = session_generation + 1,
                         updated_at = ?1
                     WHERE session_id = ?2",
                    params![now, sid],
                )
                .map_err(db_err)?;

                append_event_tx(
                    &tx,
                    max_ret,
                    &NewDurableEvent {
                        session_id: sid.clone(),
                        event_type: "session_recovered".to_owned(),
                        turn_id: None,
                        inline_text: None,
                        payload: json!({
                            "interrupted_turns": interrupted_turns,
                            "expired_interactions": expired_interactions,
                            "recovery_status": "interrupted_unknown",
                        }),
                        artifact: None,
                        bump_interaction_revision: false,
                    },
                )?;
                recovered += 1;
            }

            // Revoke any tokens or leases belonging to non-active sessions.
            tx.execute(
                "UPDATE actor_tokens SET revoked_at = ?1
                 WHERE revoked_at IS NULL
                   AND session_id IN (SELECT session_id FROM sessions WHERE status != 'active')",
                params![now],
            )
            .map_err(db_err)?;
            tx.execute(
                "DELETE FROM writer_leases
                 WHERE holder_session_id IN (
                     SELECT session_id FROM sessions WHERE status IN ('closed', 'failed')
                 )",
                [],
            )
            .map_err(db_err)?;

            tx.commit().map_err(db_err)?;
            Ok(recovered)
        })
        .await
    }

    pub(crate) async fn insert_session(
        &self,
        row: NewSessionRow,
    ) -> Result<(SessionSnapshot, SessionEvent), CoreError> {
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let output_schema_json = row
                .output_schema
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;

            tx.execute(
                "INSERT INTO sessions (
                    session_id, root_session_id, parent_session_id, depth,
                    max_delegation_depth, allow_actor_collaboration, allow_delegation,
                    role, role_snapshot_sha256, role_instructions,
                    driver, provider_session_id, model, effort, workspace,
                    status, access, approval_policy, output_schema_json,
                    session_generation, revision, interaction_revision,
                    earliest_retained_cursor, active_turn_id, created_at, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4,
                    ?5, ?6, ?7,
                    ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15,
                    'active', ?16, ?17, ?18,
                    1, 0, 0,
                    0, NULL, ?19, ?19
                 )",
                params![
                    row.session_id,
                    row.root_session_id,
                    row.parent_session_id,
                    i64::from(row.depth),
                    i64::from(row.policy.max_delegation_depth),
                    if row.policy.allow_actor_collaboration {
                        1i64
                    } else {
                        0i64
                    },
                    if row.allow_delegation { 1i64 } else { 0i64 },
                    row.role,
                    row.role_snapshot_sha256,
                    row.role_instructions,
                    row.driver,
                    row.provider_session_id,
                    row.model,
                    row.effort,
                    row.workspace,
                    access_to_str(row.access),
                    approval_to_str(row.approval_policy),
                    output_schema_json,
                    now,
                ],
            )
            .map_err(db_err)?;

            if let Some(ref hash) = row.actor_token_hash {
                tx.execute(
                    "INSERT INTO actor_tokens (token_hash, session_id, root_session_id, created_at, revoked_at)
                     VALUES (?1, ?2, ?3, ?4, NULL)",
                    params![hash, row.session_id, row.root_session_id, now],
                )
                .map_err(db_err)?;
            }

            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: row.session_id.clone(),
                    event_type: "session_created".to_owned(),
                    turn_id: None,
                    inline_text: None,
                    payload: json!({
                        "session_id": row.session_id,
                        "root_session_id": row.root_session_id,
                        "parent_session_id": row.parent_session_id,
                        "driver": row.driver,
                        "role": row.role,
                        "workspace": row.workspace,
                    }),
                    artifact: None,
                    bump_interaction_revision: false,
                },
            )?;

            let snap = load_session_record_tx(&tx, &row.session_id)?.snapshot;
            tx.commit().map_err(db_err)?;
            Ok((snap, ev))
        })
        .await
    }

    pub(crate) async fn get_session_record(
        &self,
        session_id: &str,
    ) -> Result<SessionRecord, CoreError> {
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            Ok(rec)
        })
        .await
    }

    pub(crate) async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSnapshot>, CoreError> {
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let mut sql = String::from("SELECT session_id FROM sessions WHERE 1=1");
            let mut args: Vec<String> = Vec::new();
            if let Some(parent) = filter.parent_session_id {
                sql.push_str(" AND parent_session_id = ?");
                args.push(parent);
            }
            if let Some(root) = filter.root_session_id {
                sql.push_str(" AND root_session_id = ?");
                args.push(root);
            }
            if let Some(status) = filter.status {
                sql.push_str(" AND status = ?");
                args.push(status.as_str().to_owned());
            }
            sql.push_str(" ORDER BY created_at ASC, session_id ASC");

            let ids: Vec<String> = {
                let mut stmt = tx.prepare(&sql).map_err(db_err)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                        r.get::<_, String>(0)
                    })
                    .map_err(db_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
            };

            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.push(load_session_record_tx(&tx, &id)?.snapshot);
            }
            Ok(out)
        })
        .await
    }

    pub(crate) async fn get_ancestors(
        &self,
        start_session_id: &str,
    ) -> Result<Vec<SessionSnapshot>, CoreError> {
        let mut cursor_id = Some(start_session_id.to_owned());
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let mut chain = Vec::new();
            let mut visited = std::collections::HashSet::new();
            while let Some(id) = cursor_id.take() {
                if !visited.insert(id.clone()) {
                    break;
                }
                let rec = load_session_record_tx(&tx, &id)?;
                cursor_id = rec.snapshot.parent_session_id.clone();
                chain.push(rec.snapshot);
            }
            Ok(chain)
        })
        .await
    }

    pub(crate) async fn update_session_resumed(
        &self,
        upd: ResumeSessionUpdate,
    ) -> Result<(SessionSnapshot, Vec<SessionEvent>), CoreError> {
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let current = load_session_record_tx(&tx, &upd.session_id)?;
            if current.snapshot.status == SessionStatus::Closed {
                return Err(CoreError::snapshot_conflict(
                    "cannot resume a closed session",
                ));
            }

            tx.execute(
                "DELETE FROM turns WHERE session_id = ?1 AND status = 'reserved'",
                params![upd.session_id],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE turns SET status = 'interrupted', completed_at = ?1
                 WHERE session_id = ?2 AND status = 'running'",
                params![now, upd.session_id],
            )
            .map_err(db_err)?;

            let mut events = expire_pending_interactions_tx(
                &tx,
                max_ret,
                &upd.session_id,
                &now,
                "session_resumed",
            )?;

            tx.execute(
                "UPDATE sessions
                 SET provider_session_id = ?1,
                     model = ?2,
                     effort = ?3,
                     access = ?4,
                     approval_policy = ?5,
                     status = 'active',
                     active_turn_id = NULL,
                     session_generation = session_generation + 1,
                     updated_at = ?6
                 WHERE session_id = ?7",
                params![
                    upd.provider_session_id,
                    upd.model,
                    upd.effort,
                    access_to_str(upd.access),
                    approval_to_str(upd.approval_policy),
                    now,
                    upd.session_id,
                ],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE actor_tokens SET revoked_at = ?1
                 WHERE session_id = ?2 AND revoked_at IS NULL",
                params![now, upd.session_id],
            )
            .map_err(db_err)?;

            if let Some(ref hash) = upd.new_actor_token_hash {
                tx.execute(
                    "INSERT INTO actor_tokens (token_hash, session_id, root_session_id, created_at, revoked_at)
                     VALUES (?1, ?2, ?3, ?4, NULL)",
                    params![hash, upd.session_id, current.snapshot.root_session_id, now],
                )
                .map_err(db_err)?;
            }

            let new_gen = current.snapshot.session_generation + 1;
            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: upd.session_id.clone(),
                    event_type: "session_resumed".to_owned(),
                    turn_id: None,
                    inline_text: None,
                    payload: json!({
                        "session_id": upd.session_id,
                        "session_generation": new_gen,
                    }),
                    artifact: None,
                    bump_interaction_revision: false,
                },
            )?;
            events.push(ev);

            let snap = load_session_record_tx(&tx, &upd.session_id)?.snapshot;
            tx.commit().map_err(db_err)?;
            Ok((snap, events))
        })
        .await
    }

    pub(crate) async fn close_or_fail_session(
        &self,
        session_id: &str,
        target_status: SessionStatus,
        reason: Option<String>,
    ) -> Result<(SessionSnapshot, Vec<SessionEvent>), CoreError> {
        let sid = session_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let current = load_session_record_tx(&tx, &sid)?;
            if matches!(
                current.snapshot.status,
                SessionStatus::Closed | SessionStatus::Failed
            ) {
                return Ok((current.snapshot, Vec::new()));
            }

            tx.execute(
                "DELETE FROM turns WHERE session_id = ?1 AND status = 'reserved'",
                params![sid],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE turns SET status = 'interrupted', completed_at = ?1
                 WHERE session_id = ?2 AND status = 'running'",
                params![now, sid],
            )
            .map_err(db_err)?;

            let mut events =
                expire_pending_interactions_tx(&tx, max_ret, &sid, &now, "session_closed")?;

            tx.execute(
                "UPDATE actor_tokens SET revoked_at = ?1
                 WHERE session_id = ?2 AND revoked_at IS NULL",
                params![now, sid],
            )
            .map_err(db_err)?;

            tx.execute(
                "DELETE FROM writer_leases WHERE holder_session_id = ?1",
                params![sid],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE sessions
                 SET status = ?1,
                     active_turn_id = NULL,
                     session_generation = session_generation + 1,
                     updated_at = ?2
                 WHERE session_id = ?3",
                params![target_status.as_str(), now, sid],
            )
            .map_err(db_err)?;

            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid.clone(),
                    event_type: "session_closed".to_owned(),
                    turn_id: None,
                    inline_text: None,
                    payload: json!({
                        "session_id": sid,
                        "status": target_status,
                        "reason": reason,
                    }),
                    artifact: None,
                    bump_interaction_revision: false,
                },
            )?;
            events.push(ev);

            let snap = load_session_record_tx(&tx, &sid)?.snapshot;
            tx.commit().map_err(db_err)?;
            Ok((snap, events))
        })
        .await
    }

    /// Gracefully detaches a live session on daemon shutdown (SIGINT/SIGTERM):
    /// transitions `Active` -> `Idle` (resumable!), marks running turns
    /// `interrupted_unknown`, expires pending interactions with durable
    /// `interaction_expired` events, bumps `session_generation`, revokes active
    /// actor tokens, releases writer leases, and appends `session_recovered`.
    pub(crate) async fn detach_session_on_shutdown(
        &self,
        session_id: &str,
    ) -> Result<(SessionSnapshot, Vec<SessionEvent>), CoreError> {
        let sid = session_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let current = load_session_record_tx(&tx, &sid)?;
            if current.snapshot.status != SessionStatus::Active {
                return Ok((current.snapshot, Vec::new()));
            }

            tx.execute(
                "DELETE FROM turns WHERE session_id = ?1 AND status = 'reserved'",
                params![sid],
            )
            .map_err(db_err)?;

            let interrupted_turns = tx
                .execute(
                    "UPDATE turns SET status = 'interrupted_unknown', completed_at = ?1
                     WHERE session_id = ?2 AND status = 'running'",
                    params![now, sid],
                )
                .map_err(db_err)?;

            let mut events =
                expire_pending_interactions_tx(&tx, max_ret, &sid, &now, "daemon_shutdown")?;
            let expired_interactions = events.len();

            tx.execute(
                "UPDATE actor_tokens SET revoked_at = ?1
                 WHERE session_id = ?2 AND revoked_at IS NULL",
                params![now, sid],
            )
            .map_err(db_err)?;

            tx.execute(
                "DELETE FROM writer_leases WHERE holder_session_id = ?1",
                params![sid],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE sessions
                 SET status = 'idle',
                     active_turn_id = NULL,
                     session_generation = session_generation + 1,
                     updated_at = ?1
                 WHERE session_id = ?2",
                params![now, sid],
            )
            .map_err(db_err)?;

            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid.clone(),
                    event_type: "session_recovered".to_owned(),
                    turn_id: None,
                    inline_text: None,
                    payload: json!({
                        "interrupted_turns": interrupted_turns,
                        "expired_interactions": expired_interactions,
                        "recovery_status": "interrupted_unknown",
                        "reason": "daemon_shutdown",
                    }),
                    artifact: None,
                    bump_interaction_revision: false,
                },
            )?;
            events.push(ev);

            let snap = load_session_record_tx(&tx, &sid)?.snapshot;
            tx.commit().map_err(db_err)?;
            Ok((snap, events))
        })
        .await
    }

    /// Reserves the single active turn slot AND persists input metadata on the
    /// placeholder turn row BEFORE invoking `driver.send_turn(...)`, so early
    /// `TurnStarted` / `AssistantMessage` events see the exact prompt & schema.
    pub(crate) async fn reserve_active_turn(
        &self,
        session_id: &str,
        placeholder_turn_id: &str,
        inline_text: Option<String>,
        input_artifact: Option<ArtifactRef>,
        output_schema_override: Option<Value>,
    ) -> Result<(), CoreError> {
        let sid = session_id.to_owned();
        let placeholder = placeholder_turn_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            if rec.snapshot.status != SessionStatus::Active {
                return Err(CoreError::snapshot_conflict(format!(
                    "session '{sid}' is {} and cannot accept turns",
                    rec.snapshot.status.as_str()
                )));
            }
            if rec.snapshot.active_turn_id.is_some() {
                return Err(CoreError::snapshot_conflict(format!(
                    "session '{sid}' already has an active turn"
                )));
            }
            let now = now_rfc3339();
            let input_art_json = input_artifact
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;
            let schema_json = output_schema_override
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;

            tx.execute(
                "INSERT INTO turns (
                    turn_id, session_id, session_generation, status,
                    input_text, input_artifact_json, output_schema_json,
                    error_message, created_at, completed_at
                 ) VALUES (?1, ?2, ?3, 'reserved', ?4, ?5, ?6, NULL, ?7, NULL)",
                params![
                    placeholder,
                    sid,
                    rec.snapshot.session_generation as i64,
                    inline_text,
                    input_art_json,
                    schema_json,
                    now,
                ],
            )
            .map_err(db_err)?;

            tx.execute(
                "UPDATE sessions SET active_turn_id = ?1, updated_at = ?2 WHERE session_id = ?3",
                params![placeholder, now, sid],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn release_turn_reservation(
        &self,
        session_id: &str,
        placeholder_turn_id: &str,
    ) -> Result<(), CoreError> {
        let sid = session_id.to_owned();
        let placeholder = placeholder_turn_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            tx.execute(
                "DELETE FROM turns WHERE session_id = ?1 AND turn_id = ?2 AND status = 'reserved'",
                params![sid, placeholder],
            )
            .map_err(db_err)?;
            tx.execute(
                "UPDATE sessions SET active_turn_id = NULL, updated_at = ?1
                 WHERE session_id = ?2 AND active_turn_id = ?3",
                params![now_rfc3339(), sid, placeholder],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            Ok(())
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finalize_turn_start(
        &self,
        session_id: &str,
        turn_id: &str,
        placeholder_turn_id: Option<&str>,
        inline_text: Option<String>,
        input_artifact: Option<ArtifactRef>,
        output_schema_override: Option<Value>,
    ) -> Result<(u64, Cursor, Option<SessionEvent>), CoreError> {
        let sid = session_id.to_owned();
        let tid = turn_id.to_owned();
        let placeholder = placeholder_turn_id.map(str::to_owned);
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            let now = now_rfc3339();
            let supplied_art_json = input_artifact
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;
            let supplied_schema_json = output_schema_override
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;

            struct ReservedTurnMeta {
                turn_id: String,
                input_text: Option<String>,
                input_artifact_json: Option<String>,
                output_schema_json: Option<String>,
            }

            // Pull any reserved placeholder metadata (populated by `reserve_active_turn`)
            // so even early drain-task `TurnStarted` events inherit prompt & schema!
            let reserved_row: Option<ReservedTurnMeta> = if let Some(ref ph) = placeholder {
                tx.query_row(
                    "SELECT turn_id, input_text, input_artifact_json, output_schema_json
                     FROM turns WHERE session_id = ?1 AND turn_id = ?2 AND status = 'reserved'",
                    params![sid, ph],
                    |r| {
                        Ok(ReservedTurnMeta {
                            turn_id: r.get(0)?,
                            input_text: r.get(1)?,
                            input_artifact_json: r.get(2)?,
                            output_schema_json: r.get(3)?,
                        })
                    },
                )
                .optional()
                .map_err(db_err)?
            } else {
                tx.query_row(
                    "SELECT turn_id, input_text, input_artifact_json, output_schema_json
                     FROM turns WHERE session_id = ?1 AND status = 'reserved'
                     ORDER BY created_at DESC LIMIT 1",
                    params![sid],
                    |r| {
                        Ok(ReservedTurnMeta {
                            turn_id: r.get(0)?,
                            input_text: r.get(1)?,
                            input_artifact_json: r.get(2)?,
                            output_schema_json: r.get(3)?,
                        })
                    },
                )
                .optional()
                .map_err(db_err)?
            };

            let (reserved_ph_id, res_text, res_art_json, res_schema_json) = match reserved_row {
                Some(m) => (
                    Some(m.turn_id),
                    m.input_text,
                    m.input_artifact_json,
                    m.output_schema_json,
                ),
                None => (None, None, None, None),
            };

            if let Some(ref ph_id) = reserved_ph_id {
                tx.execute(
                    "DELETE FROM turns WHERE session_id = ?1 AND turn_id = ?2 AND status = 'reserved'",
                    params![sid, ph_id],
                )
                .map_err(db_err)?;
            }

            let effective_text = inline_text.or(res_text);
            let effective_art_json = supplied_art_json.or(res_art_json);
            let effective_schema_json = supplied_schema_json.or(res_schema_json);
            let effective_artifact: Option<ArtifactRef> = match input_artifact {
                Some(a) => Some(a),
                None => effective_art_json
                    .as_deref()
                    .map(|raw| serde_json::from_str(raw).map_err(de_err))
                    .transpose()?,
            };

            let existing_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM turns WHERE session_id = ?1 AND turn_id = ?2",
                    params![sid, tid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            let active_is_placeholder = |cur: Option<&str>| -> bool {
                match (cur, placeholder.as_deref(), reserved_ph_id.as_deref()) {
                    (Some(c), Some(ph), _) if c == ph => true,
                    (Some(c), _, Some(ph)) if c == ph => true,
                    (Some(c), _, _) if c.starts_with("pending-") => true,
                    _ => false,
                }
            };

            if let Some(status_str) = existing_status {
                // Idempotent & monotonic: enrich input metadata only, never downgrade
                // terminal status, and never emit a duplicate `turn_started` event.
                tx.execute(
                    "UPDATE turns
                     SET input_text = COALESCE(?1, input_text),
                         input_artifact_json = COALESCE(?2, input_artifact_json),
                         output_schema_json = COALESCE(?3, output_schema_json)
                     WHERE session_id = ?4 AND turn_id = ?5",
                    params![
                        effective_text,
                        effective_art_json,
                        effective_schema_json,
                        sid,
                        tid
                    ],
                )
                .map_err(db_err)?;

                if status_str == "running" {
                    if active_is_placeholder(rec.snapshot.active_turn_id.as_deref()) {
                        tx.execute(
                            "UPDATE sessions SET active_turn_id = ?1, updated_at = ?2 WHERE session_id = ?3",
                            params![tid, now, sid],
                        )
                        .map_err(db_err)?;
                    }
                } else if active_is_placeholder(rec.snapshot.active_turn_id.as_deref()) {
                    tx.execute(
                        "UPDATE sessions SET active_turn_id = NULL, updated_at = ?1 WHERE session_id = ?2",
                        params![now, sid],
                    )
                    .map_err(db_err)?;
                }

                tx.commit().map_err(db_err)?;
                return Ok((rec.snapshot.revision, rec.snapshot.head_cursor, None));
            }

            tx.execute(
                "INSERT INTO turns (
                    turn_id, session_id, session_generation, status,
                    input_text, input_artifact_json, output_schema_json,
                    error_message, created_at, completed_at
                 ) VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, NULL, ?7, NULL)",
                params![
                    tid,
                    sid,
                    rec.snapshot.session_generation as i64,
                    effective_text,
                    effective_art_json,
                    effective_schema_json,
                    now,
                ],
            )
            .map_err(db_err)?;

            if rec.snapshot.active_turn_id.is_none()
                || active_is_placeholder(rec.snapshot.active_turn_id.as_deref())
            {
                tx.execute(
                    "UPDATE sessions SET active_turn_id = ?1, updated_at = ?2 WHERE session_id = ?3",
                    params![tid, now, sid],
                )
                .map_err(db_err)?;
            }

            let (ev, new_rev, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid.clone(),
                    event_type: "turn_started".to_owned(),
                    turn_id: Some(tid.clone()),
                    inline_text: effective_text,
                    payload: json!({
                        "turn_id": tid,
                        "session_generation": rec.snapshot.session_generation,
                    }),
                    artifact: effective_artifact,
                    bump_interaction_revision: false,
                },
            )?;

            tx.commit().map_err(db_err)?;
            Ok((new_rev, Cursor(new_rev), Some(ev)))
        })
        .await
    }

    pub(crate) async fn get_turn_output_schema(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<Value>, CoreError> {
        let sid = session_id.to_owned();
        let tid = turn_id.to_owned();
        self.call(move |conn| {
            let turn_schema: Option<String> = conn
                .query_row(
                    "SELECT output_schema_json FROM turns WHERE session_id = ?1 AND turn_id = ?2",
                    params![sid, tid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(db_err)?
                .flatten();

            if let Some(raw) = turn_schema {
                let parsed: Value = serde_json::from_str(&raw).map_err(de_err)?;
                return Ok(Some(parsed));
            }

            let session_schema: Option<String> = conn
                .query_row(
                    "SELECT output_schema_json FROM sessions WHERE session_id = ?1",
                    params![sid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(db_err)?
                .flatten();

            session_schema
                .map(|raw| serde_json::from_str(&raw).map_err(de_err))
                .transpose()
        })
        .await
    }

    pub(crate) async fn complete_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        status: TurnTerminalStatus,
        error: Option<String>,
    ) -> Result<SessionEvent, CoreError> {
        let sid = session_id.to_owned();
        let tid = turn_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let status_str = turn_status_to_str(status);

            let updated = tx
                .execute(
                    "UPDATE turns SET status = ?1, error_message = ?2, completed_at = ?3
                     WHERE session_id = ?4 AND turn_id = ?5",
                    params![status_str, error, now, sid, tid],
                )
                .map_err(db_err)?;

            if updated == 0 {
                let rec = load_session_record_tx(&tx, &sid)?;
                tx.execute(
                    "INSERT INTO turns (
                        turn_id, session_id, session_generation, status,
                        input_text, input_artifact_json, output_schema_json,
                        error_message, created_at, completed_at
                     ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, ?5, ?6, ?6)",
                    params![
                        tid,
                        sid,
                        rec.snapshot.session_generation as i64,
                        status_str,
                        error,
                        now,
                    ],
                )
                .map_err(db_err)?;
            }

            tx.execute(
                "UPDATE sessions SET active_turn_id = NULL, updated_at = ?1
                 WHERE session_id = ?2 AND (active_turn_id = ?3 OR active_turn_id LIKE 'pending-%')",
                params![now, sid, tid],
            )
            .map_err(db_err)?;

            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid,
                    event_type: "turn_completed".to_owned(),
                    turn_id: Some(tid.clone()),
                    inline_text: None,
                    payload: json!({
                        "turn_id": tid,
                        "status": status,
                        "error": error,
                    }),
                    artifact: None,
                    bump_interaction_revision: false,
                },
            )?;

            tx.commit().map_err(db_err)?;
            Ok(ev)
        })
        .await
    }

    pub(crate) async fn append_durable_event(
        &self,
        event: NewDurableEvent,
    ) -> Result<SessionEvent, CoreError> {
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let (ev, _, _) = append_event_tx(&tx, max_ret, &event)?;
            tx.commit().map_err(db_err)?;
            Ok(ev)
        })
        .await
    }

    pub(crate) async fn record_interaction_requested(
        &self,
        session_id: &str,
        request: InteractionRequest,
    ) -> Result<Option<(InteractionRecord, SessionEvent)>, CoreError> {
        let sid = session_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            let now = now_rfc3339();
            let kind_json = serde_json::to_string(&request.kind).map_err(ser_err)?;
            let decisions_json = serde_json::to_string(&request.decisions).map_err(ser_err)?;
            let schema_json = request
                .response_schema
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;

            let next_rev = rec.snapshot.revision + 1;
            let record = InteractionRecord {
                interaction_id: request.interaction_id.clone(),
                turn_id: request.turn_id.clone(),
                session_generation: rec.snapshot.session_generation,
                interaction_revision: next_rev,
                session_revision: next_rev,
                kind: request.kind.clone(),
                original_kind: request.original_kind.clone(),
                title: request.title.clone(),
                decisions: request.decisions.clone(),
                protected: request.protected,
                response_schema: request.response_schema.clone(),
                max_response_bytes: request.max_response_bytes,
                status: InteractionStatus::Pending,
            };

            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO interactions (
                        session_id, interaction_id, turn_id, session_generation,
                        interaction_revision, session_revision, kind_json, original_kind,
                        title, decisions_json, protected, response_schema_json,
                        max_response_bytes, status, decision_id, answer_json,
                        created_at, resolved_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4,
                        ?5, ?6, ?7, ?8,
                        ?9, ?10, ?11, ?12,
                        ?13, 'pending', NULL, NULL,
                        ?14, NULL
                     )",
                    params![
                        sid,
                        request.interaction_id,
                        request.turn_id,
                        rec.snapshot.session_generation as i64,
                        next_rev as i64,
                        next_rev as i64,
                        kind_json,
                        request.original_kind,
                        request.title,
                        decisions_json,
                        if request.protected { 1i64 } else { 0i64 },
                        schema_json,
                        request.max_response_bytes.map(|v| v as i64),
                        now,
                    ],
                )
                .map_err(db_err)?;

            if inserted == 0 {
                // Duplicate interaction_id from driver: do NOT reset a resolved/pending entry.
                return Ok(None);
            }

            let payload = serde_json::to_value(&record).map_err(ser_err)?;
            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid,
                    event_type: "interaction_requested".to_owned(),
                    turn_id: Some(request.turn_id),
                    inline_text: None,
                    payload,
                    artifact: None,
                    bump_interaction_revision: true,
                },
            )?;

            tx.commit().map_err(db_err)?;
            Ok(Some((record, ev)))
        })
        .await
    }

    pub(crate) async fn resolve_interaction_from_driver(
        &self,
        session_id: &str,
        interaction_id: &str,
        turn_id: &str,
        decision_id: Option<String>,
    ) -> Result<Option<SessionEvent>, CoreError> {
        let sid = session_id.to_owned();
        let iid = interaction_id.to_owned();
        let tid = turn_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            let current_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM interactions WHERE session_id = ?1 AND interaction_id = ?2",
                    params![sid, iid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            if current_status.as_deref() != Some("pending") {
                return Ok(None);
            }

            tx.execute(
                "UPDATE interactions
                 SET status = 'resolved', decision_id = COALESCE(?1, decision_id), resolved_at = ?2
                 WHERE session_id = ?3 AND interaction_id = ?4",
                params![decision_id, now, sid, iid],
            )
            .map_err(db_err)?;

            let (ev, _, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: sid,
                    event_type: "interaction_resolved".to_owned(),
                    turn_id: Some(tid.clone()),
                    inline_text: None,
                    payload: json!({
                        "interaction_id": iid,
                        "turn_id": tid,
                        "decision_id": decision_id,
                    }),
                    artifact: None,
                    bump_interaction_revision: true,
                },
            )?;

            tx.commit().map_err(db_err)?;
            Ok(Some(ev))
        })
        .await
    }

    /// Read-only validation of an interaction answer BEFORE delivering it to the driver.
    pub(crate) async fn validate_interaction_answer(
        &self,
        session_id: &str,
        interaction_id: &str,
        req: AnswerInteractionRequest,
    ) -> Result<String, CoreError> {
        let sid = session_id.to_owned();
        let iid = interaction_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let answer_json = validate_interaction_answer_tx(&tx, &sid, &iid, &req)?;
            Ok(answer_json)
        })
        .await
    }

    /// Commits a delivered interaction answer AFTER the driver accepted it.
    pub(crate) async fn commit_interaction_answer(
        &self,
        session_id: &str,
        interaction_id: &str,
        req: AnswerInteractionRequest,
        answer_json: String,
    ) -> Result<(AnswerInteractionResponse, Option<SessionEvent>), CoreError> {
        let sid = session_id.to_owned();
        let iid = interaction_id.to_owned();
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let session = load_session_record_tx(&tx, &sid)?;
            let current_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM interactions WHERE session_id = ?1 AND interaction_id = ?2",
                    params![sid, iid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            let Some(status_str) = current_status else {
                return Err(CoreError::not_found(format!(
                    "interaction '{iid}' not found on session '{sid}'"
                )));
            };
            let status = InteractionStatus::parse_str(&status_str)?;
            let now = now_rfc3339();

            match status {
                InteractionStatus::Pending => {
                    tx.execute(
                        "UPDATE interactions
                         SET status = 'resolved', decision_id = ?1, answer_json = ?2, resolved_at = ?3
                         WHERE session_id = ?4 AND interaction_id = ?5",
                        params![req.decision_id, answer_json, now, sid, iid],
                    )
                    .map_err(db_err)?;

                    let (ev, new_rev, new_int_rev) = append_event_tx(
                        &tx,
                        max_ret,
                        &NewDurableEvent {
                            session_id: sid,
                            event_type: "interaction_resolved".to_owned(),
                            turn_id: Some(req.turn_id),
                            inline_text: None,
                            payload: json!({
                                "interaction_id": iid,
                                "decision_id": req.decision_id,
                            }),
                            artifact: None,
                            bump_interaction_revision: true,
                        },
                    )?;

                    tx.commit().map_err(db_err)?;
                    let resp = AnswerInteractionResponse {
                        interaction_id: iid,
                        status: InteractionStatus::Resolved,
                        session_revision: new_rev,
                        interaction_revision: new_int_rev,
                        head_cursor: Cursor(new_rev),
                    };
                    Ok((resp, Some(ev)))
                }
                InteractionStatus::Resolved => {
                    // Driver emitted `InteractionResolved` concurrently while `answer_interaction`
                    // returned Ok(()): record answer_json if missing without double-emitting.
                    tx.execute(
                        "UPDATE interactions
                         SET decision_id = COALESCE(decision_id, ?1),
                             answer_json = COALESCE(answer_json, ?2)
                         WHERE session_id = ?3 AND interaction_id = ?4",
                        params![req.decision_id, answer_json, sid, iid],
                    )
                    .map_err(db_err)?;
                    tx.commit().map_err(db_err)?;
                    Ok((
                        AnswerInteractionResponse {
                            interaction_id: iid,
                            status: InteractionStatus::Resolved,
                            session_revision: session.snapshot.revision,
                            interaction_revision: session.snapshot.interaction_revision,
                            head_cursor: session.snapshot.head_cursor,
                        },
                        None,
                    ))
                }
                InteractionStatus::Expired => {
                    tracing::warn!(
                        session_id = %sid,
                        interaction_id = %iid,
                        "interaction expired before post-delivery commit"
                    );
                    Err(CoreError::snapshot_conflict(format!(
                        "interaction '{iid}' expired before commit"
                    )))
                }
            }
        })
        .await
    }

    pub(crate) async fn list_interactions(
        &self,
        session_id: &str,
        status_filter: Option<InteractionStatus>,
    ) -> Result<InteractionList, CoreError> {
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let session = load_session_record_tx(&tx, &sid)?;
            let mut sql = String::from(
                "SELECT interaction_id, turn_id, session_generation, interaction_revision,
                        session_revision, kind_json, original_kind, title, decisions_json,
                        protected, response_schema_json, max_response_bytes, status
                 FROM interactions WHERE session_id = ?1",
            );
            let mut params_vec: Vec<String> = vec![sid.clone()];
            if let Some(st) = status_filter {
                sql.push_str(" AND status = ?2");
                params_vec.push(st.as_str().to_owned());
            }
            sql.push_str(" ORDER BY interaction_revision ASC, interaction_id ASC");

            let mut stmt = tx.prepare(&sql).map_err(db_err)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, i64>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<i64>>(11)?,
                        r.get::<_, String>(12)?,
                    ))
                })
                .map_err(db_err)?;

            let mut interactions = Vec::new();
            for row in rows {
                let (
                    interaction_id,
                    turn_id,
                    session_generation,
                    interaction_revision,
                    session_revision,
                    kind_json,
                    original_kind,
                    title,
                    decisions_json,
                    protected,
                    response_schema_json,
                    max_response_bytes,
                    status_str,
                ) = row.map_err(db_err)?;

                let kind: InteractionKind = serde_json::from_str(&kind_json).map_err(de_err)?;
                let decisions: Vec<DecisionOption> =
                    serde_json::from_str(&decisions_json).map_err(de_err)?;
                let response_schema = response_schema_json
                    .map(|s| serde_json::from_str(&s).map_err(de_err))
                    .transpose()?;
                interactions.push(InteractionRecord {
                    interaction_id,
                    turn_id,
                    session_generation: session_generation as u64,
                    interaction_revision: interaction_revision as u64,
                    session_revision: session_revision as u64,
                    kind,
                    original_kind,
                    title,
                    decisions,
                    protected: protected != 0,
                    response_schema,
                    max_response_bytes: max_response_bytes.and_then(|v| usize::try_from(v).ok()),
                    status: InteractionStatus::parse_str(&status_str)?,
                });
            }

            Ok(InteractionList {
                session_revision: session.snapshot.revision,
                interaction_revision: session.snapshot.interaction_revision,
                head_cursor: session.snapshot.head_cursor,
                interactions,
            })
        })
        .await
    }

    pub(crate) async fn replay_events(
        &self,
        session_id: &str,
        after: Cursor,
    ) -> Result<ReplayResult, CoreError> {
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            let current_rev = rec.snapshot.revision;
            let earliest = rec.earliest_retained_cursor;

            if (earliest > 0 && after.0.saturating_add(1) < earliest) || after.0 > current_rev {
                return Ok(ReplayResult::RefreshRequired {
                    scopes: default_refresh_scopes(),
                });
            }

            let mut stmt = tx
                .prepare(
                    "SELECT cursor, event_type, turn_id, payload_json, created_at
                     FROM session_events
                     WHERE session_id = ?1 AND cursor > ?2
                     ORDER BY cursor ASC",
                )
                .map_err(db_err)?;

            let rows = stmt
                .query_map(params![sid, after.0 as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(db_err)?;

            let mut events = Vec::new();
            for row in rows {
                let (cursor, event_type, turn_id, payload_json, created_at) =
                    row.map_err(db_err)?;
                let payload: Value = serde_json::from_str(&payload_json).map_err(de_err)?;
                events.push(SessionEvent {
                    cursor: Some(Cursor(cursor as u64)),
                    event_type,
                    turn_id,
                    created_at,
                    payload,
                });
            }
            Ok(ReplayResult::Events(events))
        })
        .await
    }

    pub(crate) async fn list_timeline(
        &self,
        session_id: &str,
        page_token: Option<String>,
        limit: usize,
    ) -> Result<TimelinePage, CoreError> {
        let sid = session_id.to_owned();
        let after_cursor = parse_page_token(page_token.as_deref())?;
        let capped_limit = limit.clamp(1, 500);

        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            let fetch_limit = (capped_limit + 1) as i64;

            let mut stmt = tx
                .prepare(
                    "SELECT cursor, turn_id, event_type, created_at, inline_text,
                            payload_json, artifact_json
                     FROM session_events
                     WHERE session_id = ?1 AND cursor > ?2
                     ORDER BY cursor ASC
                     LIMIT ?3",
                )
                .map_err(db_err)?;

            let rows = stmt
                .query_map(params![sid, after_cursor as i64, fetch_limit], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, Option<String>>(6)?,
                    ))
                })
                .map_err(db_err)?;

            let mut items = Vec::new();
            let mut accumulated_bytes = 512usize;
            let mut has_more = false;

            for row in rows {
                let (cursor, turn_id, kind, created_at, inline_text, payload_json, artifact_json) =
                    row.map_err(db_err)?;
                if items.len() >= capped_limit {
                    has_more = true;
                    break;
                }
                let payload: Value = serde_json::from_str(&payload_json).map_err(de_err)?;
                let artifact: Option<ArtifactRef> = artifact_json
                    .map(|s| serde_json::from_str(&s).map_err(de_err))
                    .transpose()?;
                let item = TimelineItem {
                    cursor: Cursor(cursor as u64),
                    turn_id,
                    kind,
                    created_at,
                    inline_text,
                    payload: Some(payload),
                    artifact,
                };
                let item_bytes = serde_json::to_vec(&item).map_err(ser_err)?.len();
                if !items.is_empty() && accumulated_bytes + item_bytes > MAX_PAGE_BYTES {
                    has_more = true;
                    break;
                }
                accumulated_bytes += item_bytes;
                items.push(item);
            }

            let next_page_token = if has_more {
                items.last().map(|it| format_page_token(it.cursor.value()))
            } else {
                None
            };

            Ok(TimelinePage {
                session_id: sid,
                revision: rec.snapshot.revision,
                head_cursor: rec.snapshot.head_cursor,
                highest_cursor: rec.snapshot.head_cursor,
                next_page_token,
                items,
            })
        })
        .await
    }

    pub(crate) async fn post_mailbox_message(
        &self,
        row: NewMailboxRow,
    ) -> Result<(MailboxMessage, SessionEvent), CoreError> {
        let max_ret = self.inner.max_retained_events;
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let _target = load_session_record_tx(&tx, &row.session_id)?;
            let now = now_rfc3339();
            let artifact_json = row
                .artifact
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(ser_err)?;
            let artifact_ids_json = serde_json::to_string(&row.artifact_ids).map_err(ser_err)?;

            let (ev, new_rev, _) = append_event_tx(
                &tx,
                max_ret,
                &NewDurableEvent {
                    session_id: row.session_id.clone(),
                    event_type: "mailbox_received".to_owned(),
                    turn_id: None,
                    inline_text: row.inline_body.clone(),
                    payload: json!({
                        "message_id": row.message_id,
                        "from_session_id": row.from_session_id,
                        "subject": row.subject,
                        "artifact_ids": row.artifact_ids,
                    }),
                    artifact: row.artifact.clone(),
                    bump_interaction_revision: false,
                },
            )?;

            tx.execute(
                "INSERT INTO mailbox_messages (
                    message_id, session_id, cursor, from_session_id, subject,
                    inline_body, artifact_json, artifact_ids_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.message_id,
                    row.session_id,
                    new_rev as i64,
                    row.from_session_id,
                    row.subject,
                    row.inline_body,
                    artifact_json,
                    artifact_ids_json,
                    now,
                ],
            )
            .map_err(db_err)?;

            tx.commit().map_err(db_err)?;
            let msg = MailboxMessage {
                message_id: row.message_id,
                cursor: Cursor(new_rev),
                session_id: row.session_id,
                from_session_id: row.from_session_id,
                subject: row.subject,
                inline_body: row.inline_body,
                artifact: row.artifact,
                created_at: now,
            };
            Ok((msg, ev))
        })
        .await
    }

    pub(crate) async fn list_mailbox_messages(
        &self,
        session_id: &str,
        page_token: Option<String>,
        limit: usize,
    ) -> Result<MailboxPage, CoreError> {
        let sid = session_id.to_owned();
        let after_cursor = parse_page_token(page_token.as_deref())?;
        let capped_limit = limit.clamp(1, 500);

        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let _ = load_session_record_tx(&tx, &sid)?;

            let highest: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(cursor), 0) FROM mailbox_messages WHERE session_id = ?1",
                    params![sid],
                    |r| r.get(0),
                )
                .map_err(db_err)?;

            let fetch_limit = (capped_limit + 1) as i64;
            let mut stmt = tx
                .prepare(
                    "SELECT message_id, cursor, from_session_id, subject,
                            inline_body, artifact_json, created_at
                     FROM mailbox_messages
                     WHERE session_id = ?1 AND cursor > ?2
                     ORDER BY cursor ASC
                     LIMIT ?3",
                )
                .map_err(db_err)?;

            let rows = stmt
                .query_map(params![sid, after_cursor as i64, fetch_limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                })
                .map_err(db_err)?;

            let mut messages = Vec::new();
            let mut accumulated_bytes = 512usize;
            let mut has_more = false;

            for row in rows {
                let (
                    message_id,
                    cursor,
                    from_session_id,
                    subject,
                    inline_body,
                    artifact_json,
                    created_at,
                ) = row.map_err(db_err)?;

                if messages.len() >= capped_limit {
                    has_more = true;
                    break;
                }
                let artifact: Option<ArtifactRef> = artifact_json
                    .map(|s| serde_json::from_str(&s).map_err(de_err))
                    .transpose()?;
                let msg = MailboxMessage {
                    message_id,
                    cursor: Cursor(cursor as u64),
                    session_id: sid.clone(),
                    from_session_id,
                    subject,
                    inline_body,
                    artifact,
                    created_at,
                };
                let msg_bytes = serde_json::to_vec(&msg).map_err(ser_err)?.len();
                if !messages.is_empty() && accumulated_bytes + msg_bytes > MAX_PAGE_BYTES {
                    has_more = true;
                    break;
                }
                accumulated_bytes += msg_bytes;
                messages.push(msg);
            }

            let next_page_token = if has_more {
                messages.last().map(|m| format_page_token(m.cursor.value()))
            } else {
                None
            };

            Ok(MailboxPage {
                highest_cursor: Cursor(highest as u64),
                next_page_token,
                messages,
            })
        })
        .await
    }

    pub(crate) async fn record_artifact(
        &self,
        art: ArtifactRef,
        root_session_id: Option<String>,
    ) -> Result<(), CoreError> {
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let now = now_rfc3339();
            tx.execute(
                "INSERT OR IGNORE INTO artifacts (
                    artifact_id, sha256, media_type, byte_len, root_session_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    art.artifact_id,
                    art.sha256,
                    art.media_type,
                    art.byte_len as i64,
                    root_session_id,
                    now,
                ],
            )
            .map_err(db_err)?;

            if let Some(root) = root_session_id {
                tx.execute(
                    "INSERT OR IGNORE INTO artifact_scopes (artifact_id, root_session_id)
                     VALUES (?1, ?2)",
                    params![art.artifact_id, root],
                )
                .map_err(db_err)?;
            }
            tx.commit().map_err(db_err)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn grant_artifact_scope(
        &self,
        artifact_id: &str,
        root_session_id: &str,
    ) -> Result<(), CoreError> {
        let aid = artifact_id.to_owned();
        let root = root_session_id.to_owned();
        self.call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO artifact_scopes (artifact_id, root_session_id)
                 VALUES (?1, ?2)",
                params![aid, root],
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn get_artifact_meta(
        &self,
        artifact_id: &str,
        required_root_session_id: Option<&str>,
    ) -> Result<ArtifactMeta, CoreError> {
        let aid = artifact_id.to_owned();
        let req_root = required_root_session_id.map(str::to_owned);
        self.call(move |conn| {
            let row: Option<(String, String, i64)> = conn
                .query_row(
                    "SELECT sha256, media_type, byte_len FROM artifacts WHERE artifact_id = ?1",
                    params![aid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(db_err)?;

            let Some((sha256, media_type, byte_len)) = row else {
                return Err(CoreError::not_found(format!("artifact '{aid}' not found")));
            };

            if let Some(root) = req_root {
                let allowed: Option<i64> = conn
                    .query_row(
                        "SELECT 1 FROM artifact_scopes
                         WHERE artifact_id = ?1 AND root_session_id = ?2",
                        params![aid, root],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(db_err)?;
                if allowed.is_none() {
                    return Err(CoreError::forbidden_scope(format!(
                        "artifact '{aid}' does not belong to actor session tree"
                    )));
                }
            }

            Ok(ArtifactMeta {
                artifact_ref: ArtifactRef {
                    artifact_id: aid,
                    sha256,
                    media_type,
                    byte_len: byte_len as u64,
                },
            })
        })
        .await
    }

    pub(crate) async fn mint_actor_token_for_session(
        &self,
        session_id: &str,
    ) -> Result<Zeroizing<String>, CoreError> {
        let (raw_token, token_hash) = generate_actor_token()?;
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            if rec.snapshot.status != SessionStatus::Active {
                return Err(CoreError::snapshot_conflict(
                    "cannot mint actor token for non-active session",
                ));
            }
            tx.execute(
                "INSERT INTO actor_tokens (token_hash, session_id, root_session_id, created_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![token_hash, sid, rec.snapshot.root_session_id, now_rfc3339()],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            Ok(())
        })
        .await?;
        Ok(raw_token)
    }

    pub(crate) async fn verify_actor_token(
        &self,
        raw_token: &str,
    ) -> Result<ActorScope, CoreError> {
        if raw_token.is_empty() {
            return Err(CoreError::unauthorized("missing actor token"));
        }
        let candidate_hash = sha256_hex(raw_token.as_bytes());
        self.call(move |conn| {
            struct TokenRow {
                stored_hash: String,
                session_id: String,
                token_root: String,
                revoked_at: Option<String>,
                session_root: String,
                access_str: String,
                workspace: String,
                status_str: String,
            }

            let row: Option<TokenRow> = conn
                .query_row(
                    "SELECT t.token_hash, t.session_id, t.root_session_id, t.revoked_at,
                            s.root_session_id, s.access, s.workspace, s.status
                     FROM actor_tokens t
                     JOIN sessions s ON s.session_id = t.session_id
                     WHERE t.token_hash = ?1",
                    params![candidate_hash],
                    |r| {
                        Ok(TokenRow {
                            stored_hash: r.get(0)?,
                            session_id: r.get(1)?,
                            token_root: r.get(2)?,
                            revoked_at: r.get(3)?,
                            session_root: r.get(4)?,
                            access_str: r.get(5)?,
                            workspace: r.get(6)?,
                            status_str: r.get(7)?,
                        })
                    },
                )
                .optional()
                .map_err(db_err)?;

            let Some(TokenRow {
                stored_hash,
                session_id,
                token_root,
                revoked_at,
                session_root,
                access_str,
                workspace,
                status_str,
            }) = row
            else {
                return Err(CoreError::unauthorized("invalid actor token"));
            };

            if !constant_time_eq(stored_hash.as_bytes(), candidate_hash.as_bytes()) {
                return Err(CoreError::unauthorized("invalid actor token"));
            }
            if revoked_at.is_some() {
                return Err(CoreError::unauthorized("actor token has been revoked"));
            }
            if token_root != session_root {
                return Err(CoreError::unauthorized(
                    "actor token scope binding mismatch",
                ));
            }
            let status = SessionStatus::parse_str(&status_str)?;
            if status != SessionStatus::Active {
                return Err(CoreError::unauthorized("actor token session is not active"));
            }
            let access = parse_access(&access_str)?;
            Ok(ActorScope {
                session_id,
                root_session_id: token_root,
                access,
                workspace: PathBuf::from(workspace),
            })
        })
        .await
    }

    pub(crate) async fn get_waiter_mailbox_cursor(
        &self,
        waiter_session_id: &str,
    ) -> Result<u64, CoreError> {
        let sid = waiter_session_id.to_owned();
        self.call(move |conn| {
            let max_cursor: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(cursor), 0) FROM mailbox_messages WHERE session_id = ?1",
                    params![sid],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            Ok(max_cursor as u64)
        })
        .await
    }

    pub(crate) async fn get_wait_target_states(
        &self,
        session_ids: Vec<String>,
        waiter_and_baseline: Option<(String, u64)>,
    ) -> Result<Vec<WaitTargetState>, CoreError> {
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let mut out = Vec::with_capacity(session_ids.len());
            for sid in session_ids {
                let rec = load_session_record_tx(&tx, &sid)?;
                let completed: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND status NOT IN ('running', 'reserved')",
                        params![sid],
                        |r| r.get(0),
                    )
                    .map_err(db_err)?;
                let has_new_mailbox_for_waiter = match &waiter_and_baseline {
                    Some((waiter_sid, baseline)) => tx
                        .query_row(
                            "SELECT 1 FROM mailbox_messages
                             WHERE session_id = ?1 AND from_session_id = ?2 AND cursor > ?3
                             LIMIT 1",
                            params![waiter_sid, sid, *baseline as i64],
                            |r| r.get::<_, i64>(0),
                        )
                        .optional()
                        .map_err(db_err)?
                        .is_some(),
                    None => false,
                };
                out.push(WaitTargetState {
                    snapshot: rec.snapshot,
                    completed_turn_count: completed as u64,
                    has_new_mailbox_for_waiter,
                });
            }
            Ok(out)
        })
        .await
    }

    pub(crate) async fn check_writer_lease_conflict(
        &self,
        workspace: &str,
        candidate_session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let ws = workspace.to_owned();
        let cand = candidate_session_id.map(str::to_owned);
        self.call(move |conn| {
            let holder: Option<String> = conn
                .query_row(
                    "SELECT holder_session_id FROM writer_leases WHERE workspace = ?1",
                    params![ws],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            if let Some(holder_id) = holder
                && cand.as_deref() != Some(holder_id.as_str())
            {
                return Err(CoreError::snapshot_conflict(format!(
                    "workspace '{ws}' writer lease is currently held by session '{holder_id}'"
                )));
            }
            Ok(())
        })
        .await
    }

    pub(crate) async fn acquire_writer_lease(
        &self,
        workspace: &str,
        session_id: &str,
    ) -> Result<WriterLease, CoreError> {
        let ws = workspace.to_owned();
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let rec = load_session_record_tx(&tx, &sid)?;
            if matches!(
                rec.snapshot.status,
                SessionStatus::Closed | SessionStatus::Failed
            ) {
                return Err(CoreError::snapshot_conflict(
                    "closed or failed session cannot acquire a writer lease",
                ));
            }
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT holder_session_id, acquired_at FROM writer_leases WHERE workspace = ?1",
                    params![ws],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_err)?;

            if let Some((holder, acquired_at)) = existing {
                if holder == sid {
                    return Ok(WriterLease {
                        workspace: ws,
                        holder_session_id: holder,
                        acquired_at,
                    });
                }
                return Err(CoreError::snapshot_conflict(format!(
                    "workspace '{ws}' writer lease is already held by session '{holder}'"
                )));
            }

            let now = now_rfc3339();
            tx.execute(
                "INSERT INTO writer_leases (workspace, holder_session_id, acquired_at)
                 VALUES (?1, ?2, ?3)",
                params![ws, sid, now],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            Ok(WriterLease {
                workspace: ws,
                holder_session_id: sid,
                acquired_at: now,
            })
        })
        .await
    }

    pub(crate) async fn transfer_writer_lease(
        &self,
        workspace: &str,
        from_session_id: &str,
        to_session_id: &str,
    ) -> Result<WriterLease, CoreError> {
        let ws = workspace.to_owned();
        let from_sid = from_session_id.to_owned();
        let to_sid = to_session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let target = load_session_record_tx(&tx, &to_sid)?;
            if matches!(
                target.snapshot.status,
                SessionStatus::Closed | SessionStatus::Failed
            ) {
                return Err(CoreError::snapshot_conflict(
                    "cannot transfer writer lease to a closed or failed session",
                ));
            }

            let holder: Option<String> = tx
                .query_row(
                    "SELECT holder_session_id FROM writer_leases WHERE workspace = ?1",
                    params![ws],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            match holder {
                Some(h) if h == from_sid => {
                    let now = now_rfc3339();
                    tx.execute(
                        "UPDATE writer_leases SET holder_session_id = ?1, acquired_at = ?2
                         WHERE workspace = ?3",
                        params![to_sid, now, ws],
                    )
                    .map_err(db_err)?;
                    tx.commit().map_err(db_err)?;
                    Ok(WriterLease {
                        workspace: ws,
                        holder_session_id: to_sid,
                        acquired_at: now,
                    })
                }
                Some(other) => Err(CoreError::snapshot_conflict(format!(
                    "workspace '{ws}' writer lease is held by '{other}', not '{from_sid}'"
                ))),
                None => Err(CoreError::snapshot_conflict(format!(
                    "workspace '{ws}' has no active writer lease to transfer"
                ))),
            }
        })
        .await
    }

    pub(crate) async fn release_writer_lease(
        &self,
        workspace: &str,
        session_id: &str,
    ) -> Result<bool, CoreError> {
        let ws = workspace.to_owned();
        let sid = session_id.to_owned();
        self.call(move |conn| {
            let tx = conn.transaction().map_err(db_err)?;
            let holder: Option<String> = tx
                .query_row(
                    "SELECT holder_session_id FROM writer_leases WHERE workspace = ?1",
                    params![ws],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_err)?;

            match holder {
                Some(h) if h == sid => {
                    tx.execute(
                        "DELETE FROM writer_leases WHERE workspace = ?1",
                        params![ws],
                    )
                    .map_err(db_err)?;
                    tx.commit().map_err(db_err)?;
                    Ok(true)
                }
                Some(other) => Err(CoreError::snapshot_conflict(format!(
                    "workspace '{ws}' writer lease is held by '{other}', not '{sid}'"
                ))),
                None => Ok(false),
            }
        })
        .await
    }
}

pub(crate) fn generate_actor_token() -> Result<(Zeroizing<String>, String), CoreError> {
    let mut raw = Zeroizing::new([0u8; 32]);
    let mut file = File::open("/dev/urandom")
        .map_err(|e| CoreError::internal(format!("failed to open /dev/urandom: {e}")))?;
    file.read_exact(raw.as_mut())
        .map_err(|e| CoreError::internal(format!("failed to read random token bytes: {e}")))?;
    let hex_token = Zeroizing::new(hex_lower(raw.as_ref()));
    let hash = sha256_hex(hex_token.as_bytes());
    Ok((hex_token, hash))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn open_and_init_db(db_path: &Path) -> Result<Connection, CoreError> {
    let conn = Connection::open(db_path).map_err(db_err)?;
    chmod_0600_if_exists(db_path)?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(db_err)?;

    chmod_0600_if_exists(db_path)?;
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    chmod_0600_if_exists(&wal_path)?;
    chmod_0600_if_exists(&shm_path)?;

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db_err)?;

    if version == 0 {
        conn.execute_batch(SCHEMA_SQL).map_err(db_err)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .map_err(db_err)?;
    } else if version > SCHEMA_VERSION {
        return Err(CoreError::internal(format!(
            "unsupported state.db schema version {version} (max supported {SCHEMA_VERSION})"
        )));
    }

    Ok(conn)
}

fn chmod_0600_if_exists(path: &Path) -> Result<(), CoreError> {
    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn expire_pending_interactions_tx(
    tx: &Transaction<'_>,
    max_retained_events: u64,
    session_id: &str,
    now: &str,
    reason: &str,
) -> Result<Vec<SessionEvent>, CoreError> {
    let pending: Vec<(String, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT interaction_id, turn_id FROM interactions
                 WHERE session_id = ?1 AND status = 'pending'
                 ORDER BY interaction_revision ASC, interaction_id ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
    };

    if pending.is_empty() {
        return Ok(Vec::new());
    }

    tx.execute(
        "UPDATE interactions SET status = 'expired', resolved_at = ?1
         WHERE session_id = ?2 AND status = 'pending'",
        params![now, session_id],
    )
    .map_err(db_err)?;

    let mut emitted = Vec::with_capacity(pending.len());
    for (interaction_id, turn_id) in pending {
        let (ev, _, _) = append_event_tx(
            tx,
            max_retained_events,
            &NewDurableEvent {
                session_id: session_id.to_owned(),
                event_type: "interaction_expired".to_owned(),
                turn_id: Some(turn_id),
                inline_text: None,
                payload: json!({
                    "interaction_id": interaction_id,
                    "reason": reason,
                }),
                artifact: None,
                bump_interaction_revision: true,
            },
        )?;
        emitted.push(ev);
    }
    Ok(emitted)
}

fn append_event_tx(
    tx: &Transaction<'_>,
    max_retained_events: u64,
    event: &NewDurableEvent,
) -> Result<(SessionEvent, u64, u64), CoreError> {
    let (rev, int_rev, earliest): (i64, i64, i64) = tx
        .query_row(
            "SELECT revision, interaction_revision, earliest_retained_cursor
             FROM sessions WHERE session_id = ?1",
            params![event.session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| CoreError::not_found(format!("session '{}' not found", event.session_id)))?;

    let new_rev = (rev as u64) + 1;
    let new_int_rev = if event.bump_interaction_revision {
        new_rev
    } else {
        int_rev as u64
    };
    let created_at = now_rfc3339();
    let payload_json = serde_json::to_string(&event.payload).map_err(ser_err)?;
    let artifact_json = event
        .artifact
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(ser_err)?;

    tx.execute(
        "INSERT INTO session_events (
            session_id, cursor, event_type, turn_id,
            inline_text, payload_json, artifact_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event.session_id,
            new_rev as i64,
            event.event_type,
            event.turn_id,
            event.inline_text,
            payload_json,
            artifact_json,
            created_at,
        ],
    )
    .map_err(db_err)?;

    let mut new_earliest = earliest as u64;
    if new_earliest == 0 {
        new_earliest = new_rev;
    }
    if max_retained_events > 0 && new_rev > max_retained_events {
        let prune_up_to = new_rev - max_retained_events;
        tx.execute(
            "DELETE FROM session_events WHERE session_id = ?1 AND cursor <= ?2",
            params![event.session_id, prune_up_to as i64],
        )
        .map_err(db_err)?;
        new_earliest = new_earliest.max(prune_up_to + 1);
    }

    tx.execute(
        "UPDATE sessions
         SET revision = ?1,
             interaction_revision = ?2,
             earliest_retained_cursor = ?3,
             updated_at = ?4
         WHERE session_id = ?5",
        params![
            new_rev as i64,
            new_int_rev as i64,
            new_earliest as i64,
            created_at,
            event.session_id,
        ],
    )
    .map_err(db_err)?;

    let emitted = SessionEvent {
        cursor: Some(Cursor(new_rev)),
        event_type: event.event_type.clone(),
        turn_id: event.turn_id.clone(),
        created_at,
        payload: event.payload.clone(),
    };
    Ok((emitted, new_rev, new_int_rev))
}

fn validate_interaction_answer_tx(
    tx: &Transaction<'_>,
    sid: &str,
    iid: &str,
    req: &AnswerInteractionRequest,
) -> Result<String, CoreError> {
    if req.turn_id.trim().is_empty() || req.session_generation == 0 {
        return Err(CoreError::bad_request(
            "turn_id must be non-empty and session_generation must be >= 1",
        ));
    }
    if req.decision_id.trim().is_empty() {
        return Err(CoreError::bad_request("decision_id must not be empty"));
    }

    let session = load_session_record_tx(tx, sid)?;
    if session.snapshot.status != SessionStatus::Active {
        return Err(CoreError::snapshot_conflict(
            "cannot answer interaction on non-active session",
        ));
    }

    let row: Option<(String, i64, String, Option<i64>)> = tx
        .query_row(
            "SELECT turn_id, session_generation, status, max_response_bytes
             FROM interactions WHERE session_id = ?1 AND interaction_id = ?2",
            params![sid, iid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(db_err)?;

    let Some((stored_turn_id, stored_gen, status_str, max_resp)) = row else {
        return Err(CoreError::not_found(format!(
            "interaction '{iid}' not found on session '{sid}'"
        )));
    };

    let status = InteractionStatus::parse_str(&status_str)?;
    if status != InteractionStatus::Pending {
        return Err(CoreError::snapshot_conflict(format!(
            "interaction '{iid}' is already {}",
            status.as_str()
        )));
    }
    if req.turn_id != stored_turn_id {
        return Err(CoreError::snapshot_conflict(format!(
            "turn_id mismatch: expected '{stored_turn_id}', got '{}'",
            req.turn_id
        )));
    }
    let stored_gen_u64 = stored_gen as u64;
    if req.session_generation != session.snapshot.session_generation
        || req.session_generation != stored_gen_u64
    {
        return Err(CoreError::snapshot_conflict(format!(
            "session_generation mismatch: current {}, got {}",
            session.snapshot.session_generation, req.session_generation
        )));
    }
    if req.expected_interaction_revision != session.snapshot.interaction_revision {
        return Err(CoreError::snapshot_conflict(format!(
            "expected_interaction_revision mismatch: current {}, got {}",
            session.snapshot.interaction_revision, req.expected_interaction_revision
        )));
    }
    if req.expected_session_revision != session.snapshot.revision {
        return Err(CoreError::snapshot_conflict(format!(
            "expected_session_revision mismatch: current {}, got {}",
            session.snapshot.revision, req.expected_session_revision
        )));
    }

    let answer_blob = serde_json::to_vec(&json!({
        "decision_id": req.decision_id,
        "answers": req.answers,
        "payload": req.payload,
    }))
    .map_err(ser_err)?;
    let limit = max_resp
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(MAX_INLINE_BYTES);
    if answer_blob.len() > limit {
        return Err(CoreError::payload_too_large(format!(
            "interaction response ({} bytes) exceeds limit ({limit} bytes)",
            answer_blob.len()
        )));
    }
    String::from_utf8(answer_blob).map_err(|_| CoreError::internal("non-utf8 serialized json"))
}

fn load_session_record_tx(
    tx: &Transaction<'_>,
    session_id: &str,
) -> Result<SessionRecord, CoreError> {
    struct SessionDbRow {
        session_id: String,
        root_session_id: String,
        parent_session_id: Option<String>,
        depth: i64,
        max_delegation_depth: i64,
        allow_actor_collaboration: i64,
        allow_delegation: i64,
        role: Option<String>,
        role_instructions: Option<String>,
        driver: String,
        provider_session_id: String,
        model: Option<String>,
        effort: Option<String>,
        workspace: String,
        status_str: String,
        access_str: String,
        approval_str: String,
        output_schema_json: Option<String>,
        session_generation: i64,
        revision: i64,
        interaction_revision: i64,
        earliest_retained_cursor: i64,
        active_turn_id: Option<String>,
    }

    let row = tx
        .query_row(
            "SELECT session_id, root_session_id, parent_session_id, depth,
                    max_delegation_depth, allow_actor_collaboration, allow_delegation,
                    role, role_instructions, driver, provider_session_id,
                    model, effort, workspace, status, access, approval_policy,
                    output_schema_json, session_generation, revision,
                    interaction_revision, earliest_retained_cursor, active_turn_id
             FROM sessions WHERE session_id = ?1",
            params![session_id],
            |r| {
                Ok(SessionDbRow {
                    session_id: r.get(0)?,
                    root_session_id: r.get(1)?,
                    parent_session_id: r.get(2)?,
                    depth: r.get(3)?,
                    max_delegation_depth: r.get(4)?,
                    allow_actor_collaboration: r.get(5)?,
                    allow_delegation: r.get(6)?,
                    role: r.get(7)?,
                    role_instructions: r.get(8)?,
                    driver: r.get(9)?,
                    provider_session_id: r.get(10)?,
                    model: r.get(11)?,
                    effort: r.get(12)?,
                    workspace: r.get(13)?,
                    status_str: r.get(14)?,
                    access_str: r.get(15)?,
                    approval_str: r.get(16)?,
                    output_schema_json: r.get(17)?,
                    session_generation: r.get(18)?,
                    revision: r.get(19)?,
                    interaction_revision: r.get(20)?,
                    earliest_retained_cursor: r.get(21)?,
                    active_turn_id: r.get(22)?,
                })
            },
        )
        .optional()
        .map_err(db_err)?;

    let Some(r) = row else {
        return Err(CoreError::not_found(format!(
            "session '{session_id}' not found"
        )));
    };

    let pending_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM interactions WHERE session_id = ?1 AND status = 'pending'",
            params![r.session_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;

    let output_schema = r
        .output_schema_json
        .map(|s| serde_json::from_str(&s).map_err(de_err))
        .transpose()?;
    let rev_u64 = r.revision as u64;

    Ok(SessionRecord {
        snapshot: SessionSnapshot {
            session_id: r.session_id,
            root_session_id: r.root_session_id,
            parent_session_id: r.parent_session_id,
            depth: r.depth as u8,
            policy: SessionPolicy {
                max_delegation_depth: r.max_delegation_depth as u8,
                allow_actor_collaboration: r.allow_actor_collaboration != 0,
            },
            role: r.role,
            driver: r.driver,
            model: r.model,
            effort: r.effort,
            workspace: r.workspace,
            status: SessionStatus::parse_str(&r.status_str)?,
            access: parse_access(&r.access_str)?,
            approval_policy: parse_approval(&r.approval_str)?,
            session_generation: r.session_generation as u64,
            revision: rev_u64,
            head_cursor: Cursor(rev_u64),
            interaction_revision: r.interaction_revision as u64,
            active_turn_id: r.active_turn_id,
            pending_interaction_count: pending_count as u64,
        },
        allow_delegation: r.allow_delegation != 0,
        provider_session_id: r.provider_session_id,
        role_instructions: r.role_instructions,
        output_schema,
        earliest_retained_cursor: r.earliest_retained_cursor as u64,
    })
}

fn default_refresh_scopes() -> Vec<String> {
    vec![
        "session".to_owned(),
        "interaction".to_owned(),
        "timeline".to_owned(),
        "artifacts".to_owned(),
        "mailbox".to_owned(),
    ]
}

fn format_page_token(cursor: u64) -> String {
    format!("tok:{cursor}")
}

fn parse_page_token(token: Option<&str>) -> Result<u64, CoreError> {
    let Some(raw) = token else {
        return Ok(0);
    };
    let Some(digits) = raw.strip_prefix("tok:") else {
        return Err(CoreError::bad_request(format!(
            "invalid page_token '{raw}'"
        )));
    };
    Ok(Cursor::parse(digits)?.value())
}

fn access_to_str(access: AccessMode) -> &'static str {
    match access {
        AccessMode::ReadOnly => "read_only",
        AccessMode::WorkspaceWrite => "workspace_write",
    }
}

fn parse_access(s: &str) -> Result<AccessMode, CoreError> {
    match s {
        "read_only" => Ok(AccessMode::ReadOnly),
        "workspace_write" => Ok(AccessMode::WorkspaceWrite),
        other => Err(CoreError::internal(format!(
            "invalid access mode '{other}'"
        ))),
    }
}

fn approval_to_str(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Interactive => "interactive",
        ApprovalPolicy::AutoDeny => "auto_deny",
        ApprovalPolicy::AutoApproveScoped => "auto_approve_scoped",
    }
}

fn parse_approval(s: &str) -> Result<ApprovalPolicy, CoreError> {
    match s {
        "interactive" => Ok(ApprovalPolicy::Interactive),
        "auto_deny" => Ok(ApprovalPolicy::AutoDeny),
        "auto_approve_scoped" => Ok(ApprovalPolicy::AutoApproveScoped),
        other => Err(CoreError::internal(format!(
            "invalid approval policy '{other}'"
        ))),
    }
}

fn turn_status_to_str(status: TurnTerminalStatus) -> &'static str {
    match status {
        TurnTerminalStatus::Completed => "completed",
        TurnTerminalStatus::Interrupted => "interrupted",
        TurnTerminalStatus::Failed => "failed",
        TurnTerminalStatus::Unknown => "unknown",
    }
}

fn db_err(err: rusqlite::Error) -> CoreError {
    CoreError::internal(format!("sqlite error: {err}"))
}

fn ser_err(err: serde_json::Error) -> CoreError {
    CoreError::internal(format!("json serialization error: {err}"))
}

fn de_err(err: serde_json::Error) -> CoreError {
    CoreError::internal(format!("json deserialization error: {err}"))
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    root_session_id TEXT NOT NULL,
    parent_session_id TEXT,
    depth INTEGER NOT NULL,
    max_delegation_depth INTEGER NOT NULL,
    allow_actor_collaboration INTEGER NOT NULL,
    allow_delegation INTEGER NOT NULL DEFAULT 0,
    role TEXT,
    role_snapshot_sha256 TEXT,
    role_instructions TEXT,
    driver TEXT NOT NULL,
    provider_session_id TEXT NOT NULL,
    model TEXT,
    effort TEXT,
    workspace TEXT NOT NULL,
    status TEXT NOT NULL,
    access TEXT NOT NULL,
    approval_policy TEXT NOT NULL,
    output_schema_json TEXT,
    session_generation INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    interaction_revision INTEGER NOT NULL,
    earliest_retained_cursor INTEGER NOT NULL,
    active_turn_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_root ON sessions(root_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);

CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT NOT NULL,
    cursor INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    turn_id TEXT,
    inline_text TEXT,
    payload_json TEXT NOT NULL,
    artifact_json TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (session_id, cursor),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS turns (
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    session_generation INTEGER NOT NULL,
    status TEXT NOT NULL,
    input_text TEXT,
    input_artifact_json TEXT,
    output_schema_json TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (session_id, turn_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS interactions (
    session_id TEXT NOT NULL,
    interaction_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    session_generation INTEGER NOT NULL,
    interaction_revision INTEGER NOT NULL,
    session_revision INTEGER NOT NULL,
    kind_json TEXT NOT NULL,
    original_kind TEXT NOT NULL,
    title TEXT NOT NULL,
    decisions_json TEXT NOT NULL,
    protected INTEGER NOT NULL,
    response_schema_json TEXT,
    max_response_bytes INTEGER,
    status TEXT NOT NULL,
    decision_id TEXT,
    answer_json TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    PRIMARY KEY (session_id, interaction_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_interactions_status ON interactions(session_id, status);

CREATE TABLE IF NOT EXISTS mailbox_messages (
    message_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    cursor INTEGER NOT NULL,
    from_session_id TEXT,
    subject TEXT,
    inline_body TEXT,
    artifact_json TEXT,
    artifact_ids_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_mailbox_session_cursor ON mailbox_messages(session_id, cursor);

CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_len INTEGER NOT NULL,
    root_session_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_artifacts_sha256 ON artifacts(sha256);

CREATE TABLE IF NOT EXISTS artifact_scopes (
    artifact_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    PRIMARY KEY (artifact_id, root_session_id),
    FOREIGN KEY (artifact_id) REFERENCES artifacts(artifact_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS actor_tokens (
    token_hash TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_actor_tokens_session ON actor_tokens(session_id);

CREATE TABLE IF NOT EXISTS writer_leases (
    workspace TEXT PRIMARY KEY,
    holder_session_id TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    FOREIGN KEY (holder_session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
";
