//! Active Claude Code `stream-json` subprocess session lifecycle and event pump.
//!
//! ## Process Group Teardown & `setsid` Caveat
//!
//! The child runs in its own process group (`Command::process_group(0)`).
//! `close()` closes stdin, sends `/bin/kill -TERM -- -<pgid>`, waits 500 ms,
//! escalates to `-KILL` + `child.start_kill()` + `child.wait()`, and deletes
//! the `0600`/`0700` MCP config. `Drop` never blocks: it deletes the MCP config
//! immediately and dispatches a best-effort non-blocking SIGTERM + detached
//! reaper. Descendants calling `setsid()` escape the process group.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use dolgorae_spi::{
    AccessMode, ApprovalPolicy, AutoDecision, DriverError, DriverSession, EventReceiver,
    EventSender, InteractionAnswer, NormalizedEvent, TurnAck, TurnInput, evaluate_interaction,
    event_channel,
};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::wire::{
    ALLOW_ONCE, CANCEL, DENY_ONCE, DENY_SESSION, build_interaction_request, encode_allow_response,
    encode_control_request, encode_deny_response, encode_error_control_response, encode_user_turn,
    parse_assistant_blocks, parse_result_events, parse_stream_text_delta, parse_user_tool_results,
    str_field,
};
use crate::{LaunchOptions, McpConfigGuard, build_launch_args};

const EVENT_CAP: usize = 256;
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const EARLY_SID_WAIT: Duration = Duration::from_millis(25);
const GRACE_TIMEOUT: Duration = Duration::from_millis(500);
const DROP_GRACE: Duration = Duration::from_millis(300);
const KILL_WAIT: Duration = Duration::from_secs(2);

fn lock_std<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct PendingInteraction {
    interaction_id: String,
    native_id: String,
    turn_id: String,
    original_input: Value,
}

#[derive(Default)]
struct ActiveTurn {
    turn_id: String,
    streamed_text: String,
    assistant_text: String,
    interrupted: bool,
    tool_names: HashMap<String, String>,
}

struct InitHandshake {
    request_id: String,
    responder: Option<oneshot::Sender<Result<Option<String>, String>>>,
    sid_tx: Option<oneshot::Sender<String>>,
}

struct SharedState {
    stdin: AsyncMutex<Option<ChildStdin>>,
    child: AsyncMutex<Option<Child>>,
    pgid: Option<u32>,
    event_tx: EventSender,
    workspace: PathBuf,
    access: AccessMode,
    approval_policy: ApprovalPolicy,
    session_id: StdMutex<String>,
    active_turn: StdMutex<Option<ActiveTurn>>,
    pending: StdMutex<HashMap<String, PendingInteraction>>,
    closed: AtomicBool,
    stderr_tail: StdMutex<String>,
}

impl SharedState {
    async fn emit(&self, event: NormalizedEvent) {
        let _ = self.event_tx.send(event).await;
    }

    async fn write_frame(&self, frame: &Value) -> Result<(), DriverError> {
        let mut line = serde_json::to_string(frame)
            .map_err(|e| DriverError::Protocol(format!("serialize wire frame: {e}")))?;
        line.push('\n');
        let mut guard = self.stdin.lock().await;
        let pipe = guard
            .as_mut()
            .ok_or_else(|| DriverError::ProcessExited("claude stdin closed".to_owned()))?;
        pipe.write_all(line.as_bytes()).await?;
        pipe.flush().await?;
        Ok(())
    }

    async fn teardown(&self, grace: Duration) {
        // Send PGID SIGTERM before touching stdin so a blocked writer never deadlocks close().
        if let Some(pgid) = self.pgid {
            signal_pgid(pgid, "-TERM").await;
        }
        if let Ok(mut guard) = self.stdin.try_lock() {
            *guard = None;
        }
        let child = self.child.lock().await.take();
        if let Some(mut proc) = child
            && tokio::time::timeout(grace, proc.wait()).await.is_err()
        {
            if let Some(pgid) = self.pgid {
                signal_pgid(pgid, "-KILL").await;
            }
            let _ = proc.start_kill();
            let _ = tokio::time::timeout(KILL_WAIT, proc.wait()).await;
        }
        if let Ok(mut guard) = self.stdin.try_lock() {
            *guard = None;
        }
    }
}

