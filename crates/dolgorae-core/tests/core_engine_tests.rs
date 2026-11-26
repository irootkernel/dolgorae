//! Comprehensive offline integration tests for `dolgorae-core`.
//!
//! Uses an in-process programmable mock `AgentDriver`/`DriverSession` and
//! isolated temporary homes (never touches the user's real `~/.dolgorae`).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::future::Future;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dolgorae_core::{
    AccessMode, AgentDriver, AnswerInteractionRequest, ApprovalPolicy, CallerContext, CoreConfig,
    CoreEngine, CreateSessionRequest, Cursor, DecisionKind, DecisionOption, DriverCapabilities,
    DriverError, DriverSession, ErrorCode, InteractionAnswer, InteractionKind, InteractionRequest,
    InteractionStatus, MAX_ARTIFACT_BYTES, MAX_INLINE_BYTES, NormalizedEvent, PostMailboxRequest,
    ReplayResult, ResumeSessionRequest, ResumeSpec, RoleSource, SessionFilter, SessionPolicy,
    SessionSpec, SessionStatus, SubmitTurnRequest, ToolActivityStatus, TurnAck, TurnInput,
    TurnTerminalStatus, UsageMetrics, WaitMode, WaitSessionsRequest,
};
use dolgorae_spi::{EventReceiver, EventSender, event_channel};
use serde_json::json;
use uuid::Uuid;

/// Isolated temporary directory cleaned up automatically on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("dolgorae-core-test-{label}-{}", Uuid::now_v7()));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self { path: dir }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Programmable mock driver session allowing tests to inspect RPC calls and emit
/// arbitrary `NormalizedEvent`s at exact points in time.
struct MockSessionControl {
    event_tx: EventSender,
    received_turns: Mutex<Vec<TurnInput>>,
    received_answers: Mutex<Vec<InteractionAnswer>>,
    interrupted_turns: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl MockSessionControl {
    async fn emit(&self, ev: NormalizedEvent) {
        self.event_tx.send(ev).await.expect("send mock event");
    }

    fn answers(&self) -> Vec<InteractionAnswer> {
        self.received_answers.lock().unwrap().clone()
    }

    fn interrupted(&self) -> Vec<String> {
        self.interrupted_turns.lock().unwrap().clone()
    }
}

struct MockDriverSession {
    provider_session_id: String,
    event_rx: Mutex<Option<EventReceiver>>,
    control: Arc<MockSessionControl>,
    fail_send_turn_with_protocol: Arc<AtomicBool>,
    fail_answer_with_invalid_config: Arc<AtomicBool>,
    emit_start_and_complete_before_ack: Arc<AtomicBool>,
    emit_tool_activity_during_answer: Arc<AtomicBool>,
    turn_seq: AtomicU64,
}

impl DriverSession for MockDriverSession {
    fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    fn take_events(&mut self) -> Option<EventReceiver> {
        self.event_rx.lock().unwrap().take()
    }

    fn send_turn<'life0, 'async_trait>(
        &'life0 self,
        input: TurnInput,
    ) -> Pin<Box<dyn Future<Output = Result<TurnAck, DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if self.fail_send_turn_with_protocol.load(Ordering::Relaxed) {
                return Err(DriverError::Protocol(
                    "simulated driver protocol failure".to_owned(),
                ));
            }
            self.control.received_turns.lock().unwrap().push(input);
            let n = self.turn_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let turn_id = format!("turn-{n}");
            if self
                .emit_start_and_complete_before_ack
                .load(Ordering::Relaxed)
            {
                self.control
                    .emit(NormalizedEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                    })
                    .await;
                self.control
                    .emit(NormalizedEvent::AssistantMessage {
                        turn_id: turn_id.clone(),
                        item_id: Some(format!("msg-{n}")),
                        text: r#"{"override_ok":true}"#.to_owned(),
                        structured_output: None,
                    })
                    .await;
                self.control
                    .emit(NormalizedEvent::TurnCompleted {
                        turn_id: turn_id.clone(),
                        status: TurnTerminalStatus::Completed,
                        error: None,
                    })
                    .await;
                // Yield so the drain task processes all three events BEFORE send_turn returns.
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            Ok(TurnAck { turn_id })
        })
    }

    fn interrupt_turn<'life0, 'life1, 'async_trait>(
        &'life0 self,
        turn_id: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let tid = turn_id.to_owned();
        Box::pin(async move {
            self.control.interrupted_turns.lock().unwrap().push(tid);
            Ok(())
        })
    }

    fn answer_interaction<'life0, 'async_trait>(
        &'life0 self,
        answer: InteractionAnswer,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if self.fail_answer_with_invalid_config.load(Ordering::Relaxed) {
                return Err(DriverError::InvalidConfig("already cancelled".to_owned()));
            }
            self.control.received_answers.lock().unwrap().push(answer);
            if self
                .emit_tool_activity_during_answer
                .load(Ordering::Relaxed)
            {
                self.control
                    .emit(NormalizedEvent::ToolActivity {
                        turn_id: "turn-1".to_owned(),
                        item_id: "tool-resume-1".to_owned(),
                        tool_name: "shell".to_owned(),
                        status: ToolActivityStatus::Started,
                        summary: Some("resumed immediately after approval".to_owned()),
                        command: Some("ls".to_owned()),
                        exit_status: None,
                        paths: Vec::new(),
                    })
                    .await;
                // Let the drain task persist ToolActivity and bump session.revision
                // BEFORE driver.answer_interaction returns Ok(()).
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            Ok(())
        })
    }

    fn close<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.control.closed.store(true, Ordering::Relaxed);
            Ok(())
        })
    }
}

struct MockDriver {
    id: &'static str,
    available: AtomicBool,
    native_output_schema: bool,
    fail_send_turn_with_protocol: Arc<AtomicBool>,
    fail_answer_with_invalid_config: Arc<AtomicBool>,
    emit_start_and_complete_before_ack: Arc<AtomicBool>,
    emit_tool_activity_during_answer: Arc<AtomicBool>,
    session_seq: AtomicU64,
    controls: Mutex<HashMap<String, Arc<MockSessionControl>>>,
    start_specs: Mutex<Vec<SessionSpec>>,
    resume_specs: Mutex<Vec<ResumeSpec>>,
}

impl MockDriver {
    fn new(id: &'static str, available: bool, native_output_schema: bool) -> Arc<Self> {
        Arc::new(Self {
            id,
            available: AtomicBool::new(available),
            native_output_schema,
            fail_send_turn_with_protocol: Arc::new(AtomicBool::new(false)),
            fail_answer_with_invalid_config: Arc::new(AtomicBool::new(false)),
            emit_start_and_complete_before_ack: Arc::new(AtomicBool::new(false)),
            emit_tool_activity_during_answer: Arc::new(AtomicBool::new(false)),
            session_seq: AtomicU64::new(0),
            controls: Mutex::new(HashMap::new()),
            start_specs: Mutex::new(Vec::new()),
            resume_specs: Mutex::new(Vec::new()),
        })
    }

    fn last_control(&self) -> Arc<MockSessionControl> {
        let seq = self.session_seq.load(Ordering::Relaxed);
        let key = format!("{}-prov-{seq}", self.id);
        self.controls
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .expect("mock session control exists")
    }

    fn control_at(&self, index_1_based: u64) -> Arc<MockSessionControl> {
        let key = format!("{}-prov-{index_1_based}", self.id);
        self.controls
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .expect("mock session control at index")
    }

    fn start_specs(&self) -> Vec<SessionSpec> {
        self.start_specs.lock().unwrap().clone()
    }

    fn resume_specs(&self) -> Vec<ResumeSpec> {
        self.resume_specs.lock().unwrap().clone()
    }
}

impl AgentDriver for MockDriver {
    fn driver_id(&self) -> &'static str {
        self.id
    }

    fn probe<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = Result<DriverCapabilities, DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let avail = self.available.load(Ordering::Relaxed);
            Ok(DriverCapabilities {
                driver_id: self.id.to_owned(),
                cli_version: Some("1.0.0-mock".to_owned()),
                available: avail,
                unavailable_reason: if avail {
                    None
                } else {
                    Some("mock driver disabled".to_owned())
                },
                models: Vec::new(),
                supports_effort: true,
                supports_interactive_approval: true,
                native_output_schema: self.native_output_schema,
                supports_mcp_injection: true,
                supports_resume: true,
            })
        })
    }

    fn start_session<'life0, 'async_trait>(
        &'life0 self,
        spec: SessionSpec,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn DriverSession>, DriverError>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.start_specs.lock().unwrap().push(spec);
            let n = self.session_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let prov_id = format!("{}-prov-{n}", self.id);
            let (tx, rx) = event_channel(64);
            let control = Arc::new(MockSessionControl {
                event_tx: tx,
                received_turns: Mutex::new(Vec::new()),
                received_answers: Mutex::new(Vec::new()),
                interrupted_turns: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
            });
            self.controls
                .lock()
                .unwrap()
                .insert(prov_id.clone(), Arc::clone(&control));
            Ok(Box::new(MockDriverSession {
                provider_session_id: prov_id,
                event_rx: Mutex::new(Some(rx)),
                control,
                fail_send_turn_with_protocol: Arc::clone(&self.fail_send_turn_with_protocol),
                fail_answer_with_invalid_config: Arc::clone(&self.fail_answer_with_invalid_config),
                emit_start_and_complete_before_ack: Arc::clone(
                    &self.emit_start_and_complete_before_ack,
                ),
                emit_tool_activity_during_answer: Arc::clone(
                    &self.emit_tool_activity_during_answer,
                ),
                turn_seq: AtomicU64::new(0),
            }) as Box<dyn DriverSession>)
        })
    }

    fn resume_session<'life0, 'async_trait>(
        &'life0 self,
        spec: ResumeSpec,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn DriverSession>, DriverError>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let provider_session_id = spec.provider_session_id.clone();
            self.resume_specs.lock().unwrap().push(spec);
            let n = self.session_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let key = format!("{}-prov-{n}", self.id);
            let (tx, rx) = event_channel(64);
            let control = Arc::new(MockSessionControl {
                event_tx: tx,
                received_turns: Mutex::new(Vec::new()),
                received_answers: Mutex::new(Vec::new()),
                interrupted_turns: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
            });
            self.controls
                .lock()
                .unwrap()
                .insert(key, Arc::clone(&control));
            Ok(Box::new(MockDriverSession {
                provider_session_id,
                event_rx: Mutex::new(Some(rx)),
                control,
                fail_send_turn_with_protocol: Arc::clone(&self.fail_send_turn_with_protocol),
                fail_answer_with_invalid_config: Arc::clone(&self.fail_answer_with_invalid_config),
                emit_start_and_complete_before_ack: Arc::clone(
                    &self.emit_start_and_complete_before_ack,
                ),
                emit_tool_activity_during_answer: Arc::clone(
                    &self.emit_tool_activity_during_answer,
                ),
                turn_seq: AtomicU64::new(0),
            }) as Box<dyn DriverSession>)
        })
    }
}

