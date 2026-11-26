//! Dual-mode stdio MCP integration tests for `dolgorae-mcp` over a real local
//! UDS `dolgorae-api` server (isolated TempDir homes/sockets, zero live CLIs).

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dolgorae_api::{BoundUdsServer, DaemonLock, ServerConfig, UdsClient};
use dolgorae_core::{
    AccessMode, AgentDriver, ApprovalPolicy, CallerContext, CoreConfig, CoreEngine,
    CreateSessionRequest, DecisionKind, DecisionOption, DriverCapabilities, DriverError,
    DriverSession, InteractionAnswer, InteractionKind, InteractionRequest, NormalizedEvent,
    ResumeSpec, SessionPolicy, SessionSpec, TurnAck, TurnInput, TurnTerminalStatus,
};
use dolgorae_mcp::{McpError, McpMode, McpServer, McpServerConfig};
use dolgorae_spi::{EventReceiver, EventSender, event_channel};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from("/tmp").join(format!("dg-m-{label}-{}-{n}", std::process::id()));
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
    answers: Mutex<Vec<InteractionAnswer>>,
}

impl MockCtrl {
    async fn emit(&self, ev: NormalizedEvent) {
        self.tx.send(ev).await.expect("emit");
    }
}

struct MockSession {
    prov_id: String,
    rx: Mutex<Option<EventReceiver>>,
    ctrl: Arc<MockCtrl>,
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
        answer: InteractionAnswer,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.ctrl.answers.lock().unwrap().push(answer);
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
            let ctrl = Arc::new(MockCtrl {
                tx,
                answers: Mutex::new(Vec::new()),
            });
            self.ctrls.lock().unwrap().insert(n, Arc::clone(&ctrl));
            Ok(Box::new(MockSession {
                prov_id: format!("prov-{n}"),
                rx: Mutex::new(Some(rx)),
                ctrl,
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
            let ctrl = Arc::new(MockCtrl {
                tx,
                answers: Mutex::new(Vec::new()),
            });
            self.ctrls.lock().unwrap().insert(n, Arc::clone(&ctrl));
            Ok(Box::new(MockSession {
                prov_id: spec.provider_session_id,
                rx: Mutex::new(Some(rx)),
                ctrl,
                turn_seq: AtomicU64::new(0),
            }) as Box<dyn DriverSession>)
        })
    }
}

struct McpTestHarness {
    client_tx: tokio::io::DuplexStream,
    client_rx: BufReader<tokio::io::DuplexStream>,
    _server_task: tokio::task::JoinHandle<Result<(), McpError>>,
    next_id: u64,
}

impl McpTestHarness {
    fn spawn(server: McpServer) -> Self {
        let (client_tx, srv_rx) = tokio::io::duplex(64 * 1024);
        let (srv_tx, client_rx) = tokio::io::duplex(64 * 1024);
        let task =
            tokio::spawn(async move { server.run_stdio(BufReader::new(srv_rx), srv_tx).await });
        Self {
            client_tx,
            client_rx: BufReader::new(client_rx),
            _server_task: task,
            next_id: 1,
        }
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        self.client_tx.write_all(line.as_bytes()).await.unwrap();
        self.client_tx.write_all(b"\n").await.unwrap();
        self.client_tx.flush().await.unwrap();

        let mut resp_line = String::new();
        self.client_rx.read_line(&mut resp_line).await.unwrap();
        let val: Value = serde_json::from_str(&resp_line).expect("valid jsonrpc");
        assert_eq!(val["id"], json!(id));
        val
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let resp = self
            .rpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        let is_err = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("json tool payload");
        (is_err, parsed)
    }
}

async fn start_daemon(
    home: &Path,
    drv: Arc<MockDriver>,
) -> (
    PathBuf,
    Arc<CoreEngine>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let lock = DaemonLock::acquire(home).unwrap();
    let sock = home.join("run/dolgorae.sock");
    let mut cfg = CoreConfig::new(home.to_path_buf());
    cfg.socket_path = Some(sock.clone());
    let engine = Arc::new(CoreEngine::new(cfg, vec![drv]).await.unwrap());
    let server = BoundUdsServer::bind(lock, Arc::clone(&engine), ServerConfig::new(sock.clone()))
        .await
        .unwrap();
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = server
            .serve_until(async move {
                let _ = rx.await;
            })
            .await;
    });
    (sock, engine, tx, handle)
}