/// Running Claude Code `stream-json` session implementing `DriverSession`.
pub struct ClaudeSession {
    provider_session_id: String,
    events: Option<EventReceiver>,
    shared: Arc<SharedState>,
    mcp_config: McpConfigGuard,
}

impl ClaudeSession {
    pub(crate) async fn spawn(binary: &Path, opts: LaunchOptions) -> Result<Self, DriverError> {
        let mcp_config = McpConfigGuard::create(&opts.mcp_servers)?;
        let args = build_launch_args(&opts, mcp_config.file_path())?;

        let mut cmd = Command::new(binary);
        cmd.args(&args)
            .current_dir(&opts.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| DriverError::Unavailable(format!("spawn '{}': {e}", binary.display())))?;
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

        let fallback_sid = opts.session_mode.session_id().to_owned();
        let (event_tx, event_rx) = event_channel(EVENT_CAP);
        let shared = Arc::new(SharedState {
            stdin: AsyncMutex::new(Some(stdin)),
            child: AsyncMutex::new(Some(child)),
            pgid,
            event_tx,
            workspace: opts.workspace,
            access: opts.access,
            approval_policy: opts.approval_policy,
            session_id: StdMutex::new(fallback_sid),
            active_turn: StdMutex::new(None),
            pending: StdMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            stderr_tail: StdMutex::new(String::new()),
        });

        if let Some(stderr_pipe) = stderr {
            let st = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr_pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut g = lock_std(&st.stderr_tail);
                    if g.len() > 2048 {
                        g.clear();
                    }
                    if !g.is_empty() {
                        g.push('\n');
                    }
                    g.push_str(&line);
                }
            });
        }

        let init_id = Uuid::now_v7().to_string();
        let (init_tx, init_rx) = oneshot::channel();
        let (sid_tx, sid_rx) = oneshot::channel();
        let hs = InitHandshake {
            request_id: init_id.clone(),
            responder: Some(init_tx),
            sid_tx: Some(sid_tx),
        };
        tokio::spawn(run_stdout_reader(stdout, Arc::clone(&shared), hs));

        let init_frame = encode_control_request(&init_id, "initialize");
        let init_err = match shared.write_frame(&init_frame).await {
            Err(e) => Some(e),
            Ok(()) => match tokio::time::timeout(INIT_TIMEOUT, init_rx).await {
                Ok(Ok(Ok(sid))) => {
                    if let Some(s) = sid {
                        *lock_std(&shared.session_id) = s;
                    } else if let Ok(Ok(early)) = tokio::time::timeout(EARLY_SID_WAIT, sid_rx).await
                    {
                        *lock_std(&shared.session_id) = early;
                    }
                    None
                }
                Ok(Ok(Err(m))) => Some(DriverError::Protocol(format!("init failed: {m}"))),
                Ok(Err(_)) => {
                    let tail = lock_std(&shared.stderr_tail).clone();
                    let msg = if tail.is_empty() {
                        "claude exited before initialize".to_owned()
                    } else {
                        format!("claude exited before initialize: {tail}")
                    };
                    Some(DriverError::ProcessExited(msg))
                }
                Err(_) => Some(DriverError::Timeout(
                    "claude initialize timed out".to_owned(),
                )),
            },
        };
        if let Some(err) = init_err {
            shared.closed.store(true, Ordering::SeqCst);
            shared.teardown(GRACE_TIMEOUT).await;
            mcp_config.cleanup();
            return Err(err);
        }

        let provider_session_id = lock_std(&shared.session_id).clone();
        Ok(Self {
            provider_session_id,
            events: Some(event_rx),
            shared,
            mcp_config,
        })
    }
}

#[async_trait]
impl DriverSession for ClaudeSession {
    fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    fn take_events(&mut self) -> Option<EventReceiver> {
        self.events.take()
    }

