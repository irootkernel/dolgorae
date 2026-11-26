//! Integration tests for `dolgorae-api`: exclusive `DaemonLock`, safe UDS
//! permissions, `LOCAL_PEERCRED` check, all 19 OpenAPI v2 routes, `8 MiB`/`64 MiB`
//! `413 PAYLOAD_TOO_LARGE` bounds, SSE `id:`/`Last-Event-ID`/`heartbeat`/`refresh_required`,
//! and real `RecvError::Lagged` no-silent-gap recovery.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use dolgorae_api::{
    ApiError, BoundUdsServer, DaemonLock, ServerConfig, UdsClient, verify_peer_uid,
};
use dolgorae_core::{
    AccessMode, AgentDriver, AnswerInteractionRequest, ApprovalPolicy, CallerContext,
    CapabilitiesSnapshot, CoreConfig, CoreEngine, CreateSessionRequest, Cursor, DecisionKind,
    DecisionOption, DriverCapabilities, DriverError, DriverSession, ErrorCode, InteractionAnswer,
    InteractionKind, InteractionList, InteractionRequest, MAX_ARTIFACT_BYTES, MAX_RECEIVE_BYTES,
    MailboxMessage, MailboxPage, NormalizedEvent, PostMailboxRequest, ResumeSpec, RoleDetail,
    SessionSpec, SessionStatus, SubmitTurnRequest, TimelinePage, TurnAck, TurnInput,
    TurnTerminalStatus, WaitMode, WaitSessionsRequest, WaitSessionsResponse, WriterLease,
};
use dolgorae_spi::{EventReceiver, EventSender, event_channel};
use hyper::body::Frame;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from("/tmp").join(format!("dg-a-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod 0700");
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

struct MockCtrl {
    tx: EventSender,
}

impl MockCtrl {
    async fn emit(&self, ev: NormalizedEvent) {
        self.tx.send(ev).await.expect("emit mock event");
    }
}

struct MockSession {
    prov_id: String,
    rx: Mutex<Option<EventReceiver>>,
    turn_seq: AtomicU64,
}

impl DriverSession for MockSession {
    fn provider_session_id(&self) -> &str {
        &self.prov_id
    }

    fn take_events(&mut self) -> Option<EventReceiver> {
        self.rx.lock().unwrap().take()
    }

    fn send_turn<'life0, 'async_trait>(
        &'life0 self,
        _input: TurnInput,
    ) -> Pin<Box<dyn Future<Output = Result<TurnAck, DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let n = self.turn_seq.fetch_add(1, Ordering::Relaxed) + 1;
            Ok(TurnAck {
                turn_id: format!("turn-{n}"),
            })
        })
    }

    fn interrupt_turn<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _turn_id: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Ok(()) })
    }

    fn answer_interaction<'life0, 'async_trait>(
        &'life0 self,
        _answer: InteractionAnswer,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Ok(()) })
    }

    fn close<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Ok(()) })
    }
}

struct MockDriver {
    seq: AtomicU64,
    ctrls: Mutex<HashMap<u64, Arc<MockCtrl>>>,
}

impl MockDriver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seq: AtomicU64::new(0),
            ctrls: Mutex::new(HashMap::new()),
        })
    }

    fn last_ctrl(&self) -> Arc<MockCtrl> {
        let n = self.seq.load(Ordering::Relaxed);
        self.ctrls.lock().unwrap().get(&n).cloned().expect("ctrl")
    }
}

impl AgentDriver for MockDriver {
    fn driver_id(&self) -> &'static str {
        "mock-drv"
    }

    fn probe<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = Result<DriverCapabilities, DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async {
            Ok(DriverCapabilities {
                driver_id: "mock-drv".to_owned(),
                cli_version: Some("1.0.0".to_owned()),
                available: true,
                unavailable_reason: None,
                models: Vec::new(),
                supports_effort: true,
                supports_interactive_approval: true,
                native_output_schema: true,
                supports_mcp_injection: true,
                supports_resume: true,
            })
        })
    }

    fn start_session<'life0, 'async_trait>(
        &'life0 self,
        _spec: SessionSpec,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn DriverSession>, DriverError>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let (tx, rx) = event_channel(64);
            self.ctrls
                .lock()
                .unwrap()
                .insert(n, Arc::new(MockCtrl { tx }));
            Ok(Box::new(MockSession {
                prov_id: format!("prov-{n}"),
                rx: Mutex::new(Some(rx)),
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
            let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let (tx, rx) = event_channel(64);
            self.ctrls
                .lock()
                .unwrap()
                .insert(n, Arc::new(MockCtrl { tx }));
            Ok(Box::new(MockSession {
                prov_id: spec.provider_session_id,
                rx: Mutex::new(Some(rx)),
                turn_seq: AtomicU64::new(0),
            }) as Box<dyn DriverSession>)
        })
    }
}