fn base_create_req(workspace: &Path, driver: &str) -> CreateSessionRequest {
    CreateSessionRequest {
        workspace: workspace.to_path_buf(),
        driver: Some(driver.to_owned()),
        role: None,
        parent_session_id: None,
        model: None,
        effort: None,
        access: Some(AccessMode::ReadOnly),
        approval_policy: Some(ApprovalPolicy::Interactive),
        output_schema: None,
        allow_delegation: false,
        policy: None,
    }
}

async fn wait_for_condition<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..100 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within timeout");
}

struct RepeatingReader {
    remaining: usize,
    byte: u8,
}

impl Read for RepeatingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = buf.len().min(self.remaining);
        buf[..n].fill(self.byte);
        self.remaining -= n;
        Ok(n)
    }
}

#[tokio::test]
async fn stamps_ephemeral_deltas_and_distinct_page_tokens() {
    let home = TempDir::new("stamps-home");
    let ws = TempDir::new("stamps-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![drv.clone()],
    )
    .await
    .expect("init engine");

    let snap = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .expect("create session");
    assert_eq!(snap.revision, 1);
    assert_eq!(snap.head_cursor, Cursor(1));
    assert_eq!(snap.head_cursor.to_string(), snap.revision.to_string());
    assert!(snap.interaction_revision <= snap.revision);

    let (replay, mut live_rx) = engine
        .subscribe_events(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(Cursor(0)),
        )
        .await
        .expect("subscribe");
    match replay {
        ReplayResult::Events(evs) => {
            assert_eq!(evs.len(), 1);
            assert_eq!(evs[0].cursor, Some(Cursor(1)));
            assert_eq!(evs[0].event_type, "session_created");
        }
        ReplayResult::RefreshRequired { .. } => panic!("unexpected refresh"),
    }

    let accepted = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "hello".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .expect("submit turn");
    assert_eq!(accepted.revision, 2);
    assert_eq!(accepted.head_cursor, Cursor(2));

    // Emit ephemeral TextDelta and ToolActivityStatus::Delta, followed by durable AssistantMessage.
    let ctrl = drv.last_control();
    ctrl.emit(NormalizedEvent::TextDelta {
        turn_id: accepted.turn_id.clone(),
        item_id: Some("msg-1".to_owned()),
        delta: "partial ".to_owned(),
    })
    .await;
    ctrl.emit(NormalizedEvent::ToolActivity {
        turn_id: accepted.turn_id.clone(),
        item_id: "tool-1".to_owned(),
        tool_name: "shell".to_owned(),
        status: ToolActivityStatus::Delta,
        summary: Some("stdout chunk".to_owned()),
        command: None,
        exit_status: None,
        paths: Vec::new(),
    })
    .await;
    ctrl.emit(NormalizedEvent::AssistantMessage {
        turn_id: accepted.turn_id.clone(),
        item_id: Some("msg-1".to_owned()),
        text: "final answer".to_owned(),
        structured_output: None,
    })
    .await;
    ctrl.emit(NormalizedEvent::Usage {
        turn_id: Some(accepted.turn_id.clone()),
        metrics: UsageMetrics {
            input_tokens: Some(10),
            output_tokens: Some(20),
            ..Default::default()
        },
    })
    .await;
    ctrl.emit(NormalizedEvent::TurnCompleted {
        turn_id: accepted.turn_id.clone(),
        status: TurnTerminalStatus::Completed,
        error: None,
    })
    .await;

    // Collect live events and confirm ephemeral deltas have cursor == None.
    let mut saw_text_delta = false;
    let mut saw_tool_delta = false;
    let mut saw_completed = false;
    for _ in 0..20 {
        if saw_completed {
            break;
        }
        let ev = tokio::time::timeout(Duration::from_secs(2), live_rx.recv())
            .await
            .expect("live event timeout")
            .expect("live event recv");
        if ev.event_type == "text_delta" {
            assert_eq!(ev.cursor, None);
            saw_text_delta = true;
        } else if ev.event_type == "tool_delta" {
            assert_eq!(ev.cursor, None);
            saw_tool_delta = true;
        } else if ev.event_type == "turn_completed" {
            assert!(ev.cursor.is_some());
            saw_completed = true;
        }
    }
    assert!(saw_text_delta && saw_tool_delta && saw_completed);

    // Durable revision must be exactly 5: session_created(1), turn_started(2),
    // assistant_message(3), usage(4), turn_completed(5). Ephemeral deltas MUST NOT advance revision!
    let final_snap = engine
        .get_session(&CallerContext::ExternalHost, &snap.session_id)
        .await
        .expect("get session");
    assert_eq!(final_snap.revision, 5);
    assert_eq!(final_snap.head_cursor, Cursor(5));
    assert_eq!(final_snap.active_turn_id, None);
    assert!(final_snap.interaction_revision <= final_snap.revision);

    // Timeline pagination: distinct `highest_cursor` vs opaque `next_page_token`.
    let page1 = engine
        .get_timeline(&CallerContext::ExternalHost, &snap.session_id, None, 2)
        .await
        .expect("page 1");
    assert_eq!(page1.highest_cursor, Cursor(5));
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.items[0].cursor, Cursor(1));
    assert_eq!(page1.items[1].cursor, Cursor(2));
    let tok1 = page1.next_page_token.expect("next_page_token");
    assert_ne!(tok1, page1.highest_cursor.to_string());
    assert!(tok1.starts_with("tok:"));

    let page2 = engine
        .get_timeline(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(tok1),
            10,
        )
        .await
        .expect("page 2");
    assert_eq!(page2.highest_cursor, Cursor(5));
    assert_eq!(page2.next_page_token, None);
    assert_eq!(page2.items.len(), 3);
    assert!(
        page2
            .items
            .iter()
            .all(|i| i.kind != "text_delta" && i.kind != "tool_delta")
    );
}