    async fn send_turn(&self, input: TurnInput) -> Result<TurnAck, DriverError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(DriverError::ProcessExited("session closed".to_owned()));
        }
        let turn_id = Uuid::now_v7().to_string();
        let sid = lock_std(&self.shared.session_id).clone();
        *lock_std(&self.shared.active_turn) = Some(ActiveTurn {
            turn_id: turn_id.clone(),
            ..ActiveTurn::default()
        });

        self.shared
            .emit(NormalizedEvent::TurnStarted {
                turn_id: turn_id.clone(),
            })
            .await;

        let frame = encode_user_turn(&sid, &input);
        if let Err(err) = self.shared.write_frame(&frame).await {
            let mut active = lock_std(&self.shared.active_turn);
            if active.as_ref().is_some_and(|t| t.turn_id == turn_id) {
                *active = None;
            }
            return Err(err);
        }
        Ok(TurnAck { turn_id })
    }

    async fn interrupt_turn(&self, turn_id: &str) -> Result<(), DriverError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(DriverError::ProcessExited("session closed".to_owned()));
        }
        if let Some(turn) = lock_std(&self.shared.active_turn).as_mut()
            && turn.turn_id == turn_id
        {
            turn.interrupted = true;
        }
        let frame = encode_control_request(&Uuid::now_v7().to_string(), "interrupt");
        self.shared.write_frame(&frame).await
    }

    async fn answer_interaction(&self, answer: InteractionAnswer) -> Result<(), DriverError> {
        let entry = lock_std(&self.shared.pending).remove(&answer.interaction_id);
        let Some(p) = entry else {
            return Err(DriverError::InvalidConfig(format!(
                "unknown or already resolved interaction_id: {}",
                answer.interaction_id
            )));
        };

        let response = match answer.decision_id.as_str() {
            ALLOW_ONCE => encode_allow_response(&p.native_id, &p.original_input),
            DENY_ONCE | DENY_SESSION | CANCEL => {
                encode_deny_response(&p.native_id, "denied by caller")
            }
            other => {
                lock_std(&self.shared.pending).insert(p.interaction_id.clone(), p);
                return Err(DriverError::InvalidConfig(format!(
                    "unsupported decision_id '{other}'"
                )));
            }
        };

        self.shared.write_frame(&response).await?;
        self.shared
            .emit(NormalizedEvent::InteractionResolved {
                interaction_id: answer.interaction_id,
                turn_id: p.turn_id,
                decision_id: Some(answer.decision_id),
            })
            .await;
        Ok(())
    }

    async fn close(&self) -> Result<(), DriverError> {
        if !self.shared.closed.swap(true, Ordering::SeqCst) {
            self.shared.teardown(GRACE_TIMEOUT).await;
        }
        self.mcp_config.cleanup();
        Ok(())
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.mcp_config.cleanup();
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                shared.teardown(DROP_GRACE).await;
            });
        } else {
            std::thread::spawn(move || {
                if let Some(pgid) = shared.pgid {
                    signal_pgid_blocking(pgid, "-TERM");
                }
                if let Ok(mut guard) = shared.stdin.try_lock() {
                    *guard = None;
                }
                let mut exited = false;
                if let Ok(mut guard) = shared.child.try_lock()
                    && let Some(mut proc) = guard.take()
                {
                    let deadline = std::time::Instant::now() + DROP_GRACE;
                    while std::time::Instant::now() < deadline {
                        if matches!(proc.try_wait(), Ok(Some(_)) | Err(_)) {
                            exited = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    if !exited {
                        if let Some(pgid) = shared.pgid {
                            signal_pgid_blocking(pgid, "-KILL");
                        }
                        let _ = proc.start_kill();
                        for _ in 0..20 {
                            if matches!(proc.try_wait(), Ok(Some(_)) | Err(_)) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                } else if let Some(pgid) = shared.pgid {
                    std::thread::sleep(DROP_GRACE);
                    signal_pgid_blocking(pgid, "-KILL");
                }
                drop(shared);
            });
        }
    }
}

async fn run_stdout_reader(stdout: ChildStdout, shared: Arc<SharedState>, mut hs: InitHandshake) {
    let mut lines = BufReader::new(stdout).lines();
    let mut read_err = None;
    loop {
        match lines.next_line().await {
            Ok(Some(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(frame) => handle_frame(&shared, &mut hs, frame).await,
                    Err(err) => {
                        warn!(error = %err, "ignoring malformed claude stdout frame");
                        let ev = NormalizedEvent::Unknown {
                            event_type: "malformed_json".to_owned(),
                            payload: Some(Value::String(trimmed.to_owned())),
                        };
                        shared.emit(ev).await;
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                read_err = Some(e.to_string());
                break;
            }
        }
    }

    if let Some(tx) = hs.responder.take() {
        let _ = tx.send(Err("stdout closed before initialize".to_owned()));
    }
    lock_std(&shared.pending).clear();
    let reason = if shared.closed.load(Ordering::SeqCst) {
        "session closed".to_owned()
    } else if let Some(err) = read_err {
        format!("stdout read error: {err}")
    } else {
        let tail = lock_std(&shared.stderr_tail).clone();
        if tail.is_empty() {
            "claude process exited unexpectedly".to_owned()
        } else {
            format!("claude process exited: {tail}")
        }
    };
    shared
        .emit(NormalizedEvent::SessionClosed {
            reason: Some(reason),
        })
        .await;
}

async fn handle_frame(shared: &SharedState, hs: &mut InitHandshake, frame: Value) {
    match str_field(&frame, "type").unwrap_or("untagged") {
        "keep_alive" => {}
        "system" => {
            if str_field(&frame, "subtype") == Some("init")
                && let Some(sid) = str_field(&frame, "session_id").filter(|s| !s.is_empty())
            {
                *lock_std(&shared.session_id) = sid.to_owned();
                if let Some(tx) = hs.sid_tx.take() {
                    let _ = tx.send(sid.to_owned());
                }
            }
        }
        "control_response" => handle_init_response(shared, hs, &frame),
        "control_request" => handle_control_request(shared, frame).await,
        "control_cancel_request" => {
            let removed = str_field(&frame, "request_id").and_then(|nid| {
                let mut map = lock_std(&shared.pending);
                let key = map
                    .iter()
                    .find(|(_, v)| v.native_id == nid)
                    .map(|(k, _)| k.clone())?;
                map.remove(&key)
            });
            if let Some(p) = removed {
                let ev = NormalizedEvent::InteractionResolved {
                    interaction_id: p.interaction_id,
                    turn_id: p.turn_id,
                    decision_id: None,
                };
                shared.emit(ev).await;
            }
        }
        "stream_event" => {
            if let Some((item_id, delta)) = parse_stream_text_delta(&frame) {
                let turn_id = lock_std(&shared.active_turn).as_mut().map(|t| {
                    t.streamed_text.push_str(&delta);
                    t.turn_id.clone()
                });
                if let Some(turn_id) = turn_id {
                    let ev = NormalizedEvent::TextDelta {
                        turn_id,
                        item_id,
                        delta,
                    };
                    shared.emit(ev).await;
                }
            }
        }
        "assistant" => {
            let events = lock_std(&shared.active_turn)
                .as_mut()
                .map_or_else(Vec::new, |t| {
                    let (text, evs) = parse_assistant_blocks(&frame, &t.turn_id, &mut t.tool_names);
                    if !text.is_empty() {
                        if !t.assistant_text.is_empty() {
                            t.assistant_text.push('\n');
                        }
                        t.assistant_text.push_str(&text);
                    }
                    evs
                });
            for ev in events {
                shared.emit(ev).await;
            }
        }
        "user" => {
            let events = lock_std(&shared.active_turn)
                .as_ref()
                .map_or_else(Vec::new, |t| {
                    parse_user_tool_results(&frame, &t.turn_id, &t.tool_names)
                });
            for ev in events {
                shared.emit(ev).await;
            }
        }
        "result" => {
            if let Some(sid) = str_field(&frame, "session_id").filter(|s| !s.is_empty()) {
                *lock_std(&shared.session_id) = sid.to_owned();
            }
            let finished = lock_std(&shared.active_turn).take();
            if let Some(t) = finished {
                let fb = if !t.assistant_text.is_empty() {
                    t.assistant_text.as_str()
                } else {
                    t.streamed_text.as_str()
                };
                for ev in parse_result_events(&frame, &t.turn_id, fb, t.interrupted) {
                    shared.emit(ev).await;
                }
            }
        }
        other => {
            debug!(event_type = other, "forwarding unknown claude wire frame");
            let ev = NormalizedEvent::Unknown {
                event_type: other.to_owned(),
                payload: Some(frame),
            };
            shared.emit(ev).await;
        }
    }
}

fn handle_init_response(shared: &SharedState, hs: &mut InitHandshake, frame: &Value) {
    let Some(resp) = frame.get("response") else {
        return;
    };
    if str_field(resp, "request_id") != Some(hs.request_id.as_str()) {
        return;
    }
    let Some(tx) = hs.responder.take() else {
        return;
    };
    if str_field(resp, "subtype") == Some("error") {
        let msg = str_field(resp, "error").unwrap_or("initialize rejected");
        let _ = tx.send(Err(msg.to_owned()));
        return;
    }
    let sid = resp
        .get("response")
        .and_then(|r| str_field(r, "session_id"))
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(ref captured) = sid {
        *lock_std(&shared.session_id) = captured.clone();
    }
    let _ = tx.send(Ok(sid));
}

async fn handle_control_request(shared: &SharedState, frame: Value) {
    let Some(native_id) = str_field(&frame, "request_id").map(str::to_owned) else {
        return;
    };
    let Some(req) = frame.get("request") else {
        return;
    };
    let subtype = str_field(req, "subtype").unwrap_or("unknown");
    if subtype != "can_use_tool" {
        let reply = encode_error_control_response(
            &native_id,
            &format!("unsupported control_request subtype: {subtype}"),
        );
        let _ = shared.write_frame(&reply).await;
        let ev = NormalizedEvent::Unknown {
            event_type: "control_request".to_owned(),
            payload: Some(frame),
        };
        shared.emit(ev).await;
        return;
    }

    let tool_name = str_field(req, "tool_name").unwrap_or("unknown");
    let orig_input = req
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let reason = str_field(req, "decision_reason").map(str::to_owned);
    let interaction_id = Uuid::now_v7().to_string();
    let turn_id = lock_std(&shared.active_turn)
        .as_ref()
        .map(|t| t.turn_id.clone())
        .unwrap_or_default();
    let spi_req = build_interaction_request(
        interaction_id.clone(),
        turn_id.clone(),
        tool_name,
        &orig_input,
        reason,
    );

    // Claude stream-json runs without an OS sandbox (`os_sandboxed = false`):
    // ReadOnly + Interactive relies on human approval for Bash while hard-denying
    // FileChangeApproval; AutoApproveScoped denies all unsandboxed commands.
    let decision = evaluate_interaction(
        &spi_req.kind,
        shared.access,
        shared.approval_policy,
        &shared.workspace,
        false,
    );

    match decision {
        AutoDecision::Ask => {
            lock_std(&shared.pending).insert(
                interaction_id.clone(),
                PendingInteraction {
                    interaction_id,
                    native_id,
                    turn_id,
                    original_input: orig_input,
                },
            );
            shared
                .emit(NormalizedEvent::InteractionRequested { request: spi_req })
                .await;
        }
        AutoDecision::Allow | AutoDecision::Deny { .. } => {
            shared
                .emit(NormalizedEvent::InteractionRequested { request: spi_req })
                .await;
            let (reply, dec_id) = match decision {
                AutoDecision::Allow => (encode_allow_response(&native_id, &orig_input), ALLOW_ONCE),
                AutoDecision::Deny { reason } => {
                    (encode_deny_response(&native_id, &reason), DENY_ONCE)
                }
                AutoDecision::Ask => unreachable!(),
            };
            let _ = shared.write_frame(&reply).await;
            let ev = NormalizedEvent::InteractionResolved {
                interaction_id,
                turn_id,
                decision_id: Some(dec_id.to_owned()),
            };
            shared.emit(ev).await;
        }
    }
}

async fn signal_pgid(pgid: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .args([signal, "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

fn signal_pgid_blocking(pgid: u32, signal: &str) {
    let _ = std::process::Command::new("/bin/kill")
        .args([signal, "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