struct RunningServer {
    socket_path: PathBuf,
    engine: Arc<CoreEngine>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<Result<(), ApiError>>,
}

impl RunningServer {
    async fn start(
        home: &Path,
        drv: Arc<MockDriver>,
        broadcast_cap: usize,
        max_retained: u64,
        heartbeat: Duration,
    ) -> Self {
        let lock = DaemonLock::acquire(home).expect("acquire lock");
        let socket_path = home.join("run").join("dolgorae.sock");
        let mut cfg = CoreConfig::new(home.to_path_buf());
        cfg.event_broadcast_capacity = broadcast_cap;
        cfg.max_retained_events_per_session = max_retained;
        cfg.socket_path = Some(socket_path.clone());

        let engine = Arc::new(CoreEngine::new(cfg, vec![drv]).await.expect("init engine"));
        let server = BoundUdsServer::bind(
            lock,
            Arc::clone(&engine),
            ServerConfig {
                socket_path: socket_path.clone(),
                heartbeat_interval: heartbeat,
            },
        )
        .await
        .expect("bind uds");

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            server
                .serve_until(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        Self {
            socket_path,
            engine,
            shutdown_tx: Some(shutdown_tx),
            handle,
        }
    }

    async fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.handle.await.expect("join server").expect("clean stop");
    }
}

fn create_req(ws: &Path) -> CreateSessionRequest {
    CreateSessionRequest {
        workspace: ws.to_path_buf(),
        driver: Some("mock-drv".to_owned()),
        role: None,
        parent_session_id: None,
        model: None,
        effort: None,
        access: Some(AccessMode::WorkspaceWrite),
        approval_policy: Some(ApprovalPolicy::Interactive),
        output_schema: None,
        allow_delegation: false,
        policy: None,
    }
}