#[tokio::test]
async fn external_host_mode_epic_squad_workflow() {
    let home = TempDir::new("ext-home");
    let ws = TempDir::new("ext-ws");
    let drv = MockDriver::new();
    let (sock, _engine, stop_tx, srv_handle) = start_daemon(home.path(), drv.clone()).await;

    let mcp = McpServer::new(McpServerConfig {
        mode: McpMode::ExternalHost,
        socket_path: sock,
        actor_token: None,
        self_session_id: None,
    })
    .unwrap();
    let mut h = McpTestHarness::spawn(mcp);

    // Initialize + tools/list (exact 15 external tools with inputSchema).
    let init = h
        .rpc("initialize", json!({ "protocolVersion": "2025-06-18" }))
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "dolgorae");

    let list = h.rpc("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    assert!(tools.iter().all(|t| t.get("inputSchema").is_some()));

    // Start two WorkspaceWrite workers (epic-squad pattern).
    let (err, s1) = h
        .call_tool(
            "dolgorae_session_start",
            json!({
                "workspace": ws.path(),
                "driver": "mock-drv",
                "access": "workspace_write"
            }),
        )
        .await;
    assert!(!err, "start s1 failed: {s1}");
    let ctrl1 = drv.last_ctrl();
    let s1_id = s1["session_id"].as_str().unwrap().to_owned();

    let (err, s2) = h
        .call_tool(
            "dolgorae_session_start",
            json!({
                "workspace": ws.path(),
                "driver": "mock-drv",
                "access": "workspace_write"
            }),
        )
        .await;
    assert!(!err, "start s2 failed: {s2}");
    let s2_id = s2["session_id"].as_str().unwrap().to_owned();

    // Acquire + transfer + release writer lease via MCP.
    let (err, lease) = h
        .call_tool(
            "dolgorae_writer_lease",
            json!({ "workspace": ws.path(), "session_id": s1_id }),
        )
        .await;
    assert!(!err && lease["holder_session_id"] == s1_id);

    let (err, xfer) = h
        .call_tool(
            "dolgorae_writer_transfer",
            json!({
                "workspace": ws.path(),
                "from_session_id": s1_id,
                "to_session_id": s2_id
            }),
        )
        .await;
    assert!(!err && xfer["holder_session_id"] == s2_id);

    let (err, rel) = h
        .call_tool(
            "dolgorae_writer_lease",
            json!({ "workspace": ws.path(), "session_id": s2_id, "release": true }),
        )
        .await;
    assert!(!err && rel["released"] == json!(true));

    // Submit turn on s1, complete it, and wait via `dolgorae_session_wait(any)`.
    let (err, turn) = h
        .call_tool(
            "dolgorae_turn_submit",
            json!({ "session_id": s1_id, "text": "implement feature" }),
        )
        .await;
    assert!(!err);
    ctrl1
        .emit(NormalizedEvent::TurnCompleted {
            turn_id: turn["turn_id"].as_str().unwrap().to_owned(),
            status: TurnTerminalStatus::Completed,
            error: None,
        })
        .await;

    let (err, wait_res) = h
        .call_tool(
            "dolgorae_session_wait",
            json!({
                "session_ids": [s1_id, s2_id],
                "mode": "any",
                "timeout_ms": 5000
            }),
        )
        .await;
    assert!(!err && wait_res["timed_out"] == json!(false));
    assert_eq!(wait_res["completed_session_ids"], json!([s1_id]));

    // Close both sessions cleanly.
    let (err, _) = h
        .call_tool("dolgorae_session_close", json!({ "session_id": s1_id }))
        .await;
    assert!(!err);
    let (err, _) = h
        .call_tool("dolgorae_session_close", json!({ "session_id": s2_id }))
        .await;
    assert!(!err);

    let _ = stop_tx.send(());
    let _ = srv_handle.await;
}

fn decode_hex(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    assert_eq!(bytes.len() % 2, 0);
    (0..bytes.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(std::str::from_utf8(&bytes[i..i + 2]).unwrap(), 16).unwrap())
        .collect()
}

