//! Live ACP stdio session management, concurrent NDJSON routing, and PGID teardown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use dolgorae_spi::{
    AccessMode, ApprovalPolicy, AutoDecision, DriverError, DriverSession, EventReceiver,
    EventSender, InteractionAnswer, InteractionRequest, NormalizedEvent, SessionSpec, TurnAck,
    TurnInput, TurnTerminalStatus, evaluate_interaction, event_channel,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use uuid::Uuid;

use crate::protocol::{
    AgentSessionCaps, IncomingMessage, build_mcp_servers, build_session_meta,
    build_turn_prompt_text, extract_usage_metrics, map_session_update, map_stop_reason,
    parse_incoming, parse_permission_request, parse_session_caps, perm_outcome, resolve_abs_cwd,
    rpc_id_key, select_readonly_mode,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_RPC_TIMEOUT: Duration = Duration::from_millis(500);
const TERM_GRACE_PERIOD: Duration = Duration::from_millis(500);
const PGID_POLL_INTERVAL: Duration = Duration::from_millis(25);

struct PendingInteraction {
    interaction_id: String,
    turn_id: String,
    native_rpc_id: Value,
    option_by_decision: HashMap<String, String>,
}

struct ActiveTurn {
    turn_id: String,
    prompt_rpc_key: String,
    accumulated_text: String,
}

type RpcWaiter = oneshot::Sender<Result<Value, DriverError>>;

struct SessionShared {
    stdin: AsyncMutex<Option<ChildStdin>>,
    child: AsyncMutex<Option<Child>>,
    pgid: Option<u32>,
    next_rpc_id: AtomicU64,
    rpc_waiters: StdMutex<HashMap<String, RpcWaiter>>,
    pending: StdMutex<HashMap<String, PendingInteraction>>,
    active_turn: StdMutex<Option<ActiveTurn>>,
    closed: AtomicBool,
    event_tx: EventSender,
    workspace: PathBuf,
    access: AccessMode,
    approval_policy: ApprovalPolicy,
    stderr_tail: StdMutex<String>,
}

impl SessionShared {
    fn waiters(&self) -> MutexGuard<'_, HashMap<String, RpcWaiter>> {
        self.rpc_waiters.lock().expect("rpc_waiters lock")
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<String, PendingInteraction>> {
        self.pending.lock().expect("pending lock")
    }

    fn turn(&self) -> MutexGuard<'_, Option<ActiveTurn>> {
        self.active_turn.lock().expect("active_turn lock")
    }

    async fn emit(&self, event: NormalizedEvent) {
        let _ = self.event_tx.send(event).await;
    }

    async fn reply_result(&self, id: Value, result: Value) -> Result<(), DriverError> {
        self.write_frame(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await
    }

    async fn emit_resolved(
        &self,
        interaction_id: String,
        turn_id: String,
        decision_id: Option<String>,
    ) {
        self.emit(NormalizedEvent::InteractionResolved {
            interaction_id,
            turn_id,
            decision_id,
        })
        .await;
    }

    async fn set_effort(&self, sid: &str, effort: &str) -> Result<(), DriverError> {
        let p = json!({"sessionId": sid, "configId": "thought_level", "value": effort});
        self.send_rpc("session/set_config_option", p, RPC_TIMEOUT)
            .await?;
        Ok(())
    }

    async fn write_frame(&self, frame: &Value) -> Result<(), DriverError> {
        let mut bytes = serde_json::to_vec(frame)
            .map_err(|e| DriverError::Protocol(format!("serialize json-rpc frame: {e}")))?;
        bytes.push(b'\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| DriverError::ProcessExited("session stdin is closed".to_owned()))?;
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn send_rpc(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DriverError> {
        let id = self.next_rpc_id.fetch_add(1, Ordering::Relaxed);
        let key = rpc_id_key(&json!(id));
        let (tx, rx) = oneshot::channel();
        self.waiters().insert(key.clone(), tx);

        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let fut = async {
            self.write_frame(&frame).await?;
            rx.await.map_err(|_| {
                DriverError::ProcessExited(format!("ACP process closed while awaiting '{method}'"))
            })?
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(res) => {
                if res.is_err() {
                    self.waiters().remove(&key);
                }
                res
            }
            Err(_) => {
                self.waiters().remove(&key);
                Err(DriverError::Timeout(format!(
                    "timed out awaiting '{method}'"
                )))
            }
        }
    }
}

/// Active Agy ACP session over a dedicated `--process-per-session` child.
///
/// Callers must drain `take_events()` concurrently while issuing turns or
/// answers so the bounded event channel does not apply backpressure to the
/// stdout reader. Each child runs in its own process group (`process_group(0)`),
/// and grandchildren calling `setsid(2)` escape PGID teardown.
///
/// ACP executes without an OS command sandbox (`os_sandboxed = false`):
/// `AccessMode::ReadOnly` switches the agent to an advertised read-only/ask
/// mode (`read-only`/`read_only`/`ask`/`default`, never `plan`/`auto`/`accept-edits`)
/// via `session/set_mode` (failing closed if none is advertised), hard-denies
/// `FileChangeApproval` and mode-switching requests under every policy, and
/// leaves user-level Agy permission settings ("per your permission settings")
/// as a residual risk backed by driver-side `evaluate_interaction` hard-deny.
/// `AutoApproveScoped` denies commands and never falls back to `allow_always`.
pub struct AgySession {
    provider_session_id: String,
    supports_close: bool,
    events: Option<EventReceiver>,
    shared: Arc<SessionShared>,
}

impl AgySession {
    pub(crate) async fn start(
        binary: &Path,
        spec: SessionSpec,
        resume_id: Option<String>,
    ) -> Result<Self, DriverError> {
        let mut cmd = Command::new(binary);
        cmd.args(["acp", "--stdio", "--process-per-session"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                DriverError::Unavailable(format!(
                    "agy binary '{}' not found: {err}",
                    binary.display()
                ))
            } else {
                DriverError::Io(err)
            }
        })?;

        let pgid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DriverError::Protocol("missing stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DriverError::Protocol("missing stdout".to_owned()))?;
        let stderr = child.stderr.take();

        let (event_tx, event_rx) = event_channel(EVENT_CHANNEL_CAPACITY);
        let shared = Arc::new(SessionShared {
            stdin: AsyncMutex::new(Some(stdin)),
            child: AsyncMutex::new(Some(child)),
            pgid,
            next_rpc_id: AtomicU64::new(1),
            rpc_waiters: StdMutex::new(HashMap::new()),
            pending: StdMutex::new(HashMap::new()),
            active_turn: StdMutex::new(None),
            closed: AtomicBool::new(false),
            event_tx,
            workspace: spec.workspace.clone(),
            access: spec.access,
            approval_policy: spec.approval_policy,
            stderr_tail: StdMutex::new(String::new()),
        });

        // Start stderr drainer and stdout reader BEFORE `initialize` so interleaved
        // `session/update` notifications during `session/new` never deadlock RPC waiters.
        if let Some(stderr) = stderr {
            tokio::spawn(drain_stderr(stderr, Arc::clone(&shared)));
        }
        tokio::spawn(stdout_reader_loop(stdout, Arc::clone(&shared)));

        match Self::handshake(&shared, &spec, resume_id.as_deref()).await {
            Ok((provider_session_id, caps)) => Ok(Self {
                provider_session_id,
                supports_close: caps.supports_close,
                events: Some(event_rx),
                shared,
            }),
            Err(err) => {
                teardown_process(&shared).await;
                Err(err)
            }
        }
    }

    async fn handshake(
        shared: &SessionShared,
        spec: &SessionSpec,
        resume_id: Option<&str>,
    ) -> Result<(String, AgentSessionCaps), DriverError> {
        let init_params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "dolgorae", "version": env!("CARGO_PKG_VERSION")},
        });
        let caps = parse_session_caps(
            &shared
                .send_rpc("initialize", init_params, RPC_TIMEOUT)
                .await?,
        );

        let mut params = json!({
            "cwd": resolve_abs_cwd(&spec.workspace),
            "mcpServers": build_mcp_servers(&spec.mcp_servers),
        });
        if let (Some(obj), Some(meta)) = (
            params.as_object_mut(),
            build_session_meta(
                spec.role_instructions.as_deref(),
                spec.output_schema.as_ref(),
            ),
        ) {
            obj.insert("_meta".to_owned(), meta);
        }

        let (method, fallback_sid) = match resume_id {
            Some(_) if !caps.supports_resume => {
                return Err(DriverError::Unsupported(
                    "ACP agent does not advertise sessionCapabilities.resume".to_owned(),
                ));
            }
            Some(rid) => {
                params["sessionId"] = Value::String(rid.to_owned());
                ("session/resume", Some(rid))
            }
            None => ("session/new", None),
        };
        let session_res = shared.send_rpc(method, params, RPC_TIMEOUT).await?;
        let sid = session_res
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or(fallback_sid)
            .map(str::to_owned)
            .ok_or_else(|| DriverError::Protocol("session/new missing sessionId".to_owned()))?;

        if spec.access == AccessMode::ReadOnly {
            let mode_id = select_readonly_mode(&session_res).ok_or_else(|| {
                DriverError::Unsupported(
                    "provider did not advertise a read-only or ask-before-edits mode for AccessMode::ReadOnly".to_owned(),
                )
            })?;
            shared
                .send_rpc(
                    "session/set_mode",
                    json!({"sessionId": sid, "modeId": mode_id}),
                    RPC_TIMEOUT,
                )
                .await?;
        }
        if let Some(model) = spec.model.as_deref() {
            let p = json!({"sessionId": sid, "modelId": model});
            shared.send_rpc("session/set_model", p, RPC_TIMEOUT).await?;
        }
        if let Some(effort) = spec.effort.as_deref() {
            shared.set_effort(&sid, effort).await?;
        }
        Ok((sid, caps))
    }
}

#[async_trait]
impl DriverSession for AgySession {
    fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    fn take_events(&mut self) -> Option<EventReceiver> {
        self.events.take()
    }

    async fn send_turn(&self, input: TurnInput) -> Result<TurnAck, DriverError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(DriverError::ProcessExited("session is closed".to_owned()));
        }
        if let Some(effort) = input.effort.as_deref() {
            self.shared
                .set_effort(&self.provider_session_id, effort)
                .await?;
        }

        let turn_id = Uuid::now_v7().to_string();
        let rpc_id = self.shared.next_rpc_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut active = self.shared.turn();
            if active.is_some() {
                return Err(DriverError::InvalidConfig(
                    "another turn is already active".to_owned(),
                ));
            }
            *active = Some(ActiveTurn {
                turn_id: turn_id.clone(),
                prompt_rpc_key: rpc_id_key(&json!(rpc_id)),
                accumulated_text: String::new(),
            });
        }

        self.shared
            .emit(NormalizedEvent::TurnStarted {
                turn_id: turn_id.clone(),
            })
            .await;

        let req = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/prompt",
            "params": {
                "sessionId": self.provider_session_id,
                "prompt": [{"type": "text", "text": build_turn_prompt_text(&input)}],
            }
        });
        if let Err(err) = self.shared.write_frame(&req).await {
            let mut active = self.shared.turn();
            if active.as_ref().is_some_and(|t| t.turn_id == turn_id) {
                *active = None;
            }
            return Err(err);
        }
        Ok(TurnAck { turn_id })
    }

    async fn interrupt_turn(&self, _turn_id: &str) -> Result<(), DriverError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(DriverError::ProcessExited("session is closed".to_owned()));
        }
        let notif = json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": self.provider_session_id}});
        self.shared.write_frame(&notif).await?;

        let drained: Vec<PendingInteraction> =
            self.shared.pending().drain().map(|(_, v)| v).collect();
        for p in drained {
            let _ = self
                .shared
                .reply_result(p.native_rpc_id, perm_outcome(None))
                .await;
            self.shared
                .emit_resolved(p.interaction_id, p.turn_id, None)
                .await;
        }
        Ok(())
    }

    async fn answer_interaction(&self, answer: InteractionAnswer) -> Result<(), DriverError> {
        let Some(p) = self.shared.pending().remove(&answer.interaction_id) else {
            return Err(DriverError::InvalidConfig(format!(
                "no pending interaction with id '{}'",
                answer.interaction_id
            )));
        };
        let (opt_id, decision_id) = if let Some(opt) = p.option_by_decision.get(&answer.decision_id)
        {
            (Some(opt.as_str()), Some(answer.decision_id))
        } else if matches!(answer.decision_id.as_str(), "cancel" | "cancelled") {
            (None, None)
        } else {
            self.shared.pending().insert(p.interaction_id.clone(), p);
            return Err(DriverError::InvalidConfig(format!(
                "unknown decision_id '{}' for interaction '{}'",
                answer.decision_id, answer.interaction_id
            )));
        };

        self.shared
            .reply_result(p.native_rpc_id, perm_outcome(opt_id))
            .await?;
        self.shared
            .emit_resolved(p.interaction_id, p.turn_id, decision_id)
            .await;
        Ok(())
    }

    async fn close(&self) -> Result<(), DriverError> {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if self.supports_close {
            let p = json!({"sessionId": self.provider_session_id});
            let _ = self
                .shared
                .send_rpc("session/close", p, CLOSE_RPC_TIMEOUT)
                .await;
        }
        teardown_process(&self.shared).await;
        Ok(())
    }
}