#[tokio::test]
async fn retention_pruning_gap_and_exclusive_replay() {
    let home = TempDir::new("retention-home");
    let ws = TempDir::new("retention-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let mut cfg = CoreConfig::new(home.path().to_path_buf());
    cfg.max_retained_events_per_session = 3;

    let engine = CoreEngine::new(cfg, vec![drv.clone()])
        .await
        .expect("init engine");
    let snap = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .expect("create"); // cursor 1

    // Generate 5 more durable events via mailbox messages -> cursors 2..=6.
    // With retention = 3, retained cursors are 4, 5, 6 (earliest = 4).
    for i in 0..5 {
        engine
            .post_mailbox_message(
                &CallerContext::ExternalHost,
                &snap.session_id,
                PostMailboxRequest {
                    from_session_id: None,
                    subject: Some(format!("msg-{i}")),
                    body: format!("body-{i}"),
                    artifact_ids: Vec::new(),
                },
            )
            .await
            .expect("mailbox");
    }

    // after = 1 (next needed = 2 < earliest 4) -> RefreshRequired!
    let (gap_replay, _) = engine
        .subscribe_events(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(Cursor(1)),
        )
        .await
        .expect("sub gap");
    match gap_replay {
        ReplayResult::RefreshRequired { scopes } => {
            assert!(scopes.contains(&"session".to_owned()));
            assert!(scopes.contains(&"timeline".to_owned()));
        }
        ReplayResult::Events(_) => panic!("expected RefreshRequired on pruned gap"),
    }

    // after = 3 (next needed = 4 == earliest 4) -> exact contiguous replay [4, 5, 6].
    let (exact_replay, _) = engine
        .subscribe_events(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(Cursor(3)),
        )
        .await
        .expect("sub exact");
    match exact_replay {
        ReplayResult::Events(evs) => {
            let cursors: Vec<u64> = evs.iter().map(|e| e.cursor.unwrap().value()).collect();
            assert_eq!(cursors, vec![4, 5, 6]);
        }
        ReplayResult::RefreshRequired { .. } => panic!("unexpected refresh for after=3"),
    }

    // Exclusive replay: after = 5 returns strictly [6].
    let (excl_replay, _) = engine
        .subscribe_events(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(Cursor(5)),
        )
        .await
        .expect("sub excl");
    match excl_replay {
        ReplayResult::Events(evs) => {
            assert_eq!(evs.len(), 1);
            assert_eq!(evs[0].cursor, Some(Cursor(6)));
        }
        ReplayResult::RefreshRequired { .. } => panic!("unexpected refresh"),
    }

    // Future cursor after = 99 > revision 6 -> RefreshRequired.
    let (future_replay, _) = engine
        .subscribe_events(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(Cursor(99)),
        )
        .await
        .expect("sub future");
    assert!(matches!(
        future_replay,
        ReplayResult::RefreshRequired { .. }
    ));
}

#[tokio::test]
async fn optimistic_concurrency_and_child_interaction_authority() {
    let home = TempDir::new("occ-home");
    let ws = TempDir::new("occ-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![drv.clone()],
    )
    .await
    .expect("init");

    let mut parent_req = base_create_req(ws.path(), "mock-a");
    parent_req.access = Some(AccessMode::WorkspaceWrite);
    let parent = engine
        .create_session(&CallerContext::ExternalHost, parent_req)
        .await
        .expect("parent");
    let parent_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &parent.session_id)
        .await
        .expect("parent token");
    let parent_scope = engine
        .authenticate_actor_token(&parent_tok)
        .await
        .expect("auth parent");

    // Spawn child actor under parent.
    let child_req = CreateSessionRequest {
        parent_session_id: Some(parent.session_id.clone()),
        ..base_create_req(ws.path(), "mock-a")
    };
    let child = engine
        .create_session(&CallerContext::ScopedActor(parent_scope.clone()), child_req)
        .await
        .expect("child");
    let child_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child.session_id)
        .await
        .expect("child token");
    let child_scope = engine
        .authenticate_actor_token(&child_tok)
        .await
        .expect("auth child");
    let child_ctrl = drv.last_control();

    // Start turn on child; concurrent second turn must return 409 SNAPSHOT_CONFLICT.
    let turn = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &child.session_id,
            SubmitTurnRequest {
                text: "run check".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .expect("submit turn 1");

    let dup_err = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &child.session_id,
            SubmitTurnRequest {
                text: "concurrent turn".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(dup_err.code, ErrorCode::SnapshotConflict);

    // Emit interactive approval request with tight max_response_bytes = 64.
    child_ctrl
        .emit(NormalizedEvent::InteractionRequested {
            request: InteractionRequest {
                interaction_id: "int-1".to_owned(),
                turn_id: turn.turn_id.clone(),
                kind: InteractionKind::CommandApproval {
                    command: vec!["cargo".to_owned(), "test".to_owned()],
                    cwd: None,
                    reason: Some("run tests".to_owned()),
                },
                original_kind: "command_approval".to_owned(),
                title: "Run cargo test".to_owned(),
                decisions: vec![DecisionOption {
                    decision_id: "allow".to_owned(),
                    label: "Allow".to_owned(),
                    kind: DecisionKind::AllowOnce,
                }],
                protected: false,
                response_schema: None,
                max_response_bytes: Some(128),
            },
        })
        .await;

    wait_for_condition(|| async {
        engine
            .list_interactions(
                &CallerContext::ExternalHost,
                &child.session_id,
                Some(InteractionStatus::Pending),
            )
            .await
            .is_ok_and(|l| l.interactions.len() == 1)
    })
    .await;

    let list = engine
        .list_interactions(
            &CallerContext::ExternalHost,
            &child.session_id,
            Some(InteractionStatus::Pending),
        )
        .await
        .expect("list interactions");
    assert_eq!(list.interactions.len(), 1);
    let rec = &list.interactions[0];
    assert_eq!(rec.session_generation, 1);
    assert!(list.interaction_revision <= list.session_revision);

    let valid_answer = || AnswerInteractionRequest {
        turn_id: turn.turn_id.clone(),
        session_generation: rec.session_generation,
        expected_interaction_revision: list.interaction_revision,
        expected_session_revision: list.session_revision,
        decision_id: "allow".to_owned(),
        answers: BTreeMap::new(),
        payload: None,
    };

    // Scoped actor (self OR parent) CANNOT answer approvals -> 403 FORBIDDEN_SCOPE.
    let self_err = engine
        .answer_interaction(
            &CallerContext::ScopedActor(child_scope),
            &child.session_id,
            "int-1",
            valid_answer(),
        )
        .await
        .unwrap_err();
    assert_eq!(self_err.code, ErrorCode::ForbiddenScope);

    let parent_err = engine
        .answer_interaction(
            &CallerContext::ScopedActor(parent_scope),
            &child.session_id,
            "int-1",
            valid_answer(),
        )
        .await
        .unwrap_err();
    assert_eq!(parent_err.code, ErrorCode::ForbiddenScope);

    // Bad request: empty turn_id or generation == 0.
    let mut bad = valid_answer();
    bad.turn_id = "".to_owned();
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                bad
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );

    let mut bad_gen = valid_answer();
    bad_gen.session_generation = 0;
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                bad_gen
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );

    // Stale turn_id, generation, interaction_revision, session_revision -> 409 SNAPSHOT_CONFLICT.
    let mut stale_turn = valid_answer();
    stale_turn.turn_id = "turn-wrong".to_owned();
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                stale_turn
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    let mut stale_gen = valid_answer();
    stale_gen.session_generation = 99;
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                stale_gen
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    let mut stale_irev = valid_answer();
    stale_irev.expected_interaction_revision = 0;
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                stale_irev
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    let mut stale_srev = valid_answer();
    stale_srev.expected_session_revision = 0;
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                stale_srev
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    // Oversized interaction response -> 413 PAYLOAD_TOO_LARGE.
    let mut huge = valid_answer();
    huge.payload = Some(json!({ "blob": "x".repeat(256) }));
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                huge
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PayloadTooLarge
    );

    // ExternalHost succeeds and forwards answer to driver.
    let resolved = engine
        .answer_interaction(
            &CallerContext::ExternalHost,
            &child.session_id,
            "int-1",
            valid_answer(),
        )
        .await
        .expect("external host answer");
    assert_eq!(resolved.status, InteractionStatus::Resolved);
    assert!(resolved.interaction_revision <= resolved.session_revision);
    assert_eq!(resolved.head_cursor, Cursor(resolved.session_revision));
    assert_eq!(child_ctrl.answers().len(), 1);
    assert_eq!(child_ctrl.answers()[0].decision_id, "allow");

    // Re-answering already-resolved interaction -> 409 SNAPSHOT_CONFLICT.
    let mut replay_ans = valid_answer();
    replay_ans.expected_interaction_revision = resolved.interaction_revision;
    replay_ans.expected_session_revision = resolved.session_revision;
    assert_eq!(
        engine
            .answer_interaction(
                &CallerContext::ExternalHost,
                &child.session_id,
                "int-1",
                replay_ans
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    // Interrupt active turn forwards to driver.
    engine
        .interrupt_turn(
            &CallerContext::ExternalHost,
            &child.session_id,
            &turn.turn_id,
        )
        .await
        .expect("interrupt");
    assert_eq!(child_ctrl.interrupted(), vec![turn.turn_id]);
}

#[tokio::test]
async fn invariant4_depth_cycles_escalation_escape_and_token_revocation() {
    let home = TempDir::new("inv4-home");
    let ws = TempDir::new("inv4-ws");
    let sub_ws = ws.path().join("sub");
    fs::create_dir_all(&sub_ws).expect("sub ws");
    let outside_ws = TempDir::new("inv4-outside");

    // Workspace-local untrusted role requesting workspace_write!
    let ws_roles_dir = ws.path().join(".dolgorae").join("roles");
    fs::create_dir_all(&ws_roles_dir).expect("roles dir");
    fs::write(
        ws_roles_dir.join("escalator.md"),
        "---\ndriver: mock-a\naccess: workspace_write\n---\nTry to escalate.",
    )
    .expect("write escalator role");
    fs::write(
        ws_roles_dir.join("reviewer.md"),
        "---\ndriver: mock-a\naccess: read_only\n---\nReview code.",
    )
    .expect("write reviewer role");

    let drv = MockDriver::new("mock-a", true, true);
    let mut cfg = CoreConfig::new(home.path().to_path_buf());
    cfg.mcp_bin_path = Some(PathBuf::from("/usr/local/bin/dolgorae"));
    cfg.socket_path = Some(home.path().join("dolgorae.sock"));

    let engine = CoreEngine::new(cfg, vec![drv.clone()]).await.expect("init");

    // Root session with max_delegation_depth = 1, role = reviewer, ReadOnly, allow_delegation = true.
    let mut root_req = base_create_req(ws.path(), "mock-a");
    root_req.role = Some("reviewer".to_owned());
    root_req.allow_delegation = true;
    root_req.policy = Some(SessionPolicy {
        max_delegation_depth: 1,
        allow_actor_collaboration: true,
    });
    let root = engine
        .create_session(&CallerContext::ExternalHost, root_req)
        .await
        .expect("root");

    // Verify Scoped MCP injection passed DOLGORAE_ACTOR_TOKEN and it authenticates!
    let specs = drv.start_specs();
    assert_eq!(specs[0].mcp_servers.len(), 1);
    let injected_tok = specs[0].mcp_servers[0]
        .env
        .get("DOLGORAE_ACTOR_TOKEN")
        .expect("injected actor token")
        .clone();
    let root_scope = engine
        .authenticate_actor_token(&injected_tok)
        .await
        .expect("auth injected token");
    assert_eq!(root_scope.session_id, root.session_id);
    assert_eq!(root_scope.root_session_id, root.session_id);

    // 1. Role trust boundary: ReadOnly actor CANNOT escalate via workspace role frontmatter!
    let mut esc_role_req = base_create_req(ws.path(), "mock-a");
    esc_role_req.role = Some("escalator".to_owned());
    esc_role_req.access = None; // comes from escalator.md (workspace_write)
    let esc_err = engine
        .create_session(
            &CallerContext::ScopedActor(root_scope.clone()),
            esc_role_req,
        )
        .await
        .unwrap_err();
    assert_eq!(esc_err.code, ErrorCode::ForbiddenScope);

    // 2. Explicit access escalation ReadOnly -> WorkspaceWrite rejected.
    let mut esc_explicit = base_create_req(ws.path(), "mock-a");
    esc_explicit.access = Some(AccessMode::WorkspaceWrite);
    assert_eq!(
        engine
            .create_session(
                &CallerContext::ScopedActor(root_scope.clone()),
                esc_explicit
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // 3. Workspace escape outside parent canonical workspace -> 403 FORBIDDEN_SCOPE.
    let escape_req = base_create_req(outside_ws.path(), "mock-a");
    assert_eq!(
        engine
            .create_session(&CallerContext::ScopedActor(root_scope.clone()), escape_req)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // 4. Ancestor (role, canonical_workspace) re-entry -> 409 DELEGATION_CYCLE.
    let mut cycle_req = base_create_req(ws.path(), "mock-a");
    cycle_req.role = Some("reviewer".to_owned());
    assert_eq!(
        engine
            .create_session(&CallerContext::ScopedActor(root_scope.clone()), cycle_req)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DelegationCycle
    );

    // 5. Valid child in sub-workspace succeeds (depth = 1).
    let child_req = base_create_req(&sub_ws, "mock-a");
    let child = engine
        .create_session(&CallerContext::ScopedActor(root_scope.clone()), child_req)
        .await
        .expect("child depth 1");
    assert_eq!(child.depth, 1);

    // 6. Grandchild exceeds max_delegation_depth = 1 -> 409 DELEGATION_DEPTH_EXCEEDED.
    let child_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child.session_id)
        .await
        .expect("child tok");
    let child_scope = engine
        .authenticate_actor_token(&child_tok)
        .await
        .expect("auth child");
    let grandchild_req = base_create_req(&sub_ws, "mock-a");
    assert_eq!(
        engine
            .create_session(
                &CallerContext::ScopedActor(child_scope.clone()),
                grandchild_req
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::DelegationDepthExceeded
    );

    // 7. Cyclic wait_sessions: root waits on child, then child tries to wait on root -> 409 DELEGATION_CYCLE.
    let engine = Arc::new(engine);
    let eng_wait = Arc::clone(&engine);
    let root_scope_wait = root_scope.clone();
    let child_id = child.session_id.clone();
    let wait_task = tokio::spawn(async move {
        eng_wait
            .wait_sessions(
                &CallerContext::ScopedActor(root_scope_wait),
                WaitSessionsRequest {
                    waiter_session_id: None,
                    session_ids: vec![child_id],
                    mode: WaitMode::All,
                    timeout_ms: 2_000,
                },
            )
            .await
    });

    // Give the wait task a moment to register in the wait graph.
    tokio::time::sleep(Duration::from_millis(40)).await;

    let cycle_wait_err = engine
        .wait_sessions(
            &CallerContext::ScopedActor(child_scope),
            WaitSessionsRequest {
                waiter_session_id: None,
                session_ids: vec![root.session_id.clone()],
                mode: WaitMode::All,
                timeout_ms: 500,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(cycle_wait_err.code, ErrorCode::DelegationCycle);

    // Close child -> unblocks root's wait_sessions AND revokes child's token!
    engine
        .close_session(&CallerContext::ExternalHost, &child.session_id)
        .await
        .expect("close child");
    let wait_res = wait_task.await.expect("join wait").expect("wait ok");
    assert!(!wait_res.timed_out);
    assert_eq!(
        wait_res.completed_session_ids,
        vec![child.session_id.clone()]
    );

    // 8. Token revocation on close -> 401 UNAUTHORIZED.
    let rev_err = engine
        .authenticate_actor_token(&child_tok)
        .await
        .unwrap_err();
    assert_eq!(rev_err.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn unified_mailbox_isolation_policy_and_artifact_spill() {
    let home = TempDir::new("mbox-home");
    let ws = TempDir::new("mbox-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
        .await
        .expect("init");

    // Tree 1: allow_actor_collaboration = false.
    let mut root1_req = base_create_req(ws.path(), "mock-a");
    root1_req.policy = Some(SessionPolicy {
        max_delegation_depth: 3,
        allow_actor_collaboration: false,
    });
    let root1 = engine
        .create_session(&CallerContext::ExternalHost, root1_req)
        .await
        .expect("root1");
    let mut c1_req = base_create_req(ws.path(), "mock-a");
    c1_req.parent_session_id = Some(root1.session_id.clone());
    let child1a = engine
        .create_session(&CallerContext::ExternalHost, c1_req.clone())
        .await
        .expect("child1a");
    let child1b = engine
        .create_session(&CallerContext::ExternalHost, c1_req)
        .await
        .expect("child1b");

    let c1a_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child1a.session_id)
        .await
        .unwrap();
    let c1a_scope = engine.authenticate_actor_token(&c1a_tok).await.unwrap();

    // Direct child -> parent succeeds even when allow_actor_collaboration == false!
    let msg_up = engine
        .post_mailbox_message(
            &CallerContext::ScopedActor(c1a_scope.clone()),
            &root1.session_id,
            PostMailboxRequest {
                from_session_id: None,
                subject: Some("report".to_owned()),
                body: "all clear".to_owned(),
                artifact_ids: Vec::new(),
            },
        )
        .await
        .expect("child -> parent");
    assert_eq!(msg_up.inline_body.as_deref(), Some("all clear"));

    // Peer child1a -> child1b rejected when allow_actor_collaboration == false -> 403 FORBIDDEN_SCOPE.
    let peer_err = engine
        .post_mailbox_message(
            &CallerContext::ScopedActor(c1a_scope.clone()),
            &child1b.session_id,
            PostMailboxRequest {
                from_session_id: None,
                subject: None,
                body: "hi sibling".to_owned(),
                artifact_ids: Vec::new(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(peer_err.code, ErrorCode::ForbiddenScope);

    // Tree 2: allow_actor_collaboration = true.
    let root2 = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .expect("root2");
    let mut c2_req = base_create_req(ws.path(), "mock-a");
    c2_req.parent_session_id = Some(root2.session_id.clone());
    let child2a = engine
        .create_session(&CallerContext::ExternalHost, c2_req.clone())
        .await
        .expect("child2a");
    let child2b = engine
        .create_session(&CallerContext::ExternalHost, c2_req)
        .await
        .expect("child2b");

    let c2a_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child2a.session_id)
        .await
        .unwrap();
    let c2a_scope = engine.authenticate_actor_token(&c2a_tok).await.unwrap();

    // Cross-tree child1a -> root2 rejected -> 403 FORBIDDEN_SCOPE.
    assert_eq!(
        engine
            .post_mailbox_message(
                &CallerContext::ScopedActor(c1a_scope),
                &root2.session_id,
                PostMailboxRequest {
                    from_session_id: None,
                    subject: None,
                    body: "cross tree".to_owned(),
                    artifact_ids: Vec::new(),
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // Same-tree peer child2a -> child2b with >256 KiB body succeeds & spills to artifact!
    let big_body = "A".repeat(MAX_INLINE_BYTES + 128);
    let spilled = engine
        .post_mailbox_message(
            &CallerContext::ScopedActor(c2a_scope.clone()),
            &child2b.session_id,
            PostMailboxRequest {
                from_session_id: None,
                subject: Some("big".to_owned()),
                body: big_body.clone(),
                artifact_ids: Vec::new(),
            },
        )
        .await
        .expect("peer spilled");
    assert_eq!(spilled.inline_body, None);
    let art = spilled.artifact.expect("spilled artifact");
    assert_eq!(art.byte_len as usize, big_body.len());

    let (_, read_back) = engine
        .read_artifact(
            &CallerContext::ScopedActor(c2a_scope),
            &art.artifact_id,
            0,
            big_body.len(),
        )
        .await
        .expect("read spilled artifact");
    assert_eq!(read_back, big_body.as_bytes());

    let page = engine
        .list_mailbox_messages(&CallerContext::ExternalHost, &child2b.session_id, None, 10)
        .await
        .expect("list mailbox");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.highest_cursor, spilled.cursor);
}

#[tokio::test]
async fn streaming_artifacts_permissions_size_cap_and_tree_isolation() {
    let home = TempDir::new("art-home");
    let ws = TempDir::new("art-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
        .await
        .expect("init");

    // Verify 0700 home and 0600 state.db permissions.
    let home_mode = fs::metadata(home.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(home_mode, 0o700);
    let db_mode = fs::metadata(home.path().join("state.db"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(db_mode, 0o600);

    // Two distinct session trees.
    let root_a = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();
    let root_b = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();
    let tok_a = engine
        .mint_actor_token(&CallerContext::ExternalHost, &root_a.session_id)
        .await
        .unwrap();
    let scope_a = engine.authenticate_actor_token(&tok_a).await.unwrap();
    let tok_b = engine
        .mint_actor_token(&CallerContext::ExternalHost, &root_b.session_id)
        .await
        .unwrap();
    let scope_b = engine.authenticate_actor_token(&tok_b).await.unwrap();

    // Tree A uploads artifact -> verify 0600 blob permissions and bounded slice read.
    let art = engine
        .create_artifact_bytes(
            &CallerContext::ScopedActor(scope_a.clone()),
            "text/plain",
            b"0123456789abcdef".to_vec(),
        )
        .await
        .expect("create artifact");
    let blob_path = home
        .path()
        .join("artifacts")
        .join(&art.sha256[..2])
        .join(&art.sha256);
    let blob_mode = fs::metadata(&blob_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(blob_mode, 0o600);

    let (_, slice) = engine
        .read_artifact(&CallerContext::ScopedActor(scope_a), &art.artifact_id, 4, 6)
        .await
        .expect("slice read");
    assert_eq!(slice, b"456789");

    // Tree B reading Tree A's artifact -> 403 FORBIDDEN_SCOPE.
    let cross_err = engine
        .read_artifact(
            &CallerContext::ScopedActor(scope_b),
            &art.artifact_id,
            0,
            16,
        )
        .await
        .unwrap_err();
    assert_eq!(cross_err.code, ErrorCode::ForbiddenScope);

    // Streaming >64 MiB upload -> 413 PAYLOAD_TOO_LARGE without OOM.
    let oversized_reader = RepeatingReader {
        remaining: MAX_ARTIFACT_BYTES + 1,
        byte: b'z',
    };
    let big_err = engine
        .create_artifact(
            &CallerContext::ExternalHost,
            "application/octet-stream".to_owned(),
            oversized_reader,
        )
        .await
        .unwrap_err();
    assert_eq!(big_err.code, ErrorCode::PayloadTooLarge);
}

#[tokio::test]
async fn role_loader_precedence_guards_and_non_native_output_schema() {
    let home = TempDir::new("role-home");
    let ws = TempDir::new("role-ws");

    // Global role vs workspace role precedence.
    let global_roles = home.path().join("roles");
    let ws_roles = ws.path().join(".dolgorae").join("roles");
    fs::create_dir_all(&global_roles).unwrap();
    fs::create_dir_all(&ws_roles).unwrap();

    fs::write(
        global_roles.join("auditor.md"),
        "---\ndriver: mock-nonnative\nmodel: global-m\n---\nGlobal instructions.",
    )
    .unwrap();
    fs::write(
        ws_roles.join("auditor.md"),
        "---\ndriver: mock-nonnative\nmodel: ws-m\n---\nWorkspace instructions.",
    )
    .unwrap();
    fs::write(
        ws_roles.join("oversized.md"),
        "x".repeat(dolgorae_core::MAX_ROLE_FILE_BYTES + 1),
    )
    .unwrap();

    let drv = MockDriver::new("mock-nonnative", true, false);
    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![drv.clone()],
    )
    .await
    .unwrap();

    let role = engine
        .get_role("auditor", Some(ws.path()))
        .expect("load role");
    assert_eq!(role.summary.source, RoleSource::Workspace);
    assert_eq!(role.summary.model.as_deref(), Some("ws-m"));
    assert_eq!(role.summary.snapshot_sha256.len(), 64);
    assert_eq!(role.instructions, "Workspace instructions.");

    // Traversal rejected.
    assert_eq!(
        engine
            .get_role("../escape", Some(ws.path()))
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );
    // >64 KiB role rejected.
    assert_eq!(
        engine
            .get_role("oversized", Some(ws.path()))
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );

    // Non-native output_schema validation: records {validated, errors} without failing turn.
    let schema = json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {
            "verdict": { "enum": ["pass", "fail"] }
        }
    });
    let mut req = base_create_req(ws.path(), "mock-nonnative");
    req.output_schema = Some(schema);
    let snap = engine
        .create_session(&CallerContext::ExternalHost, req)
        .await
        .unwrap();
    let turn = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "audit".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    let ctrl = drv.last_control();
    // Invalid schema output still completes turn without failing it.
    ctrl.emit(NormalizedEvent::AssistantMessage {
        turn_id: turn.turn_id.clone(),
        item_id: Some("a1".to_owned()),
        text: r#"{"verdict":"maybe"}"#.to_owned(),
        structured_output: None,
    })
    .await;
    ctrl.emit(NormalizedEvent::TurnCompleted {
        turn_id: turn.turn_id.clone(),
        status: TurnTerminalStatus::Completed,
        error: None,
    })
    .await;

    wait_for_condition(|| async {
        engine
            .get_session(&CallerContext::ExternalHost, &snap.session_id)
            .await
            .is_ok_and(|s| s.active_turn_id.is_none())
    })
    .await;

    let page = engine
        .get_timeline(&CallerContext::ExternalHost, &snap.session_id, None, 20)
        .await
        .unwrap();
    let msg_item = page
        .items
        .iter()
        .find(|i| i.kind == "assistant_message")
        .expect("assistant_message item");
    let payload = msg_item.payload.as_ref().unwrap();
    assert_eq!(payload["schema_validation"]["validated"], json!(false));
    assert!(
        !payload["schema_validation"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn wait_any_all_writer_lease_and_daemon_restart_recovery() {
    let home = TempDir::new("lifecycle-home");
    let ws = TempDir::new("lifecycle-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = Arc::new(
        CoreEngine::new(
            CoreConfig::new(home.path().to_path_buf()),
            vec![drv.clone()],
        )
        .await
        .unwrap(),
    );

    // Writer lease: create two WorkspaceWrite sessions before acquiring lease,
    // verify conflict on new WorkspaceWrite session, transfer, and auto-release on close.
    let mut s1_req = base_create_req(ws.path(), "mock-a");
    s1_req.access = Some(AccessMode::WorkspaceWrite);
    let s1 = engine
        .create_session(&CallerContext::ExternalHost, s1_req.clone())
        .await
        .unwrap();
    let s2 = engine
        .create_session(&CallerContext::ExternalHost, s1_req.clone())
        .await
        .unwrap();
    let lease = engine
        .acquire_writer_lease(&CallerContext::ExternalHost, ws.path(), &s1.session_id)
        .await
        .expect("acquire lease");
    assert_eq!(lease.holder_session_id, s1.session_id);

    // Creating another WorkspaceWrite session while lease held -> 409 SNAPSHOT_CONFLICT.
    assert_eq!(
        engine
            .create_session(&CallerContext::ExternalHost, s1_req.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    // Transfer lease from s1 to s2 (both WorkspaceWrite on ws).
    let transferred = engine
        .transfer_writer_lease(
            &CallerContext::ExternalHost,
            ws.path(),
            &s1.session_id,
            &s2.session_id,
        )
        .await
        .expect("transfer lease");
    assert_eq!(transferred.holder_session_id, s2.session_id);

    // Closing s2 auto-releases the writer lease so a new WorkspaceWrite session succeeds!
    engine
        .close_session(&CallerContext::ExternalHost, &s2.session_id)
        .await
        .unwrap();
    let s3 = engine
        .create_session(&CallerContext::ExternalHost, s1_req)
        .await
        .expect("workspace_write after auto-release");

    // wait_sessions(Any) and wait_sessions(All): submit turns on s1 & s3.
    let t1 = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &s1.session_id,
            SubmitTurnRequest {
                text: "job1".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();
    let _t3 = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &s3.session_id,
            SubmitTurnRequest {
                text: "job3".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    let ctrl1 = drv.control_at(1);
    let eng_any = Arc::clone(&engine);
    let s1_id = s1.session_id.clone();
    let s3_id = s3.session_id.clone();
    let any_handle = tokio::spawn(async move {
        eng_any
            .wait_sessions(
                &CallerContext::ExternalHost,
                WaitSessionsRequest {
                    waiter_session_id: None,
                    session_ids: vec![s1_id, s3_id],
                    mode: WaitMode::Any,
                    timeout_ms: 3_000,
                },
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    ctrl1
        .emit(NormalizedEvent::TurnCompleted {
            turn_id: t1.turn_id,
            status: TurnTerminalStatus::Completed,
            error: None,
        })
        .await;

    let any_res = any_handle.await.unwrap().unwrap();
    assert!(!any_res.timed_out);
    assert_eq!(any_res.completed_session_ids, vec![s1.session_id.clone()]);

    // Simulate daemon crash/restart while s3 still has an active turn and a pending interaction!
    let ctrl3 = drv.control_at(3);
    ctrl3
        .emit(NormalizedEvent::InteractionRequested {
            request: InteractionRequest {
                interaction_id: "int-crash".to_owned(),
                turn_id: _t3.turn_id.clone(),
                kind: InteractionKind::PermissionRequest {
                    permissions: vec!["net".to_owned()],
                    reason: None,
                },
                original_kind: "permission_request".to_owned(),
                title: "Need net".to_owned(),
                decisions: Vec::new(),
                protected: false,
                response_schema: None,
                max_response_bytes: None,
            },
        })
        .await;
    wait_for_condition(|| async {
        engine
            .list_interactions(
                &CallerContext::ExternalHost,
                &s3.session_id,
                Some(InteractionStatus::Pending),
            )
            .await
            .is_ok_and(|l| l.interactions.len() == 1)
    })
    .await;

    let s3_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &s3.session_id)
        .await
        .unwrap();

    // Drop old engine cleanly (exercises Store writer-thread Drop join).
    drop(engine);

    // Reopen engine on the same home_dir -> triggers startup crash recovery!
    let recovered_engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![MockDriver::new("mock-a", true, true)],
    )
    .await
    .expect("reopen engine");

    let s3_after = recovered_engine
        .get_session(&CallerContext::ExternalHost, &s3.session_id)
        .await
        .unwrap();
    assert_eq!(s3_after.status, SessionStatus::Idle);
    assert_eq!(s3_after.active_turn_id, None);
    assert_eq!(s3_after.session_generation, 2);
    assert_eq!(s3_after.pending_interaction_count, 0);

    // Pending interaction is now Expired, and actor token is Revoked (401).
    let ints_after = recovered_engine
        .list_interactions(&CallerContext::ExternalHost, &s3.session_id, None)
        .await
        .unwrap();
    assert_eq!(
        ints_after.interactions[0].status,
        InteractionStatus::Expired
    );
    assert_eq!(
        recovered_engine
            .authenticate_actor_token(&s3_tok)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );

    // Timeline includes durable `interaction_expired` and `session_recovered` events.
    let tl = recovered_engine
        .get_timeline(&CallerContext::ExternalHost, &s3.session_id, None, 50)
        .await
        .unwrap();
    assert!(tl.items.iter().any(|i| i.kind == "interaction_expired"));
    assert!(tl.items.iter().any(|i| {
        i.kind == "session_recovered"
            && i.payload.as_ref().and_then(|p| p.get("recovery_status"))
                == Some(&json!("interrupted_unknown"))
    }));

    // Resume s3 -> bumps generation to 3 and transitions back to Active!
    let resumed = recovered_engine
        .resume_session(
            &CallerContext::ExternalHost,
            &s3.session_id,
            ResumeSessionRequest::default(),
        )
        .await
        .expect("resume after recovery");
    assert_eq!(resumed.status, SessionStatus::Active);
    assert_eq!(resumed.session_generation, 3);
}

#[tokio::test]
async fn all_ten_canonical_error_codes_and_driver_faults() {
    let home = TempDir::new("errors-home");
    let ws = TempDir::new("errors-ws");
    let unavailable = MockDriver::new("mock-down", false, true);
    let faulty = MockDriver::new("mock-faulty", true, true);
    faulty
        .fail_send_turn_with_protocol
        .store(true, Ordering::Relaxed);

    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![unavailable, faulty],
    )
    .await
    .unwrap();

    // Verify canonical HTTP status mapping for all 10 codes.
    let expected = [
        (ErrorCode::BadRequest, 400, "BAD_REQUEST"),
        (ErrorCode::Unauthorized, 401, "UNAUTHORIZED"),
        (ErrorCode::ForbiddenScope, 403, "FORBIDDEN_SCOPE"),
        (ErrorCode::NotFound, 404, "NOT_FOUND"),
        (ErrorCode::SnapshotConflict, 409, "SNAPSHOT_CONFLICT"),
        (
            ErrorCode::DelegationDepthExceeded,
            409,
            "DELEGATION_DEPTH_EXCEEDED",
        ),
        (ErrorCode::DelegationCycle, 409, "DELEGATION_CYCLE"),
        (ErrorCode::PayloadTooLarge, 413, "PAYLOAD_TOO_LARGE"),
        (ErrorCode::DriverUnavailable, 503, "DRIVER_UNAVAILABLE"),
        (ErrorCode::InternalError, 500, "INTERNAL_ERROR"),
    ];
    for (code, status, name) in expected {
        assert_eq!(code.http_status(), status);
        assert_eq!(code.as_str(), name);
    }

    // 503 DRIVER_UNAVAILABLE
    assert_eq!(
        engine
            .create_session(
                &CallerContext::ExternalHost,
                base_create_req(ws.path(), "mock-down")
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::DriverUnavailable
    );

    // 404 NOT_FOUND
    assert_eq!(
        engine
            .get_session(&CallerContext::ExternalHost, "sess-does-not-exist")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    // 401 UNAUTHORIZED
    assert_eq!(
        engine
            .authenticate_actor_token("bogus-token")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );

    // 500 INTERNAL_ERROR via simulated driver protocol fault.
    let s = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-faulty"),
        )
        .await
        .unwrap();
    let err = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &s.session_id,
            SubmitTurnRequest {
                text: "trigger fault".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InternalError);
    assert_eq!(err.to_envelope().error.code, ErrorCode::InternalError);

    // Capabilities lists both drivers accurately.
    let caps = engine.capabilities().await.unwrap();
    assert_eq!(caps.api_version, "v2");
    assert_eq!(caps.drivers.len(), 2);

    // List sessions works.
    let sessions = engine
        .list_sessions(&CallerContext::ExternalHost, SessionFilter::default())
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
}

#[tokio::test]
async fn scoped_actor_per_operation_authorization_blocks_root_and_sibling() {
    let home = TempDir::new("authz-home");
    let ws = TempDir::new("authz-ws");
    let outside_ws = TempDir::new("authz-outside");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
        .await
        .unwrap();

    let root = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();

    // Even ExternalHost cannot spawn a child whose workspace escapes its parent.
    let mut escape_req = base_create_req(outside_ws.path(), "mock-a");
    escape_req.parent_session_id = Some(root.session_id.clone());
    assert_eq!(
        engine
            .create_session(&CallerContext::ExternalHost, escape_req)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    let mut child_req = base_create_req(ws.path(), "mock-a");
    child_req.parent_session_id = Some(root.session_id.clone());
    let child_a = engine
        .create_session(&CallerContext::ExternalHost, child_req.clone())
        .await
        .unwrap();
    let child_b = engine
        .create_session(&CallerContext::ExternalHost, child_req)
        .await
        .unwrap();

    let turn_b = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &child_b.session_id,
            SubmitTurnRequest {
                text: "sibling task".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    let root_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &root.session_id)
        .await
        .unwrap();
    let root_scope = engine.authenticate_actor_token(&root_tok).await.unwrap();
    let child_a_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child_a.session_id)
        .await
        .unwrap();
    let child_a_scope = engine.authenticate_actor_token(&child_a_tok).await.unwrap();

    // child_a -> root close_session => 403 FORBIDDEN_SCOPE
    assert_eq!(
        engine
            .close_session(
                &CallerContext::ScopedActor(child_a_scope.clone()),
                &root.session_id
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // child_a -> sibling interrupt_turn => 403 FORBIDDEN_SCOPE
    assert_eq!(
        engine
            .interrupt_turn(
                &CallerContext::ScopedActor(child_a_scope.clone()),
                &child_b.session_id,
                &turn_b.turn_id,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // child_a -> root submit_turn => 403 FORBIDDEN_SCOPE
    assert_eq!(
        engine
            .submit_turn(
                &CallerContext::ScopedActor(child_a_scope.clone()),
                &root.session_id,
                SubmitTurnRequest {
                    text: "hijack root".to_owned(),
                    effort: None,
                    attachments: Vec::new(),
                    output_schema_override: None,
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // child_a -> sibling get_session => 403 FORBIDDEN_SCOPE
    assert_eq!(
        engine
            .get_session(
                &CallerContext::ScopedActor(child_a_scope.clone()),
                &child_b.session_id
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // child_a -> self close_session => 403 FORBIDDEN_SCOPE; child_a -> self get_session => Ok
    assert_eq!(
        engine
            .close_session(
                &CallerContext::ScopedActor(child_a_scope.clone()),
                &child_a.session_id
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );
    let self_snap = engine
        .get_session(
            &CallerContext::ScopedActor(child_a_scope),
            &child_a.session_id,
        )
        .await
        .expect("self get_session allowed");
    assert_eq!(self_snap.session_id, child_a.session_id);

    // Direct parent -> child submit_turn, get_session, close_session all succeed!
    let parent_turn = engine
        .submit_turn(
            &CallerContext::ScopedActor(root_scope.clone()),
            &child_a.session_id,
            SubmitTurnRequest {
                text: "direct child work".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .expect("parent submit_turn to direct child");
    assert!(!parent_turn.turn_id.is_empty());

    engine
        .close_session(&CallerContext::ScopedActor(root_scope), &child_a.session_id)
        .await
        .expect("parent close direct child");
}

#[tokio::test]
async fn answer_interaction_driver_error_does_not_commit_resolved() {
    let home = TempDir::new("ans-err-home");
    let ws = TempDir::new("ans-err-ws");
    let drv = MockDriver::new("mock-a", true, true);
    drv.fail_answer_with_invalid_config
        .store(true, Ordering::Relaxed);

    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![drv.clone()],
    )
    .await
    .unwrap();

    let snap = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();
    let turn = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "ask approval".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    let ctrl = drv.last_control();
    ctrl.emit(NormalizedEvent::InteractionRequested {
        request: InteractionRequest {
            interaction_id: "int-reject".to_owned(),
            turn_id: turn.turn_id.clone(),
            kind: InteractionKind::CommandApproval {
                command: vec!["ls".to_owned()],
                cwd: None,
                reason: None,
            },
            original_kind: "command_approval".to_owned(),
            title: "Run ls".to_owned(),
            decisions: vec![DecisionOption {
                decision_id: "allow".to_owned(),
                label: "Allow".to_owned(),
                kind: DecisionKind::AllowOnce,
            }],
            protected: false,
            response_schema: None,
            max_response_bytes: None,
        },
    })
    .await;

    wait_for_condition(|| async {
        engine
            .list_interactions(
                &CallerContext::ExternalHost,
                &snap.session_id,
                Some(InteractionStatus::Pending),
            )
            .await
            .is_ok_and(|l| l.interactions.len() == 1)
    })
    .await;

    let list = engine
        .list_interactions(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(InteractionStatus::Pending),
        )
        .await
        .unwrap();
    let rec = &list.interactions[0];

    let err = engine
        .answer_interaction(
            &CallerContext::ExternalHost,
            &snap.session_id,
            "int-reject",
            AnswerInteractionRequest {
                turn_id: turn.turn_id,
                session_generation: rec.session_generation,
                expected_interaction_revision: list.interaction_revision,
                expected_session_revision: list.session_revision,
                decision_id: "allow".to_owned(),
                answers: BTreeMap::new(),
                payload: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);

    // Durable state MUST NOT claim `resolved` after driver delivery failed.
    let after = engine
        .list_interactions(&CallerContext::ExternalHost, &snap.session_id, None)
        .await
        .unwrap();
    assert_eq!(after.interactions.len(), 1);
    assert_eq!(after.interactions[0].status, InteractionStatus::Pending);
    assert_eq!(after.interaction_revision, list.interaction_revision);

    let timeline = engine
        .get_timeline(&CallerContext::ExternalHost, &snap.session_id, None, 20)
        .await
        .unwrap();
    assert!(
        timeline
            .items
            .iter()
            .all(|item| item.kind != "interaction_resolved"),
        "must not emit interaction_resolved when driver rejected answer"
    );
}

#[tokio::test]
async fn turn_start_and_fast_complete_before_send_turn_returns() {
    let home = TempDir::new("fast-turn-home");
    let ws = TempDir::new("fast-turn-ws");
    // Use a non-native output_schema driver so we also prove per-turn
    // `output_schema_override` was persisted in `reserve_active_turn` BEFORE
    // early `AssistantMessage` drained!
    let drv = MockDriver::new("mock-a", true, false);
    drv.emit_start_and_complete_before_ack
        .store(true, Ordering::Relaxed);

    let engine = CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
        .await
        .unwrap();

    // Session default schema requires `session_only` (would fail `{"override_ok":true}`).
    let mut create_req = base_create_req(ws.path(), "mock-a");
    create_req.output_schema = Some(json!({
        "type": "object",
        "required": ["session_only"],
        "properties": { "session_only": { "type": "string" } }
    }));
    let snap = engine
        .create_session(&CallerContext::ExternalHost, create_req)
        .await
        .unwrap();

    // Per-turn override schema requires `override_ok: true` (matches the fast AssistantMessage!).
    let override_schema = json!({
        "type": "object",
        "required": ["override_ok"],
        "additionalProperties": false,
        "properties": { "override_ok": { "const": true } }
    });

    // Turn 1 emits TurnStarted + AssistantMessage + TurnCompleted BEFORE send_turn returns TurnAck!
    let t1 = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "instant turn 1".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: Some(override_schema),
            },
        )
        .await
        .expect("submit fast turn 1");

    let after_t1 = engine
        .get_session(&CallerContext::ExternalHost, &snap.session_id)
        .await
        .unwrap();
    assert_eq!(
        after_t1.active_turn_id, None,
        "completed fast turn must not leak active_turn_id"
    );

    let tl = engine
        .get_timeline(&CallerContext::ExternalHost, &snap.session_id, None, 50)
        .await
        .unwrap();
    let started_items: Vec<_> = tl
        .items
        .iter()
        .filter(|i| i.kind == "turn_started" && i.turn_id.as_deref() == Some(t1.turn_id.as_str()))
        .collect();
    assert_eq!(
        started_items.len(),
        1,
        "turn_started must be emitted exactly once"
    );
    assert_eq!(
        started_items[0].inline_text.as_deref(),
        Some("instant turn 1"),
        "durable turn_started must carry reserved user prompt even when drain runs first"
    );

    let msg_item = tl
        .items
        .iter()
        .find(|i| {
            i.kind == "assistant_message" && i.turn_id.as_deref() == Some(t1.turn_id.as_str())
        })
        .expect("assistant_message item");
    assert_eq!(
        msg_item.payload.as_ref().unwrap()["schema_validation"]["validated"],
        json!(true),
        "early AssistantMessage must validate against reserved per-turn output_schema_override"
    );

    assert!(tl.items.iter().any(|i| {
        i.kind == "turn_completed" && i.turn_id.as_deref() == Some(t1.turn_id.as_str())
    }));

    // Subsequent submit_turn MUST succeed (not 409 SNAPSHOT_CONFLICT).
    let t2 = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "instant turn 2".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .expect("subsequent submit_turn must succeed");
    assert_ne!(t1.turn_id, t2.turn_id);
}

#[tokio::test]
async fn resume_rotates_actor_token_and_reinjects_scoped_mcp() {
    let home = TempDir::new("resume-tok-home");
    let ws = TempDir::new("resume-tok-ws");
    let drv1 = MockDriver::new("mock-a", true, true);

    let mut cfg = CoreConfig::new(home.path().to_path_buf());
    cfg.mcp_bin_path = Some(PathBuf::from("/usr/local/bin/dolgorae"));
    cfg.socket_path = Some(home.path().join("dolgorae.sock"));

    let engine1 = CoreEngine::new(cfg.clone(), vec![drv1.clone()])
        .await
        .unwrap();
    let mut req = base_create_req(ws.path(), "mock-a");
    req.allow_delegation = true;
    let snap = engine1
        .create_session(&CallerContext::ExternalHost, req)
        .await
        .unwrap();

    let token_1 = drv1.start_specs()[0].mcp_servers[0]
        .env
        .get("DOLGORAE_ACTOR_TOKEN")
        .expect("token 1 injected")
        .clone();
    assert_eq!(
        engine1
            .authenticate_actor_token(&token_1)
            .await
            .expect("token 1 valid before restart")
            .session_id,
        snap.session_id
    );

    // Simulate daemon restart -> session becomes Idle, token_1 revoked.
    drop(engine1);

    let drv2 = MockDriver::new("mock-a", true, true);
    let engine2 = CoreEngine::new(cfg, vec![drv2.clone()]).await.unwrap();
    assert_eq!(
        engine2
            .authenticate_actor_token(&token_1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );

    // Resume rotates token and re-injects scoped MCP into ResumeSpec.
    let resumed1 = engine2
        .resume_session(
            &CallerContext::ExternalHost,
            &snap.session_id,
            ResumeSessionRequest::default(),
        )
        .await
        .expect("resume after restart");
    assert_eq!(resumed1.status, SessionStatus::Active);

    let resume_specs = drv2.resume_specs();
    assert_eq!(resume_specs.len(), 1);
    assert_eq!(resume_specs[0].mcp_servers.len(), 1);
    let token_2 = resume_specs[0].mcp_servers[0]
        .env
        .get("DOLGORAE_ACTOR_TOKEN")
        .expect("token 2 injected on resume")
        .clone();
    assert_ne!(token_1, token_2);

    assert_eq!(
        engine2
            .authenticate_actor_token(&token_1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );
    let scope_2 = engine2
        .authenticate_actor_token(&token_2)
        .await
        .expect("token 2 authenticates after resume");
    assert_eq!(scope_2.session_id, snap.session_id);

    // Resuming again while active revokes token_2 and issues token_3.
    engine2
        .resume_session(
            &CallerContext::ExternalHost,
            &snap.session_id,
            ResumeSessionRequest::default(),
        )
        .await
        .expect("second resume");
    let token_3 = drv2.resume_specs()[1].mcp_servers[0]
        .env
        .get("DOLGORAE_ACTOR_TOKEN")
        .expect("token 3 injected")
        .clone();
    assert_ne!(token_2, token_3);
    assert_eq!(
        engine2
            .authenticate_actor_token(&token_2)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );
    assert_eq!(
        engine2
            .authenticate_actor_token(&token_3)
            .await
            .expect("token 3 valid")
            .session_id,
        snap.session_id
    );
}

#[tokio::test]
async fn answer_interaction_succeeds_when_driver_advances_revision_during_delivery() {
    let home = TempDir::new("ans-rev-home");
    let ws = TempDir::new("ans-rev-ws");
    let drv = MockDriver::new("mock-a", true, true);
    drv.emit_tool_activity_during_answer
        .store(true, Ordering::Relaxed);

    let engine = CoreEngine::new(
        CoreConfig::new(home.path().to_path_buf()),
        vec![drv.clone()],
    )
    .await
    .unwrap();

    let snap = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();
    let turn = engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &snap.session_id,
            SubmitTurnRequest {
                text: "run tool".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    let ctrl = drv.last_control();
    ctrl.emit(NormalizedEvent::InteractionRequested {
        request: InteractionRequest {
            interaction_id: "int-live".to_owned(),
            turn_id: turn.turn_id.clone(),
            kind: InteractionKind::CommandApproval {
                command: vec!["ls".to_owned()],
                cwd: None,
                reason: None,
            },
            original_kind: "command_approval".to_owned(),
            title: "Run ls".to_owned(),
            decisions: vec![DecisionOption {
                decision_id: "allow".to_owned(),
                label: "Allow".to_owned(),
                kind: DecisionKind::AllowOnce,
            }],
            protected: false,
            response_schema: None,
            max_response_bytes: None,
        },
    })
    .await;

    wait_for_condition(|| async {
        engine
            .list_interactions(
                &CallerContext::ExternalHost,
                &snap.session_id,
                Some(InteractionStatus::Pending),
            )
            .await
            .is_ok_and(|l| l.interactions.len() == 1)
    })
    .await;

    let list = engine
        .list_interactions(
            &CallerContext::ExternalHost,
            &snap.session_id,
            Some(InteractionStatus::Pending),
        )
        .await
        .unwrap();
    let rec = &list.interactions[0];

    // Mock driver emits durable ToolActivity (bumping session.revision) INSIDE
    // driver.answer_interaction before returning Ok(()). Commit must still succeed!
    let resp = engine
        .answer_interaction(
            &CallerContext::ExternalHost,
            &snap.session_id,
            "int-live",
            AnswerInteractionRequest {
                turn_id: turn.turn_id,
                session_generation: rec.session_generation,
                expected_interaction_revision: list.interaction_revision,
                expected_session_revision: list.session_revision,
                decision_id: "allow".to_owned(),
                answers: BTreeMap::new(),
                payload: None,
            },
        )
        .await
        .expect("answer_interaction must succeed even when driver emits ToolActivity mid-delivery");

    assert_eq!(resp.status, InteractionStatus::Resolved);
    // Revision advanced by ToolActivity (+1) AND interaction_resolved (+1).
    assert_eq!(resp.session_revision, list.session_revision + 2);
    assert_eq!(resp.head_cursor, Cursor(resp.session_revision));

    let tl = engine
        .get_timeline(&CallerContext::ExternalHost, &snap.session_id, None, 20)
        .await
        .unwrap();
    assert!(tl.items.iter().any(|i| i.kind == "tool_activity"));
    assert!(tl.items.iter().any(|i| i.kind == "interaction_resolved"));
}

#[tokio::test]
async fn wait_sessions_wakes_and_settles_on_mailbox_delivery() {
    let home = TempDir::new("wait-mbox-home");
    let ws = TempDir::new("wait-mbox-ws");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = Arc::new(
        CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
            .await
            .unwrap(),
    );

    let parent = engine
        .create_session(
            &CallerContext::ExternalHost,
            base_create_req(ws.path(), "mock-a"),
        )
        .await
        .unwrap();
    let mut child_req = base_create_req(ws.path(), "mock-a");
    child_req.parent_session_id = Some(parent.session_id.clone());
    let child = engine
        .create_session(&CallerContext::ExternalHost, child_req)
        .await
        .unwrap();

    // Start a long-running turn on child (never completes during this test).
    engine
        .submit_turn(
            &CallerContext::ExternalHost,
            &child.session_id,
            SubmitTurnRequest {
                text: "long running task".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .unwrap();

    // Oversized timeout_ms > 300_000 rejected with 400 BAD_REQUEST.
    assert_eq!(
        engine
            .wait_sessions(
                &CallerContext::ExternalHost,
                WaitSessionsRequest {
                    waiter_session_id: Some(parent.session_id.clone()),
                    session_ids: vec![child.session_id.clone()],
                    mode: WaitMode::Any,
                    timeout_ms: 300_001,
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );

    // Parent waits on child while child's turn is still running.
    let eng_wait = Arc::clone(&engine);
    let p_id = parent.session_id.clone();
    let c_id = child.session_id.clone();
    let wait_task = tokio::spawn(async move {
        eng_wait
            .wait_sessions(
                &CallerContext::ExternalHost,
                WaitSessionsRequest {
                    waiter_session_id: Some(p_id),
                    session_ids: vec![c_id],
                    mode: WaitMode::Any,
                    timeout_ms: 5_000,
                },
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(40)).await;

    // Child posts a mailbox message to parent -> wait_sessions wakes immediately!
    let child_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child.session_id)
        .await
        .unwrap();
    let child_scope = engine.authenticate_actor_token(&child_tok).await.unwrap();
    engine
        .post_mailbox_message(
            &CallerContext::ScopedActor(child_scope),
            &parent.session_id,
            PostMailboxRequest {
                from_session_id: None,
                subject: Some("checkpoint".to_owned()),
                body: "ready for review".to_owned(),
                artifact_ids: Vec::new(),
            },
        )
        .await
        .expect("child posts mailbox");

    let res = tokio::time::timeout(Duration::from_secs(2), wait_task)
        .await
        .expect("wait_sessions woke promptly")
        .expect("join wait")
        .expect("wait ok");
    assert!(!res.timed_out);
    assert_eq!(res.completed_session_ids, vec![child.session_id]);
}

#[tokio::test]
async fn writer_lease_containment_readonly_target_and_hardening_guards() {
    let home = TempDir::new("harden-home");
    let ws = TempDir::new("harden-ws");
    let other_ws = TempDir::new("harden-other");
    let drv = MockDriver::new("mock-a", true, true);
    let engine = CoreEngine::new(CoreConfig::new(home.path().to_path_buf()), vec![drv])
        .await
        .unwrap();

    // WorkspaceWrite root + WorkspaceWrite child + ReadOnly child.
    let mut root_req = base_create_req(ws.path(), "mock-a");
    root_req.access = Some(AccessMode::WorkspaceWrite);
    root_req.approval_policy = Some(ApprovalPolicy::Interactive);
    let root = engine
        .create_session(&CallerContext::ExternalHost, root_req)
        .await
        .unwrap();

    let root_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &root.session_id)
        .await
        .unwrap();
    let root_scope = engine.authenticate_actor_token(&root_tok).await.unwrap();

    // Hardening #5: child cannot escalate approval_policy from Interactive to AutoApproveScoped.
    let mut esc_policy = base_create_req(ws.path(), "mock-a");
    esc_policy.parent_session_id = Some(root.session_id.clone());
    esc_policy.approval_policy = Some(ApprovalPolicy::AutoApproveScoped);
    assert_eq!(
        engine
            .create_session(&CallerContext::ScopedActor(root_scope.clone()), esc_policy)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    let mut ro_child_req = base_create_req(ws.path(), "mock-a");
    ro_child_req.parent_session_id = Some(root.session_id.clone());
    ro_child_req.access = Some(AccessMode::ReadOnly);
    let ro_child = engine
        .create_session(&CallerContext::ExternalHost, ro_child_req)
        .await
        .unwrap();

    // B2: ScopedActor cannot acquire lease on another workspace -> 403 FORBIDDEN_SCOPE.
    assert_eq!(
        engine
            .acquire_writer_lease(
                &CallerContext::ScopedActor(root_scope.clone()),
                other_ws.path(),
                &root.session_id,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // Acquire valid lease on root's own workspace.
    engine
        .acquire_writer_lease(
            &CallerContext::ScopedActor(root_scope.clone()),
            ws.path(),
            &root.session_id,
        )
        .await
        .expect("acquire valid lease");

    // B2: Transfer to ReadOnly target rejected -> 403 FORBIDDEN_SCOPE.
    assert_eq!(
        engine
            .transfer_writer_lease(
                &CallerContext::ScopedActor(root_scope.clone()),
                ws.path(),
                &root.session_id,
                &ro_child.session_id,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // B2: Release lease on another workspace rejected -> 403 FORBIDDEN_SCOPE.
    assert_eq!(
        engine
            .release_writer_lease(
                &CallerContext::ScopedActor(root_scope.clone()),
                other_ws.path(),
                &root.session_id,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenScope
    );

    // Hardening #4: Workspace role symlink rejected on get_role and skipped cleanly in list_roles.
    let ws_roles = ws.path().join(".dolgorae").join("roles");
    fs::create_dir_all(&ws_roles).unwrap();
    fs::write(
        ws_roles.join("good.md"),
        "---\ndriver: mock-a\n---\nValid role.",
    )
    .unwrap();
    let target_file = other_ws.path().join("secret.md");
    fs::write(&target_file, "---\ndriver: mock-a\n---\nSecret.").unwrap();
    std::os::unix::fs::symlink(&target_file, ws_roles.join("link.md")).unwrap();

    assert_eq!(
        engine.get_role("link", Some(ws.path())).unwrap_err().code,
        ErrorCode::ForbiddenScope
    );
    let roles = engine
        .list_roles(Some(ws.path()))
        .expect("resilient list_roles");
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].name, "good");

    // Hardening #7: ExternalHost mailbox artifact attachment grants target tree read scope.
    let host_art = engine
        .create_artifact_bytes(
            &CallerContext::ExternalHost,
            "text/plain",
            b"attached-by-host".to_vec(),
        )
        .await
        .unwrap();
    engine
        .post_mailbox_message(
            &CallerContext::ExternalHost,
            &root.session_id,
            PostMailboxRequest {
                from_session_id: None,
                subject: Some("spec".to_owned()),
                body: "see attachment".to_owned(),
                artifact_ids: vec![host_art.artifact_id.clone()],
            },
        )
        .await
        .unwrap();
    let (_, attached_bytes) = engine
        .read_artifact(
            &CallerContext::ScopedActor(root_scope),
            &host_art.artifact_id,
            0,
            64,
        )
        .await
        .expect("scoped actor can read host-attached mailbox artifact");
    assert_eq!(attached_bytes, b"attached-by-host");
}