#[tokio::test]
async fn daemon_lock_rejects_second_serve_and_unsafe_paths() {
    let home = TempDir::new("lock-home");
    let ws = TempDir::new("lock-ws");
    let drv = MockDriver::new();

    // Loose permissions directory rejected by DaemonLock::acquire.
    let loose_home = TempDir::new("loose-home");
    fs::set_permissions(loose_home.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        DaemonLock::acquire(loose_home.path()),
        Err(ApiError::UnsafePath(_))
    ));

    // Symlink socket path rejected by BoundUdsServer::bind.
    let sym_home = TempDir::new("sym-home");
    let sym_lock = DaemonLock::acquire(sym_home.path()).unwrap();
    let sym_sock = sym_home.path().join("run").join("sym.sock");
    std::os::unix::fs::symlink(ws.path(), &sym_sock).unwrap();
    let sym_eng = Arc::new(
        CoreEngine::new(
            CoreConfig::new(sym_home.path().to_path_buf()),
            vec![drv.clone()],
        )
        .await
        .unwrap(),
    );
    assert!(matches!(
        BoundUdsServer::bind(sym_lock, sym_eng, ServerConfig::new(sym_sock)).await,
        Err(ApiError::UnsafePath(_))
    ));

    // B1(a): Existing regular file at socket_path MUST return UnsafePath WITHOUT deleting it!
    let reg_home = TempDir::new("reg-home");
    let reg_lock = DaemonLock::acquire(reg_home.path()).unwrap();
    let reg_file = reg_home.path().join("run").join("precious.db");
    fs::write(&reg_file, b"keep-me-intact").unwrap();
    let reg_eng = Arc::new(
        CoreEngine::new(
            CoreConfig::new(reg_home.path().to_path_buf()),
            vec![drv.clone()],
        )
        .await
        .unwrap(),
    );
    assert!(matches!(
        BoundUdsServer::bind(reg_lock, reg_eng, ServerConfig::new(reg_file.clone())).await,
        Err(ApiError::UnsafePath(_))
    ));
    assert_eq!(
        fs::read(&reg_file).expect("regular file preserved"),
        b"keep-me-intact"
    );

    // Start first daemon and create an active session.
    let srv =
        RunningServer::start(home.path(), drv.clone(), 64, 1000, Duration::from_secs(10)).await;
    let client = UdsClient::new(srv.socket_path.clone());

    // B1(b): Second daemon with a DIFFERENT home pointing at an ALREADY-LIVE socket
    // MUST return AlreadyRunning WITHOUT unlinking or disrupting the first daemon!
    let other_home = TempDir::new("other-home");
    let other_lock = DaemonLock::acquire(other_home.path()).unwrap();
    let other_eng = Arc::new(
        CoreEngine::new(CoreConfig::new(other_home.path().to_path_buf()), vec![drv])
            .await
            .unwrap(),
    );
    assert!(matches!(
        BoundUdsServer::bind(
            other_lock,
            other_eng,
            ServerConfig::new(srv.socket_path.clone())
        )
        .await,
        Err(ApiError::AlreadyRunning(_))
    ));

    // Socket permissions must be 0600 inside 0700 run dir.
    let run_mode = fs::metadata(home.path().join("run"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(run_mode, 0o700);
    let sock_mode = fs::symlink_metadata(&srv.socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(sock_mode, 0o600);

    let snap: dolgorae_core::SessionSnapshot = client
        .post_json("/v2/sessions", &create_req(ws.path()))
        .await
        .expect("create session");
    assert_eq!(snap.status, SessionStatus::Active);

    // Second daemon on same home MUST fail with AlreadyRunning without touching DB!
    let second = DaemonLock::acquire(home.path());
    assert!(matches!(second, Err(ApiError::AlreadyRunning(_))));

    // First daemon's session MUST remain Active (no startup recovery ran!).
    let still_active: dolgorae_core::SessionSnapshot = client
        .get_json(&format!("/v2/sessions/{}", snap.session_id))
        .await
        .expect("get session");
    assert_eq!(still_active.status, SessionStatus::Active);
    assert_eq!(still_active.session_generation, 1);

    // Peer UID mismatch check returns UNAUTHORIZED.
    assert_eq!(
        verify_peer_uid(9999, 1000).unwrap_err().code,
        ErrorCode::Unauthorized
    );

    let sock_path = srv.socket_path.clone();
    srv.stop().await;
    assert!(
        !sock_path.exists(),
        "socket must be unlinked on clean shutdown"
    );
}

#[tokio::test]
async fn all_nineteen_routes_auth_size_caps_and_sse_framing() {
    let home = TempDir::new("routes-home");
    let ws = TempDir::new("routes-ws");
    fs::create_dir_all(ws.path().join(".dolgorae/roles")).unwrap();
    fs::write(
        ws.path().join(".dolgorae/roles/reviewer.md"),
        "---\ndriver: mock-drv\n---\nReview carefully.",
    )
    .unwrap();

    let drv = MockDriver::new();
    let srv =
        RunningServer::start(home.path(), drv.clone(), 64, 5, Duration::from_millis(80)).await;
    let client = UdsClient::new(srv.socket_path.clone());

    // 1. GET /v2/health & N2: unknown route 404 JSON + bad query param 400 JSON
    let health: Value = client.get_json("/v2/health").await.expect("health");
    assert_eq!(health["status"], "ok");
    assert_eq!(health["api_version"], "v2");
    assert_eq!(
        client
            .get_json::<Value>("/v2/does-not-exist")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        client
            .get_json::<Value>("/v2/sessions/sess-x/timeline?limit=notanint")
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );

    // 2. GET /v2/capabilities
    let caps: CapabilitiesSnapshot = client.get_json("/v2/capabilities").await.expect("caps");
    assert_eq!(caps.drivers.len(), 1);

    // 3-4. GET /v2/roles & /v2/roles/{name}
    let roles_val: Value = client
        .get_json(&format!(
            "/v2/roles?workspace={}",
            dolgorae_api::url_encode(ws.path().to_str().unwrap())
        ))
        .await
        .expect("roles");
    assert_eq!(roles_val["roles"].as_array().unwrap().len(), 1);
    let role_detail: RoleDetail = client
        .get_json(&format!(
            "/v2/roles/reviewer?workspace={}",
            dolgorae_api::url_encode(ws.path().to_str().unwrap())
        ))
        .await
        .expect("role detail");
    assert_eq!(role_detail.summary.name, "reviewer");

    // Invalid bearer token -> 401 UNAUTHORIZED
    let bad_client = UdsClient::new(srv.socket_path.clone())
        .with_bearer_token(Zeroizing::new("forged-token".to_owned()));
    assert_eq!(
        bad_client
            .get_json::<Value>("/v2/sessions")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );

    // > 8 MiB JSON body -> 413 PAYLOAD_TOO_LARGE
    let huge_req = PostMailboxRequest {
        from_session_id: None,
        subject: None,
        body: "x".repeat(MAX_RECEIVE_BYTES + 1024),
        artifact_ids: Vec::new(),
    };
    let err_8m = client
        .post_json::<_, Value>("/v2/sessions/sess-any/mailbox", &huge_req)
        .await
        .unwrap_err();
    assert_eq!(err_8m.code, ErrorCode::PayloadTooLarge);

    // 5-6. POST & GET /v2/sessions, GET /v2/sessions/{id}
    let s1: dolgorae_core::SessionSnapshot = client
        .post_json("/v2/sessions", &create_req(ws.path()))
        .await
        .expect("s1");
    let ctrl1 = drv.last_ctrl();
    let s2: dolgorae_core::SessionSnapshot = client
        .post_json("/v2/sessions", &create_req(ws.path()))
        .await
        .expect("s2");

    let list_val: Value = client
        .get_json("/v2/sessions")
        .await
        .expect("list sessions");
    assert_eq!(list_val["sessions"].as_array().unwrap().len(), 2);
    let got_s1: dolgorae_core::SessionSnapshot = client
        .get_json(&format!("/v2/sessions/{}", s1.session_id))
        .await
        .expect("get s1");
    assert_eq!(got_s1.session_id, s1.session_id);

    // 16-18. Writer lease: POST, transfer, DELETE
    let lease: WriterLease = client
        .post_json(
            "/v2/workspaces/lease",
            &json!({ "workspace": ws.path(), "session_id": s1.session_id }),
        )
        .await
        .expect("acquire lease");
    assert_eq!(lease.holder_session_id, s1.session_id);
    let transferred: WriterLease = client
        .post_json(
            "/v2/workspaces/lease/transfer",
            &json!({
                "workspace": ws.path(),
                "from_session_id": s1.session_id,
                "to_session_id": s2.session_id,
            }),
        )
        .await
        .expect("transfer lease");
    assert_eq!(transferred.holder_session_id, s2.session_id);
    let released: Value = client
        .delete_json(&format!(
            "/v2/workspaces/lease?workspace={}&session_id={}",
            dolgorae_api::url_encode(ws.path().to_str().unwrap()),
            s2.session_id
        ))
        .await
        .expect("release lease");
    assert_eq!(released["released"], json!(true));

    // 9. POST /v2/sessions/{id}/turns & 10. interrupt
    let turn: dolgorae_core::TurnAccepted = client
        .post_json(
            &format!("/v2/sessions/{}/turns", s1.session_id),
            &SubmitTurnRequest {
                text: "hello".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            },
        )
        .await
        .expect("submit turn");

    // 11. SSE: malformed Last-Event-ID -> 400 BAD_REQUEST
    assert_eq!(
        client
            .open_sse(
                &format!("/v2/sessions/{}/events", s1.session_id),
                Some("01")
            )
            .await
            .err()
            .expect("bad cursor")
            .code,
        ErrorCode::BadRequest
    );

    // Valid SSE: exclusive replay after cursor 1 -> receives cursor 2 (`turn_started`),
    // then ephemeral `text_delta` (NO `id:`), then `heartbeat` (NO `id:`)!
    let mut sse = client
        .open_sse(&format!("/v2/sessions/{}/events", s1.session_id), Some("1"))
        .await
        .expect("open sse");
    let frame_replay = tokio::time::timeout(Duration::from_secs(2), sse.next_frame())
        .await
        .expect("replay frame")
        .unwrap()
        .unwrap();
    assert_eq!(frame_replay.id.as_deref(), Some("2"));
    assert_eq!(frame_replay.event.as_deref(), Some("turn_started"));

    ctrl1
        .emit(NormalizedEvent::TextDelta {
            turn_id: turn.turn_id.clone(),
            item_id: Some("m1".to_owned()),
            delta: "안녕하세요 돌고래 🐬🚀".to_owned(),
        })
        .await;
    let mut saw_delta = false;
    let mut saw_hb = false;
    for _ in 0..8 {
        if saw_delta && saw_hb {
            break;
        }
        let f = tokio::time::timeout(Duration::from_secs(2), sse.next_frame())
            .await
            .expect("frame timeout")
            .unwrap()
            .unwrap();
        if f.event.as_deref() == Some("text_delta") {
            assert_eq!(f.id, None, "ephemeral text_delta MUST NOT carry id:");
            assert!(
                f.data.contains("안녕하세요 돌고래 🐬🚀") && !f.data.contains('\u{FFFD}'),
                "multi-byte UTF-8 SSE payload must decode losslessly: {}",
                f.data
            );
            saw_delta = true;
        } else if f.event.as_deref() == Some("heartbeat") {
            assert_eq!(f.id, None, "heartbeat MUST NOT carry id:");
            saw_hb = true;
        }
    }
    assert!(saw_delta && saw_hb);
    drop(sse);

    // 12-13. Interactions list + answer (409 on stale revision, 200 on fresh revision)
    ctrl1
        .emit(NormalizedEvent::InteractionRequested {
            request: InteractionRequest {
                interaction_id: "int-api-1".to_owned(),
                turn_id: turn.turn_id.clone(),
                kind: InteractionKind::CommandApproval {
                    command: vec!["echo".to_owned()],
                    cwd: None,
                    reason: None,
                },
                original_kind: "command_approval".to_owned(),
                title: "Echo".to_owned(),
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
    tokio::time::sleep(Duration::from_millis(40)).await;
    let ilist: InteractionList = client
        .get_json(&format!(
            "/v2/sessions/{}/interactions?pending_only=true",
            s1.session_id
        ))
        .await
        .expect("list interactions");
    assert_eq!(ilist.interactions.len(), 1);

    let stale_ans = AnswerInteractionRequest {
        turn_id: turn.turn_id.clone(),
        session_generation: ilist.interactions[0].session_generation,
        expected_interaction_revision: 0,
        expected_session_revision: ilist.session_revision,
        decision_id: "allow".to_owned(),
        answers: BTreeMap::new(),
        payload: None,
    };
    assert_eq!(
        client
            .post_json::<_, Value>(
                &format!(
                    "/v2/sessions/{}/interactions/int-api-1/answer",
                    s1.session_id
                ),
                &stale_ans
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SnapshotConflict
    );

    let ok_ans = AnswerInteractionRequest {
        expected_interaction_revision: ilist.interaction_revision,
        ..stale_ans
    };
    let answered: Value = client
        .post_json(
            &format!(
                "/v2/sessions/{}/interactions/int-api-1/answer",
                s1.session_id
            ),
            &ok_ans,
        )
        .await
        .expect("answer ok");
    assert_eq!(answered["status"], "resolved");

    let intr: Value = client
        .post_empty(&format!(
            "/v2/sessions/{}/turns/{}/interrupt",
            s1.session_id, turn.turn_id
        ))
        .await
        .expect("interrupt");
    assert_eq!(intr["status"], "interrupt_requested");

    ctrl1
        .emit(NormalizedEvent::TurnCompleted {
            turn_id: turn.turn_id.clone(),
            status: TurnTerminalStatus::Completed,
            error: None,
        })
        .await;

    // 15. POST /v2/sessions/wait
    let waited: WaitSessionsResponse = client
        .post_json(
            "/v2/sessions/wait",
            &WaitSessionsRequest {
                waiter_session_id: None,
                session_ids: vec![s1.session_id.clone()],
                mode: WaitMode::All,
                timeout_ms: 2_000,
            },
        )
        .await
        .expect("wait");
    assert!(!waited.timed_out);

    // 14. Mailbox POST + GET (also triggers retention prune with max_retained=5!)
    for i in 0..4 {
        let _: MailboxMessage = client
            .post_json(
                &format!("/v2/sessions/{}/mailbox", s1.session_id),
                &PostMailboxRequest {
                    from_session_id: None,
                    subject: Some(format!("s{i}")),
                    body: format!("b{i}"),
                    artifact_ids: Vec::new(),
                },
            )
            .await
            .expect("mailbox post");
    }
    let mpage: MailboxPage = client
        .get_json(&format!(
            "/v2/sessions/{}/mailbox?after=0&limit=10",
            s1.session_id
        ))
        .await
        .expect("mailbox list");
    assert_eq!(mpage.messages.len(), 4);

    // Pruned cursor 1 on SSE -> emits `refresh_required` with NO `id:` and closes cleanly!
    let mut pruned_sse = client
        .open_sse(&format!("/v2/sessions/{}/events", s1.session_id), Some("1"))
        .await
        .expect("open pruned sse");
    let rr = tokio::time::timeout(Duration::from_secs(2), pruned_sse.next_frame())
        .await
        .expect("refresh_required")
        .unwrap()
        .unwrap();
    assert_eq!(rr.event.as_deref(), Some("refresh_required"));
    assert_eq!(rr.id, None);
    assert!(rr.data.contains("session"));

    // 10b. Timeline GET
    let tl: TimelinePage = client
        .get_json(&format!("/v2/sessions/{}/timeline?limit=2", s1.session_id))
        .await
        .expect("timeline");
    assert_eq!(tl.items.len(), 2);

    // 19. Artifacts POST + GET + streaming > 64 MiB -> 413 PAYLOAD_TOO_LARGE
    let art = client
        .post_artifact_bytes("text/plain", b"hello-artifact-bytes".to_vec())
        .await
        .expect("upload artifact");
    let slice = client
        .get_artifact_slice(&art.artifact_id, 6, 8)
        .await
        .expect("download slice");
    assert_eq!(slice, b"artifact");

    let (chunk_tx, chunk_rx) = mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(4);
    let feeder = tokio::spawn(async move {
        let chunk = Bytes::from(vec![b'x'; 256 * 1024]);
        let count = (MAX_ARTIFACT_BYTES / (256 * 1024)) + 2;
        for _ in 0..count {
            if chunk_tx.send(Ok(Frame::data(chunk.clone()))).await.is_err() {
                break;
            }
        }
    });
    let err_64m = client
        .post_artifact_stream("application/octet-stream", chunk_rx)
        .await
        .unwrap_err();
    feeder.abort();
    assert_eq!(err_64m.code, ErrorCode::PayloadTooLarge);

    // 8. POST /v2/sessions/{id}/resume & 7. DELETE /v2/sessions/{id}
    let resumed: dolgorae_core::SessionSnapshot = client
        .post_empty(&format!("/v2/sessions/{}/resume", s1.session_id))
        .await
        .expect("resume");
    assert_eq!(resumed.session_generation, 2);

    let closed: dolgorae_core::SessionSnapshot = client
        .delete_json(&format!("/v2/sessions/{}", s1.session_id))
        .await
        .expect("close");
    assert_eq!(closed.status, SessionStatus::Closed);

    // Revoked token after close -> 401 UNAUTHORIZED
    let tok_s2 = srv
        .engine
        .mint_actor_token(&CallerContext::ExternalHost, &s2.session_id)
        .await
        .unwrap();
    let _: dolgorae_core::SessionSnapshot = client
        .delete_json(&format!("/v2/sessions/{}", s2.session_id))
        .await
        .unwrap();
    let revoked_client = UdsClient::new(srv.socket_path.clone()).with_bearer_token(tok_s2);
    assert_eq!(
        revoked_client
            .get_json::<Value>("/v2/sessions")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );

    srv.stop().await;
}

#[tokio::test]
async fn sse_broadcast_lag_recovers_without_silent_gap() {
    let home = TempDir::new("lag-home");
    let ws = TempDir::new("lag-ws");
    let drv = MockDriver::new();

    // 32 KiB subject per durable mailbox event => 40 events = ~1.28 MiB of SSE
    // frames, saturating the OS UDS socket buffer + SSE mpsc(8) channel while
    // the client is stalled and guaranteeing tokio::sync::broadcast(4) returns
    // RecvError::Lagged(n)!
    let big_subject = "S".repeat(32 * 1024);

    // Case A: tiny broadcast capacity (4) + generous retention (100) ->
    // 40 large bursts trigger RecvError::Lagged(n), and SSE MUST re-replay
    // every missed durable cursor contiguously with ZERO silent gaps!
    let srv = RunningServer::start(home.path(), drv.clone(), 4, 100, Duration::from_secs(30)).await;
    let client = UdsClient::new(srv.socket_path.clone());
    let snap: dolgorae_core::SessionSnapshot = client
        .post_json("/v2/sessions", &create_req(ws.path()))
        .await
        .expect("create session"); // cursor 1

    let mut sse = client
        .open_sse(
            &format!("/v2/sessions/{}/events", snap.session_id),
            Some("1"),
        )
        .await
        .expect("open sse");

    for i in 0..40 {
        let _: MailboxMessage = client
            .post_json(
                &format!("/v2/sessions/{}/mailbox", snap.session_id),
                &PostMailboxRequest {
                    from_session_id: None,
                    subject: Some(format!("burst-{i}-{big_subject}")),
                    body: "b".to_owned(),
                    artifact_ids: Vec::new(),
                },
            )
            .await
            .expect("burst mailbox");
    }

    let mut seen_cursors = Vec::new();
    while seen_cursors.len() < 40 {
        let frame = tokio::time::timeout(Duration::from_secs(5), sse.next_frame())
            .await
            .expect("frame timeout")
            .expect("frame ok")
            .expect("not eof");
        if let Some(id) = frame.id {
            seen_cursors.push(Cursor::parse(&id).unwrap().value());
        }
    }
    let expected: Vec<u64> = (2..=41).collect();
    assert_eq!(
        seen_cursors, expected,
        "lagged SSE MUST recover every durable cursor with zero silent gaps"
    );
    drop(sse);
    srv.stop().await;

    // Case B: tiny broadcast capacity (4) + tiny retention (5) ->
    // 40 large bursts saturate the UDS buffer and trigger RecvError::Lagged(n)
    // beyond the 5-event retention window -> SSE MUST emit `refresh_required`
    // (NO id:) and terminate cleanly!
    let home2 = TempDir::new("lag-prune-home");
    let srv2 = RunningServer::start(home2.path(), drv, 4, 5, Duration::from_secs(30)).await;
    let client2 = UdsClient::new(srv2.socket_path.clone());
    let snap2: dolgorae_core::SessionSnapshot = client2
        .post_json("/v2/sessions", &create_req(ws.path()))
        .await
        .expect("s2");

    let mut sse2 = client2
        .open_sse(
            &format!("/v2/sessions/{}/events", snap2.session_id),
            Some("1"),
        )
        .await
        .expect("open sse2");

    for i in 0..40 {
        let _: MailboxMessage = client2
            .post_json(
                &format!("/v2/sessions/{}/mailbox", snap2.session_id),
                &PostMailboxRequest {
                    from_session_id: None,
                    subject: Some(format!("p-{i}-{big_subject}")),
                    body: "b".to_owned(),
                    artifact_ids: Vec::new(),
                },
            )
            .await
            .expect("prune burst");
    }

    let mut saw_refresh = false;
    let mut prev_cursor = 1u64;
    for _ in 0..60 {
        let maybe_frame = tokio::time::timeout(Duration::from_secs(5), sse2.next_frame())
            .await
            .expect("timeout")
            .expect("ok");
        let Some(frame) = maybe_frame else {
            break;
        };
        if frame.event.as_deref() == Some("refresh_required") {
            assert_eq!(frame.id, None, "refresh_required must NOT carry id:");
            assert!(frame.data.contains("session"));
            saw_refresh = true;
            break;
        }
        if let Some(id) = frame.id {
            let c = Cursor::parse(&id).unwrap().value();
            assert_eq!(
                c,
                prev_cursor + 1,
                "must NEVER silently skip a cursor before refresh_required"
            );
            prev_cursor = c;
            if prev_cursor == 41 {
                break;
            }
        }
    }
    assert!(
        saw_refresh,
        "stalled SSE with broadcast(4) + retention(5) over 1.28 MiB burst MUST trigger RecvError::Lagged -> refresh_required (prev_cursor={prev_cursor})"
    );
    srv2.stop().await;
}