impl Drop for AgySession {
    fn drop(&mut self) {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Non-blocking Drop: hand SIGTERM -> grace -> SIGKILL + reaping to a detached
        // task/thread where every `/bin/kill` invocation is waited via `.status()` (zero zombies).
        let shared = Arc::clone(&self.shared);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { teardown_process(&shared).await });
        } else {
            let _ = std::thread::Builder::new()
                .name("agy-acp-reaper".to_owned())
                .spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(teardown_process(&shared));
                    }
                });
        }
    }
}

async fn stdout_reader_loop(stdout: tokio::process::ChildStdout, shared: Arc<SessionShared>) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_incoming(trimmed) {
            Ok(IncomingMessage::Response { id, result, error }) => {
                handle_rpc_response(&shared, id, result, error).await
            }
            Ok(IncomingMessage::Notification { method, params }) => {
                handle_notification(&shared, &method, &params).await
            }
            Ok(IncomingMessage::ServerRequest { id, method, params }) => {
                handle_server_request(&shared, id, &method, &params).await
            }
            Err(err) => {
                tracing::debug!(error = %err, "ignoring unparseable ACP stdout line");
                shared
                    .emit(NormalizedEvent::Unknown {
                        event_type: "acp.malformed_line".to_owned(),
                        payload: Some(json!({"error": err})),
                    })
                    .await;
            }
        }
    }

    let tail = shared.stderr_tail.lock().expect("stderr_tail lock").clone();
    let reason = if shared.closed.load(Ordering::Acquire) {
        Some("session closed".to_owned())
    } else if tail.is_empty() {
        Some("ACP process exited unexpectedly".to_owned())
    } else {
        Some(format!("ACP process exited unexpectedly: {tail}"))
    };

    let waiters: Vec<RpcWaiter> = shared.waiters().drain().map(|(_, tx)| tx).collect();
    for tx in waiters {
        let _ = tx.send(Err(DriverError::ProcessExited(
            reason
                .clone()
                .unwrap_or_else(|| "ACP process exited".to_owned()),
        )));
    }
    shared.pending().clear();

    let orphaned = shared.turn().take();
    if let Some(turn) = orphaned {
        shared
            .emit(NormalizedEvent::TurnCompleted {
                turn_id: turn.turn_id,
                status: TurnTerminalStatus::Failed,
                error: reason.clone(),
            })
            .await;
    }
    shared.emit(NormalizedEvent::SessionClosed { reason }).await;
}