async fn read_entire_artifact_via_mcp(h: &mut McpTestHarness, artifact_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(pages < 32, "pagination did not reach eof");
        let resp = h
            .rpc(
                "tools/call",
                json!({
                    "name": "dolgorae_artifact_read",
                    "arguments": {
                        "artifact_id": artifact_id,
                        "offset": offset,
                    }
                }),
            )
            .await;
        assert_eq!(resp["result"]["isError"], json!(false));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.len() <= dolgorae_core::MAX_INLINE_BYTES,
            "MCP artifact_read response {} exceeded 256 KiB",
            text.len()
        );
        let page: Value = serde_json::from_str(text).unwrap();
        let encoding = page["encoding"].as_str().unwrap();
        let content = page["content"].as_str().unwrap();
        let chunk = match encoding {
            "utf-8" => content.as_bytes().to_vec(),
            "hex" => decode_hex(content),
            other => panic!("unexpected encoding {other}"),
        };
        assert_eq!(chunk.len() as u64, page["bytes_read"].as_u64().unwrap());
        out.extend_from_slice(&chunk);
        offset = page["next_offset"].as_u64().unwrap();
        if page["eof"].as_bool().unwrap() {
            break;
        }
    }
    out
}

#[tokio::test]
async fn oversized_tool_response_spills_and_artifact_read_paginates_losslessly() {
    let home = TempDir::new("spill-home");
    let ws = TempDir::new("spill-ws");
    let drv = MockDriver::new();
    let (sock, _engine, stop_tx, srv_handle) = start_daemon(home.path(), drv.clone()).await;

    let mut h = McpTestHarness::spawn(
        McpServer::new(McpServerConfig {
            mode: McpMode::ExternalHost,
            socket_path: sock.clone(),
            actor_token: None,
            self_session_id: None,
        })
        .unwrap(),
    );

    // Start session and emit 4 AssistantMessage events of 100 KiB each (~400 KiB timeline page).
    let (err, sess) = h
        .call_tool(
            "dolgorae_session_start",
            json!({ "workspace": ws.path(), "driver": "mock-drv" }),
        )
        .await;
    assert!(!err);
    let sid = sess["session_id"].as_str().unwrap().to_owned();
    let ctrl = drv.last_ctrl();

    let (err, turn) = h
        .call_tool(
            "dolgorae_turn_submit",
            json!({ "session_id": sid, "text": "generate big timeline" }),
        )
        .await;
    assert!(!err);
    let tid = turn["turn_id"].as_str().unwrap().to_owned();

    let msg_chunk = "M".repeat(100 * 1024);
    for idx in 0..4 {
        ctrl.emit(NormalizedEvent::AssistantMessage {
            turn_id: tid.clone(),
            item_id: Some(format!("big-{idx}")),
            text: format!("{idx}:{msg_chunk}"),
            structured_output: None,
        })
        .await;
    }
    ctrl.emit(NormalizedEvent::TurnCompleted {
        turn_id: tid,
        status: TurnTerminalStatus::Completed,
        error: None,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;

    // B2(a): `dolgorae_timeline_read` exceeds 256 KiB -> MUST spill to a real ArtifactRef!
    let raw_resp = h
        .rpc(
            "tools/call",
            json!({
                "name": "dolgorae_timeline_read",
                "arguments": { "session_id": sid, "limit": 50 }
            }),
        )
        .await;
    assert_eq!(raw_resp["result"]["isError"], json!(false));
    let raw_text = raw_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(raw_text.len() <= dolgorae_core::MAX_INLINE_BYTES);
    let spill_val: Value = serde_json::from_str(raw_text).unwrap();
    assert_eq!(spill_val["spilled"], json!(true));
    let spill_art_id = spill_val["artifact"]["artifact_id"]
        .as_str()
        .expect("spilled artifact_id")
        .to_owned();
    assert!(
        spill_val["artifact"]["byte_len"].as_u64().unwrap()
            > dolgorae_core::MAX_INLINE_BYTES as u64
    );

    // Reconstruct the spilled JSON byte-for-byte via paginated `dolgorae_artifact_read`!
    let reconstructed_bytes = read_entire_artifact_via_mcp(&mut h, &spill_art_id).await;
    let reconstructed_json: Value =
        serde_json::from_slice(&reconstructed_bytes).expect("reconstructed spilled timeline JSON");
    assert_eq!(reconstructed_json["session_id"], sid);
    let big_items = reconstructed_json["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "assistant_message")
        .count();
    assert_eq!(big_items, 4);

    // B2(b): Upload 300 KiB multi-byte UTF-8 artifact AND 300 KiB binary artifact,
    // paginate BOTH using default `dolgorae_artifact_read` to EOF with zero truncation!
    let client = UdsClient::new(sock);
    let utf8_unit = "가나다라마바사아자차카타파하-Dolgorae-🐬\n";
    let mut utf8_payload = String::new();
    while utf8_payload.len() < 300 * 1024 {
        utf8_payload.push_str(utf8_unit);
    }
    let utf8_art = client
        .post_artifact_bytes(
            "text/plain; charset=utf-8",
            utf8_payload.as_bytes().to_vec(),
        )
        .await
        .unwrap();
    let got_utf8 = read_entire_artifact_via_mcp(&mut h, &utf8_art.artifact_id).await;
    assert_eq!(got_utf8, utf8_payload.as_bytes());

    let bin_payload: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 256) as u8).collect();
    let bin_art = client
        .post_artifact_bytes("application/octet-stream", bin_payload.clone())
        .await
        .unwrap();
    let got_bin = read_entire_artifact_via_mcp(&mut h, &bin_art.artifact_id).await;
    assert_eq!(got_bin, bin_payload);

    let _ = stop_tx.send(());
    let _ = srv_handle.await;
}

#[tokio::test]
async fn scoped_actor_mode_fail_closed_delegation_sse_approvals_and_boundaries() {
    let home = TempDir::new("scoped-home");
    let ws = TempDir::new("scoped-ws");
    fs::create_dir_all(ws.path().join(".dolgorae/roles")).unwrap();
    fs::write(
        ws.path().join(".dolgorae/roles/lead.md"),
        "---\ndriver: mock-drv\n---\nLead role.",
    )
    .unwrap();

    let drv = MockDriver::new();
    let (sock, engine, stop_tx, srv_handle) = start_daemon(home.path(), drv.clone()).await;

    // Fail closed when token is missing or empty.
    assert!(matches!(
        McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: sock.clone(),
            actor_token: None,
            self_session_id: None,
        }),
        Err(McpError::MissingActorToken)
    ));

    // Create root delegating session (max_delegation_depth = 1, role = lead, collaboration = false).
    let root = engine
        .create_session(
            &CallerContext::ExternalHost,
            CreateSessionRequest {
                workspace: ws.path().to_path_buf(),
                driver: Some("mock-drv".to_owned()),
                role: Some("lead".to_owned()),
                parent_session_id: None,
                model: None,
                effort: None,
                access: Some(AccessMode::ReadOnly),
                approval_policy: Some(ApprovalPolicy::Interactive),
                output_schema: None,
                allow_delegation: true,
                policy: Some(SessionPolicy {
                    max_delegation_depth: 1,
                    allow_actor_collaboration: false,
                }),
            },
        )
        .await
        .unwrap();
    let root_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &root.session_id)
        .await
        .unwrap();

    let mut scoped = McpTestHarness::spawn(
        McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: sock.clone(),
            actor_token: Some(root_tok),
            self_session_id: Some(root.session_id.clone()),
        })
        .unwrap(),
    );

    // Scoped tools/list exposes EXACTLY 7 tools.
    let list = scoped.rpc("tools/list", json!({})).await;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 7);

    // Calling external host tool in scoped mode -> FORBIDDEN_SCOPE.
    let (is_err, body) = scoped
        .call_tool("dolgorae_interaction_answer", json!({}))
        .await;
    assert!(is_err && body["error"]["code"] == "FORBIDDEN_SCOPE");

    // Ancestor role cycle (`lead` on same workspace) -> DELEGATION_CYCLE.
    let (is_err, body) = scoped
        .call_tool(
            "actor_delegate",
            json!({ "role": "lead", "prompt": "loop" }),
        )
        .await;
    assert!(is_err && body["error"]["code"] == "DELEGATION_CYCLE");

    // Valid `actor_delegate` spawns child + submits prompt.
    let (is_err, delegated) = scoped
        .call_tool(
            "actor_delegate",
            json!({ "prompt": "inspect src", "driver": "mock-drv" }),
        )
        .await;
    assert!(!is_err, "delegate failed: {delegated}");
    let child_id = delegated["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_turn_id = delegated["turn"]["turn_id"].as_str().unwrap().to_owned();
    let child_ctrl = drv.last_ctrl();

    // Child emits approval -> External Host discovers it over SSE and answers via External MCP!
    let host_client = UdsClient::new(sock.clone());
    let mut child_sse = host_client
        .open_sse(&format!("/v2/sessions/{child_id}/events"), Some("0"))
        .await
        .unwrap();
    child_ctrl
        .emit(NormalizedEvent::InteractionRequested {
            request: InteractionRequest {
                interaction_id: "int-child-1".to_owned(),
                turn_id: child_turn_id.clone(),
                kind: InteractionKind::CommandApproval {
                    command: vec!["git".to_owned(), "status".to_owned()],
                    cwd: None,
                    reason: None,
                },
                original_kind: "command_approval".to_owned(),
                title: "Git status".to_owned(),
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

    let mut int_payload = None;
    for _ in 0..10 {
        let f = tokio::time::timeout(Duration::from_secs(2), child_sse.next_frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if f.event.as_deref() == Some("interaction_requested") {
            int_payload = Some(serde_json::from_str::<Value>(&f.data).unwrap());
            break;
        }
    }
    let int_ev = int_payload.expect("host saw child interaction_requested over SSE");
    let rec = &int_ev["payload"];

    let mut ext = McpTestHarness::spawn(
        McpServer::new(McpServerConfig {
            mode: McpMode::ExternalHost,
            socket_path: sock.clone(),
            actor_token: None,
            self_session_id: None,
        })
        .unwrap(),
    );
    let (is_err, ans) = ext
        .call_tool(
            "dolgorae_interaction_answer",
            json!({
                "session_id": child_id,
                "interaction_id": "int-child-1",
                "turn_id": child_turn_id,
                "session_generation": rec["session_generation"],
                "expected_interaction_revision": rec["interaction_revision"],
                "expected_session_revision": rec["session_revision"],
                "decision_id": "allow"
            }),
        )
        .await;
    assert!(!is_err, "host answer failed: {ans}");
    assert_eq!(ans["status"], "resolved");

    // Child trying to delegate grandchild exceeds depth 1 -> DELEGATION_DEPTH_EXCEEDED.
    let child_tok = engine
        .mint_actor_token(&CallerContext::ExternalHost, &child_id)
        .await
        .unwrap();
    let mut child_mcp = McpTestHarness::spawn(
        McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: sock.clone(),
            actor_token: Some(child_tok),
            self_session_id: Some(child_id.clone()),
        })
        .unwrap(),
    );
    let (is_err, depth_err) = child_mcp
        .call_tool("actor_delegate", json!({ "prompt": "too deep" }))
        .await;
    assert!(is_err && depth_err["error"]["code"] == "DELEGATION_DEPTH_EXCEEDED");

    // Second child (sibling) -> peer mailbox rejected when allow_actor_collaboration == false!
    let (_, delegated2) = scoped
        .call_tool("actor_delegate", json!({ "prompt": "sibling" }))
        .await;
    let sibling_id = delegated2["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (is_err, peer_err) = child_mcp
        .call_tool(
            "actor_mailbox_send",
            json!({ "target_session_id": sibling_id, "body": "psst" }),
        )
        .await;
    assert!(is_err && peer_err["error"]["code"] == "FORBIDDEN_SCOPE");

    // Child -> parent mailbox succeeds & parent reads via `actor_mailbox_read`!
    let (is_err, _) = child_mcp
        .call_tool(
            "actor_mailbox_send",
            json!({ "target_session_id": root.session_id, "body": "done!" }),
        )
        .await;
    assert!(!is_err);
    let (is_err, mbox) = scoped.call_tool("actor_mailbox_read", json!({})).await;
    assert!(!is_err && mbox["messages"].as_array().unwrap().len() == 1);

    // Forged token -> UNAUTHORIZED.
    let mut forged = McpTestHarness::spawn(
        McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: sock,
            actor_token: Some(Zeroizing::new("not-a-valid-token".to_owned())),
            self_session_id: Some(root.session_id.clone()),
        })
        .unwrap(),
    );
    let (is_err, unauth) = forged.call_tool("actor_mailbox_read", json!({})).await;
    assert!(is_err && unauth["error"]["code"] == "UNAUTHORIZED");

    // Parent inspects status & cancels child via `actor_child_status` / `actor_child_cancel`.
    let (is_err, st) = scoped
        .call_tool(
            "actor_child_status",
            json!({ "child_session_id": child_id }),
        )
        .await;
    assert!(!is_err && st["session"]["session_id"] == child_id);
    let (is_err, canceled) = scoped
        .call_tool(
            "actor_child_cancel",
            json!({ "child_session_id": child_id }),
        )
        .await;
    assert!(!is_err && canceled["status"] == "closed");

    let _ = stop_tx.send(());
    let _ = srv_handle.await;
}