async fn handle_rpc_response(
    shared: &SessionShared,
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
) {
    let key = rpc_id_key(&id);
    let completed = {
        let mut active = shared.turn();
        active
            .as_ref()
            .is_some_and(|t| t.prompt_rpc_key == key)
            .then(|| active.take())
            .flatten()
    };

    if let Some(turn) = completed {
        if let Some(err_val) = error {
            shared
                .emit(NormalizedEvent::TurnCompleted {
                    turn_id: turn.turn_id,
                    status: TurnTerminalStatus::Failed,
                    error: Some(format_rpc_error(&err_val)),
                })
                .await;
            return;
        }
        let res = result.unwrap_or(Value::Null);
        if !turn.accumulated_text.is_empty() {
            shared
                .emit(NormalizedEvent::AssistantMessage {
                    turn_id: turn.turn_id.clone(),
                    item_id: None,
                    text: turn.accumulated_text,
                    structured_output: None,
                })
                .await;
        }
        if res.get("usage").is_some() {
            let m = extract_usage_metrics(&res);
            if m != Default::default() {
                shared
                    .emit(NormalizedEvent::Usage {
                        turn_id: Some(turn.turn_id.clone()),
                        metrics: m,
                    })
                    .await;
            }
        }
        shared
            .emit(NormalizedEvent::TurnCompleted {
                turn_id: turn.turn_id,
                status: map_stop_reason(res.get("stopReason").and_then(Value::as_str)),
                error: None,
            })
            .await;
        return;
    }

    if let Some(tx) = shared.waiters().remove(&key) {
        let outcome = match error {
            Some(e) => Err(DriverError::Protocol(format_rpc_error(&e))),
            None => Ok(result.unwrap_or(Value::Null)),
        };
        let _ = tx.send(outcome);
    }
}

async fn handle_notification(shared: &SessionShared, method: &str, params: &Value) {
    let event = if method == "session/update" {
        let mut active = shared.turn();
        match active.as_mut() {
            Some(t) => map_session_update(params, Some(&t.turn_id), Some(&mut t.accumulated_text)),
            None => map_session_update(params, None, None),
        }
    } else {
        NormalizedEvent::Unknown {
            event_type: format!("acp.notification.{method}"),
            payload: Some(params.clone()),
        }
    };
    shared.emit(event).await;
}

async fn handle_server_request(shared: &SessionShared, id: Value, method: &str, params: &Value) {
    if method != "session/request_permission" {
        let reply = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unsupported client method '{method}'")}});
        let _ = shared.write_frame(&reply).await;
        shared
            .emit(NormalizedEvent::Unknown {
                event_type: format!("acp.request.{method}"),
                payload: Some(params.clone()),
            })
            .await;
        return;
    }

    let turn_id = shared
        .turn()
        .as_ref()
        .map(|t| t.turn_id.clone())
        .unwrap_or_default();
    let parsed = parse_permission_request(params);
    let interaction_id = Uuid::now_v7().to_string();
    let decision = if shared.access == AccessMode::ReadOnly && parsed.is_mode_change {
        AutoDecision::Deny {
            reason: "mode switching is denied under read_only access".to_owned(),
        }
    } else {
        evaluate_interaction(
            &parsed.kind,
            shared.access,
            shared.approval_policy,
            &shared.workspace,
            false,
        )
    };
    let request = InteractionRequest {
        interaction_id: interaction_id.clone(),
        turn_id: turn_id.clone(),
        kind: parsed.kind,
        original_kind: parsed.original_kind,
        title: parsed.title,
        decisions: parsed.decisions,
        protected: false,
        response_schema: None,
        max_response_bytes: None,
    };

    if matches!(decision, AutoDecision::Ask) {
        shared.pending().insert(
            interaction_id,
            PendingInteraction {
                interaction_id: request.interaction_id.clone(),
                turn_id,
                native_rpc_id: id,
                option_by_decision: parsed.option_by_decision,
            },
        );
        shared
            .emit(NormalizedEvent::InteractionRequested { request })
            .await;
        return;
    }

    shared
        .emit(NormalizedEvent::InteractionRequested { request })
        .await;
    // AutoApproveScoped must NEVER fall back to `allow_always`; cancel if no `allow_once`.
    let chosen = match decision {
        AutoDecision::Allow => parsed.allow_once_decision,
        AutoDecision::Deny { .. } => parsed.deny_decision,
        AutoDecision::Ask => unreachable!(),
    };
    let (opt_id, decision_id) = match chosen {
        Some((dec_id, native_opt)) => (Some(native_opt), Some(dec_id)),
        None => (None, None),
    };
    let _ = shared
        .reply_result(id, perm_outcome(opt_id.as_deref()))
        .await;
    shared
        .emit_resolved(interaction_id, turn_id, decision_id)
        .await;
}

fn format_rpc_error(err: &Value) -> String {
    let msg = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JSON-RPC error");
    match err.get("code").and_then(Value::as_i64) {
        Some(c) => format!("JSON-RPC error {c}: {msg}"),
        None => msg.to_owned(),
    }
}

async fn drain_stderr(stderr: tokio::process::ChildStderr, shared: Arc<SessionShared>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "driver_agy_acp::stderr", "{line}");
        let mut tail = shared.stderr_tail.lock().expect("stderr_tail lock");
        if !tail.is_empty() {
            tail.push('\n');
        }
        tail.push_str(&line);
        if tail.len() > 1024 {
            let boundary = tail.ceil_char_boundary(tail.len() - 1024);
            tail.drain(..boundary);
        }
    }
}

async fn teardown_process(shared: &SessionShared) {
    // 1. Signal PGID with SIGTERM first (never block on stdin mutex before signaling!).
    #[cfg(unix)]
    if let Some(pgid) = shared.pgid {
        let _ = kill_pgid("-TERM", pgid).await;
    }

    // 2. Best-effort non-blocking close of stdin without deadlocking on any stuck writer.
    if let Ok(mut stdin) = shared.stdin.try_lock() {
        *stdin = None;
    }

    // 3. Wait up to TERM_GRACE_PERIOD for direct child and any PGID descendants.
    let deadline = tokio::time::Instant::now() + TERM_GRACE_PERIOD;
    let mut child = shared.child.lock().await.take();
    if let Some(ref mut proc) = child {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if tokio::time::timeout(rem, proc.wait()).await.is_ok() {
            child = None;
        }
    }

    #[cfg(unix)]
    if let Some(pgid) = shared.pgid {
        while tokio::time::Instant::now() < deadline && kill_pgid("-0", pgid).await {
            tokio::time::sleep(PGID_POLL_INTERVAL).await;
        }
        if child.is_some() || kill_pgid("-0", pgid).await {
            let _ = kill_pgid("-KILL", pgid).await;
        }
    }

    if let Some(mut proc) = child {
        let _ = proc.start_kill();
        let _ = proc.wait().await;
    }
    if let Ok(mut stdin) = shared.stdin.try_lock() {
        *stdin = None;
    }
}

#[cfg(unix)]
async fn kill_pgid(signal: &str, pgid: u32) -> bool {
    Command::new("/bin/kill")
        .args([signal, "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}
