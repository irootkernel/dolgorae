//! EPIC-002 worker-owned app-server draining.
//!
//! A Run's Turn advances because the worker drains the app-server on its own
//! thread, not because some caller is standing there holding the Run still.
//! These cases hold that to its contract: an interrupt and an interaction
//! answer reach a running Turn while another caller is blocked on it, a
//! submitted Turn reaches its terminal with nobody waiting, a caller that
//! vanishes takes nothing down with it, and one Turn produces exactly one
//! terminal however many callers were watching for it.

use base64::Engine as _;
use dolgorae::audit::AuditKind;
use dolgorae::controller::{CredentialCarrier, create_controller_credential};
use dolgorae::domain::{
    Access, Assurance, ControlMode, ControllerKind, ExecutionLane, Purpose, PurposeKind,
};
use dolgorae::event::EventProjection;
use dolgorae::jcs::LosslessJson;
use dolgorae::ledger::Ledger;
use dolgorae::run::{
    AgentConfigurationSnapshot, AppServerFacts, AuditPolicy, CapabilityState, CompatibilityVerdict,
    ControllerBinding, DolgoraeBuild, ExecutableIdentity, InstructionSnapshot,
    ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest, RunStore, launch_contract_digest,
    runtime_profile_snapshot_digest,
};
use dolgorae::turn::ImageDetail;
use dolgorae::worker::{
    ControlRequestV1, ControlResponseV1, ExecutingBuild, REMEMBERED_TERMINAL_TURNS,
    RunControllerAuthority, RunFacts, SessionAttach, TurnControlImage, TurnControlRequest,
    WorkerControlServer, WorkerControlState, WorkerHello, WorkerIdentity, WorkerSession,
    WorkerSessionBootstrap, bind_worker_socket, read_frame, write_control_request,
};
use dolgorae::workspace::{GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode};
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a case waits for the worker to reach a state before it calls the
/// Run stuck.  Generous, because the point is to fail rather than hang.
const SETTLE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// A scripted app-server
// ---------------------------------------------------------------------------

/// What this fixture does on its own, without the case saying so.
#[derive(Clone, Copy, Default)]
struct Behaviour {
    /// Complete every Turn the moment it starts, the way a fast Turn does.
    /// Left off, a Turn runs until the case interrupts or completes it.
    complete_on_start: bool,
    /// Acknowledge `turn/interrupt` and then send no terminal at all, the way
    /// a Codex turn that never settles behaves.
    silent_interrupt: bool,
    /// Refuse the first `turn/start` with a JSON-RPC error on its own reply,
    /// the way an app-server that will not run a turn answers.  Definitive,
    /// not lost: the request was received, decided, and answered.
    refuse_first_turn: bool,
    /// Delay the first `turn/start` reply long enough for shutdown to observe
    /// the drain inside Accept while its published snapshot is still idle.
    first_turn_delay_ms: u64,
    /// Take the connection away the moment `turn/interrupt` arrives, without
    /// ever answering it: the write landed and its outcome is unobservable.
    hang_up_on_interrupt: bool,
}

/// An app-server stand-in: one Unix socket, one WebSocket conversation, and a
/// script the case drives message by message.
struct FakeAppServer {
    socket_path: PathBuf,
    codex_home: PathBuf,
    inbound: Arc<Mutex<Vec<Value>>>,
    arrived: Arc<Condvar>,
    outbound: Arc<Mutex<Option<UnixStream>>>,
    joiner: Option<JoinHandle<()>>,
}

impl FakeAppServer {
    fn start(root: &Path, behaviour: Behaviour) -> Self {
        let socket_path = root.join("app-server.sock");
        let codex_home = root.join("codex-home");
        make_dir(&codex_home);
        let listener = UnixListener::bind(&socket_path).unwrap();
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).unwrap();
        let inbound = Arc::new(Mutex::new(Vec::new()));
        let arrived = Arc::new(Condvar::new());
        let outbound = Arc::new(Mutex::new(None));
        let joiner = thread::spawn({
            let inbound = Arc::clone(&inbound);
            let arrived = Arc::clone(&arrived);
            let outbound = Arc::clone(&outbound);
            let codex_home = codex_home.clone();
            move || {
                serve_app_server(
                    &listener,
                    &codex_home,
                    behaviour,
                    &inbound,
                    &arrived,
                    &outbound,
                )
            }
        });
        Self {
            socket_path,
            codex_home,
            inbound,
            arrived,
            outbound,
            joiner: Some(joiner),
        }
    }

    /// Push one unsolicited message at the Run.
    fn emit(&self, message: &Value) {
        let mut outbound = self.outbound.lock().unwrap();
        let stream = outbound.as_mut().expect("the Run has connected");
        write_server_message(stream, &serde_json::to_vec(message).unwrap()).unwrap();
    }

    fn complete_turn(&self, turn_id: &str, status: &str, text: &str) {
        self.emit(&json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-1",
                "turn": {
                    "id": turn_id,
                    "status": status,
                    "items": [{
                        "type": "agentMessage",
                        "status": "completed",
                        "phase": "final_answer",
                        "threadId": "thread-1",
                        "turnId": turn_id,
                        "text": text,
                    }],
                },
            },
        }));
    }

    fn open_approval(&self, request_id: u64, turn_id: &str) {
        self.emit(&json!({
            "id": request_id,
            "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread-1", "turnId": turn_id, "command": ["ls"]},
        }));
    }

    /// Block until the Run has sent `method` at least `count` times.
    fn await_call(&self, method: &str, count: usize) {
        let deadline = Instant::now() + SETTLE;
        let mut inbound = self.inbound.lock().unwrap();
        loop {
            if inbound
                .iter()
                .filter(|message| message.get("method").and_then(Value::as_str) == Some(method))
                .count()
                >= count
            {
                return;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("the run never sent {method} {count} times"));
            inbound = self.arrived.wait_timeout(inbound, remaining).unwrap().0;
        }
    }

    /// Every request this fixture received naming `method`, in arrival order.
    fn calls(&self, method: &str) -> Vec<Value> {
        self.inbound
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
            .collect()
    }

    /// Every client reply this fixture received to one of its own requests.
    fn replies(&self) -> Vec<Value> {
        self.inbound
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.get("method").is_none())
            .cloned()
            .collect()
    }

    /// Hang up on the Run the way a crashed app-server does.
    fn hang_up(&self) {
        if let Some(stream) = self.outbound.lock().unwrap().take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn stop(&mut self) {
        if let Some(joiner) = self.joiner.take() {
            let _ = joiner.join();
        }
    }
}

impl Drop for FakeAppServer {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn serve_app_server(
    listener: &UnixListener,
    codex_home: &Path,
    behaviour: Behaviour,
    inbound: &Mutex<Vec<Value>>,
    arrived: &Condvar,
    outbound: &Mutex<Option<UnixStream>>,
) {
    let Ok((mut stream, _)) = listener.accept() else {
        return;
    };
    if complete_upgrade(&mut stream).is_none() {
        return;
    }
    *outbound.lock().unwrap() = Some(stream.try_clone().unwrap());
    let mut started_turns = 0_u32;
    while let Some(payload) = read_client_message(&mut stream) {
        let Ok(message) = serde_json::from_slice::<Value>(&payload) else {
            return;
        };
        inbound.lock().unwrap().push(message.clone());
        arrived.notify_all();
        let (Some(id), Some(method)) = (
            message.get("id").and_then(Value::as_u64),
            message.get("method").and_then(Value::as_str),
        ) else {
            continue;
        };
        if method == "turn/interrupt" && behaviour.hang_up_on_interrupt {
            if let Some(stream) = outbound.lock().unwrap().take() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            return;
        }
        let reply = match method {
            "initialize" => json!({"codexHome": codex_home.to_str().unwrap()}),
            "thread/start" => json!({"thread": {"id": "thread-1"}}),
            "turn/start" => {
                started_turns += 1;
                if behaviour.first_turn_delay_ms > 0 && started_turns == 1 {
                    thread::sleep(Duration::from_millis(behaviour.first_turn_delay_ms));
                }
                if behaviour.refuse_first_turn && started_turns == 1 {
                    let error = json!({"id": id, "error": {"code": -32602, "message": "turn input is invalid"}});
                    let mut writer = outbound.lock().unwrap();
                    let _ = write_server_message(
                        writer.as_mut().unwrap(),
                        &serde_json::to_vec(&error).unwrap(),
                    );
                    continue;
                }
                json!({"turn": {"id": format!("turn-{started_turns}")}})
            }
            "turn/interrupt" => json!({}),
            _ => {
                let error = json!({"id": id, "error": {"code": -32601, "message": method}});
                let mut writer = outbound.lock().unwrap();
                let _ = write_server_message(
                    writer.as_mut().unwrap(),
                    &serde_json::to_vec(&error).unwrap(),
                );
                continue;
            }
        };
        let follow_up = match method {
            "turn/start" if behaviour.complete_on_start => {
                Some((format!("turn-{started_turns}"), "completed".to_owned()))
            }
            "turn/interrupt" if !behaviour.silent_interrupt => message
                .get("params")
                .and_then(|params| params.get("turnId"))
                .and_then(Value::as_str)
                .map(|turn_id| (turn_id.to_owned(), "interrupted".to_owned())),
            _ => None,
        };
        {
            let mut writer = outbound.lock().unwrap();
            let stream = writer.as_mut().unwrap();
            let body = json!({"id": id, "result": reply});
            if write_server_message(stream, &serde_json::to_vec(&body).unwrap()).is_err() {
                return;
            }
        }
        if let Some((turn_id, status)) = follow_up {
            let body = json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-1",
                    "turn": {
                        "id": turn_id,
                        "status": status,
                        "items": [{
                            "type": "agentMessage",
                            "status": "completed",
                            "phase": "final_answer",
                            "threadId": "thread-1",
                            "turnId": turn_id,
                            "text": "done",
                        }],
                    },
                },
            });
            let mut writer = outbound.lock().unwrap();
            let stream = writer.as_mut().unwrap();
            if write_server_message(stream, &serde_json::to_vec(&body).unwrap()).is_err() {
                return;
            }
        }
    }
}

fn complete_upgrade(stream: &mut UnixStream) -> Option<()> {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).ok()?;
        request.push(byte[0]);
        if request.len() > 16 * 1024 {
            return None;
        }
    }
    let text = std::str::from_utf8(&request).ok()?;
    let key = text.split("\r\n").find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("sec-websocket-key")
            .then(|| value.trim().to_owned())
    })?;
    let digest = Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    let accept = base64::engine::general_purpose::STANDARD.encode(digest);
    stream
        .write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    stream.flush().ok()
}

/// Read one whole client text message, or `None` once the Run hangs up.
fn read_client_message(stream: &mut UnixStream) -> Option<Vec<u8>> {
    loop {
        let mut prefix = [0_u8; 2];
        stream.read_exact(&mut prefix).ok()?;
        let opcode = prefix[0] & 0x0f;
        let masked = prefix[1] & 0x80 != 0;
        let length = match prefix[1] & 0x7f {
            126 => {
                let mut bytes = [0_u8; 2];
                stream.read_exact(&mut bytes).ok()?;
                usize::from(u16::from_be_bytes(bytes))
            }
            127 => {
                let mut bytes = [0_u8; 8];
                stream.read_exact(&mut bytes).ok()?;
                usize::try_from(u64::from_be_bytes(bytes)).ok()?
            }
            marker => usize::from(marker),
        };
        let mut mask = [0_u8; 4];
        if masked {
            stream.read_exact(&mut mask).ok()?;
        }
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).ok()?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        match opcode {
            0x1 => return Some(payload),
            0x8 => return None,
            _ => {}
        }
    }
}

fn write_server_message(stream: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x81_u8];
    match payload.len() {
        length @ 0..=125 => frame.push(u8::try_from(length).unwrap()),
        length @ 126..=65_535 => {
            frame.push(126);
            frame.extend(u16::try_from(length).unwrap().to_be_bytes());
        }
        length => {
            frame.push(127);
            frame.extend(u64::try_from(length).unwrap().to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// One Run, worker and all
// ---------------------------------------------------------------------------

struct Run {
    root: PathBuf,
    state_root: PathBuf,
    identity: WorkerIdentity,
    control_path: PathBuf,
    control: WorkerControlServer,
    serving: Option<JoinHandle<()>>,
    server: FakeAppServer,
    credential: CredentialCarrier,
    ledger: Arc<Mutex<Ledger>>,
    /// The Run itself, so a case can drive the sequences that have no control
    /// verb of their own: the shutdown settle and the drain's own stop.
    session: Arc<WorkerSession>,
}

impl Run {
    fn start(behaviour: Behaviour) -> Self {
        // A short root: the app-server socket lives inside it, and a Unix
        // socket path is bounded well below what a temporary directory name
        // on this platform would spend.
        let root = PathBuf::from("/tmp").join(format!("dg-drain-{}", Uuid::now_v7().simple()));
        make_dir(&root);
        let state_root = root.join("state");
        make_dir(&state_root);
        make_dir(&state_root.join("runs"));
        make_dir(&state_root.join("runtime"));
        make_dir(&state_root.join("runtime/locks"));
        dolgorae::writer::WriterStore::initialize_layout(
            &state_root,
            &"1".repeat(64),
            fs::metadata(".").unwrap().uid(),
        )
        .unwrap();
        let run_id = Uuid::now_v7();

        let credential_path = root.join("controller.json");
        let created = create_controller_credential(
            &credential_path,
            ControllerKind::HumanCli,
            "cli".to_owned(),
            None,
            None,
        )
        .unwrap();
        let credential = CredentialCarrier::open_path(&credential_path).unwrap();
        let binding = dolgorae::controller::binding_from_carrier(&credential, 1).unwrap();
        assert_eq!(
            binding.identity.controller_id,
            created.credential.controller_id
        );
        RunStore::new(SystemWorkspacePlatform, &state_root)
            .publish(&manifest(run_id, binding))
            .unwrap();

        let ledger_root = state_root.join("runs").join(run_id.to_string());
        prepare_ledger_root(&ledger_root);
        let ledger = Arc::new(Mutex::new(Ledger::open(&ledger_root, run_id).unwrap()));

        let server = FakeAppServer::start(&root, behaviour);
        let session = Arc::new(
            WorkerSession::connect(
                &WorkerSessionBootstrap {
                    app_server_socket: server.socket_path.clone(),
                    canonical_codex_home: server.codex_home.to_str().unwrap().to_owned(),
                    server_key: "a".repeat(64),
                    server_epoch: 1,
                    controller_id: created.credential.controller_id,
                    control_mode: "direct_interactive".to_owned(),
                    fixed_model: "gpt-5".to_owned(),
                    default_effort: "medium".to_owned(),
                    supported_efforts: vec!["medium".to_owned(), "low".to_owned()],
                    cwd: root.clone(),
                    developer_instructions: "fixed".to_owned(),
                    sandbox: "read-only".to_owned(),
                    approval_policy: "on-request".to_owned(),
                    safety_policy: dolgorae::turn::SessionSafetyPolicy::Standard,
                    artifact_root: root.join("artifacts"),
                    attach: SessionAttach::Start,
                    transport_timeout_seconds: 60,
                    dedicated_server: None,
                },
                RunFacts {
                    run_id,
                    profile: "default".to_owned(),
                },
                Arc::clone(&ledger),
                1,
                fs::metadata(".").unwrap().uid(),
                &state_root,
                &"1".repeat(64),
            )
            .unwrap(),
        );

        let identity = worker_identity(run_id);
        let lease = bind_worker_socket(&identity, None).unwrap();
        let control_path = lease.path().to_owned();
        let control = WorkerControlServer::new(
            WorkerHello {
                schema_version: 1,
                identity: identity.clone(),
                control_socket_epoch: 1,
                dolgorae_version: "0.1.0".to_owned(),
                mutation_protocol_version: 1,
                binary_sha256: "3".repeat(64),
            },
            RunFacts {
                run_id,
                profile: "default".to_owned(),
            },
            WorkerControlState {
                lifecycle: "idle".to_owned(),
                active_turn: None,
            },
            RunControllerAuthority::new(state_root.clone(), run_id),
        );
        let serving = thread::spawn({
            let control = control.clone();
            move || control.serve(&lease).unwrap()
        });
        control
            .attach_run(Arc::clone(&session), Arc::clone(&ledger))
            .unwrap();
        let run = Self {
            root,
            state_root,
            identity,
            control_path,
            control,
            serving: Some(serving),
            server,
            credential,
            ledger,
            session,
        };
        run.await_ready();
        run
    }

    fn await_ready(&self) {
        let deadline = Instant::now() + SETTLE;
        while self.try_status().is_none() {
            assert!(Instant::now() < deadline, "control socket never answered");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn try_status(&self) -> Option<ControlResponseV1> {
        let mut stream = UnixStream::connect(&self.control_path).ok()?;
        stream.set_read_timeout(Some(SETTLE)).ok()?;
        let request = ControlRequestV1::Status {
            expected: self.identity.clone(),
        };
        write_control_request(&mut stream, &request, None).ok()?;
        read_frame(&mut BufReader::new(stream)).ok()
    }

    /// One complete control exchange.
    fn call(&self, request: &ControlRequestV1) -> ControlResponseV1 {
        self.begin(request).join().unwrap()
    }

    /// Write one control request now and read its reply on another thread.
    ///
    /// Splitting the write from the read is what makes "that caller is still
    /// blocked" a fact rather than a guess: the request is on the socket
    /// before this returns, and the handle stays unfinished until the worker
    /// answers it.
    fn begin(&self, request: &ControlRequestV1) -> JoinHandle<ControlResponseV1> {
        let mut stream = UnixStream::connect(&self.control_path).unwrap();
        stream.set_read_timeout(Some(SETTLE)).unwrap();
        let credential = request
            .requires_controller()
            .then(|| self.credential.raw_fd());
        write_control_request(&mut stream, &declared(request), credential).unwrap();
        thread::spawn(move || read_frame(&mut BufReader::new(stream)).unwrap())
    }

    /// Write one control request and hang up without ever reading the reply.
    fn abandon(&self, request: &ControlRequestV1) {
        let mut stream = UnixStream::connect(&self.control_path).unwrap();
        let credential = request
            .requires_controller()
            .then(|| self.credential.raw_fd());
        write_control_request(&mut stream, &declared(request), credential).unwrap();
        drop(stream);
    }

    fn status(&self) -> (String, Option<String>) {
        match self.call(&ControlRequestV1::Status {
            expected: self.identity.clone(),
        }) {
            ControlResponseV1::Status {
                lifecycle,
                active_turn,
                ..
            } => (lifecycle, active_turn),
            other => panic!("expected a status response, got {other:?}"),
        }
    }

    /// Block until the Run publishes `lifecycle`, which is what proves the
    /// drain advanced without a caller waiting on it.
    fn await_lifecycle(&self, lifecycle: &str) -> Option<String> {
        let deadline = Instant::now() + SETTLE;
        loop {
            let (observed, active_turn) = self.status();
            if observed == lifecycle {
                return active_turn;
            }
            assert!(
                Instant::now() < deadline,
                "run stayed {observed:?} instead of reaching {lifecycle:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Read the durable event page.  Observers must keep answering while a
    /// Turn is running, which is what a drain that took the ledger for the
    /// length of a Turn would cost.
    fn events(&self) -> usize {
        match self.call(&ControlRequestV1::Events {
            caller: None,
            expected: self.identity.clone(),
            after: 0,
            projection: EventProjection::Operational,
            limit: 256,
        }) {
            ControlResponseV1::Events { deliveries, .. } => deliveries.len(),
            other => panic!("expected an events response, got {other:?}"),
        }
    }

    /// Every terminal this Run wrote to its durable ledger, in order.
    fn recorded_terminals(&self) -> Vec<(String, String)> {
        let ledger = self.ledger.lock().unwrap();
        ledger
            .durable_records()
            .unwrap()
            .iter()
            .filter(|record| record.kind() == AuditKind::TurnTerminal)
            .map(|record| {
                let LosslessJson::Object(members) = record.payload() else {
                    panic!("a terminal record carries an object payload");
                };
                let text = |name: &str| {
                    members
                        .iter()
                        .find(|(key, _)| key == name)
                        .and_then(|(_, value)| match value {
                            LosslessJson::String(text) => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default()
                };
                (text("turn_id"), text("status"))
            })
            .collect()
    }

    fn turn(&self, key: &str) -> TurnControlRequest {
        TurnControlRequest {
            write: false,
            normalized_request_sha256: None,
            message: format!("do {key}"),
            idempotency_key: key.to_owned(),
            effort: None,
            images: Vec::new(),
        }
    }

    /// One Turn that names its own effort and carries one detailed image.
    fn detailed_turn(&self, key: &str, effort: &str, image: TurnControlImage) -> ControlRequestV1 {
        ControlRequestV1::Submit {
            caller: None,
            expected: self.identity.clone(),
            request: TurnControlRequest {
                write: false,
                normalized_request_sha256: None,
                message: format!("do {key}"),
                idempotency_key: key.to_owned(),
                effort: Some(effort.to_owned()),
                images: vec![image],
            },
        }
    }

    /// The frozen control-v1 shutdown, and whether it confirmed a terminal.
    fn shutdown(&self) -> bool {
        match self.call(&ControlRequestV1::Shutdown {
            expected: self.identity.clone(),
        }) {
            ControlResponseV1::Shutdown {
                terminal_confirmed, ..
            } => terminal_confirmed,
            other => panic!("expected a shutdown response, got {other:?}"),
        }
    }

    /// The whole `status` answer, including the terminal it carries.
    ///
    /// That is the ordinary, skew-checked request: the frozen control-v1
    /// `status` answer may not grow a member, so the terminal is asked for by
    /// name rather than smuggled onto the frozen wire.
    fn full_status(&self) -> ControlResponseV1 {
        self.call(&ControlRequestV1::RunStatus {
            caller: None,
            expected: self.identity.clone(),
        })
    }

    /// One control exchange written exactly as given, with no build
    /// declaration added, answered as the raw JSON that reached the wire.
    fn call_raw(&self, request: &ControlRequestV1) -> Value {
        let mut stream = UnixStream::connect(&self.control_path).unwrap();
        stream.set_read_timeout(Some(SETTLE)).unwrap();
        write_control_request(&mut stream, request, None).unwrap();
        read_frame(&mut BufReader::new(stream)).unwrap()
    }

    /// Every audit kind this Run wrote, in order.
    fn recorded_kinds(&self) -> Vec<AuditKind> {
        let ledger = self.ledger.lock().unwrap();
        ledger
            .durable_records()
            .unwrap()
            .iter()
            .map(dolgorae::audit::AuditRecord::kind)
            .collect()
    }

    fn send(&self, key: &str) -> ControlRequestV1 {
        self.send_within(key, None)
    }

    fn send_within(&self, key: &str, timeout_ms: Option<u64>) -> ControlRequestV1 {
        ControlRequestV1::Send {
            caller: None,
            expected: self.identity.clone(),
            request: self.turn(key),
            timeout_ms,
        }
    }

    fn submit(&self, key: &str) -> ControlRequestV1 {
        ControlRequestV1::Submit {
            caller: None,
            expected: self.identity.clone(),
            request: self.turn(key),
        }
    }

    fn wait(&self, turn_id: &str) -> ControlRequestV1 {
        self.wait_within(turn_id, None)
    }

    fn wait_within(&self, turn_id: &str, timeout_ms: Option<u64>) -> ControlRequestV1 {
        ControlRequestV1::Wait {
            caller: None,
            expected: self.identity.clone(),
            turn_id: turn_id.to_owned(),
            timeout_ms,
        }
    }

    fn close(&self, interrupt: bool) -> ControlRequestV1 {
        ControlRequestV1::Close {
            caller: None,
            expected: self.identity.clone(),
            interrupt,
        }
    }

    fn pause(&self, interrupt: bool) -> ControlRequestV1 {
        ControlRequestV1::Pause {
            caller: None,
            expected: self.identity.clone(),
            interrupt,
        }
    }

    fn resume(&self) -> ControlRequestV1 {
        ControlRequestV1::Resume {
            caller: None,
            expected: self.identity.clone(),
        }
    }

    fn reset_fence(&self) -> ControlRequestV1 {
        ControlRequestV1::ResetFence {
            caller: None,
            expected: self.identity.clone(),
            confirmation: self.identity.run_id,
        }
    }

    /// The fsynced reset token an operator PREPARE leaves behind, naming the
    /// Controller generation it is replacing.
    fn write_reset_prepare(&self) {
        let path = self
            .state_root
            .join("runs")
            .join(self.identity.run_id.to_string())
            .join("recovery/controller-reset.jsonl");
        make_dir(path.parent().unwrap());
        let record = json!({
            "schema_version": 1,
            "operation_id": Uuid::now_v7(),
            "status": "prepared",
            "run_id": self.identity.run_id,
            "controller_generation": 1,
        });
        fs::write(&path, format!("{record}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn fenced(response: &ControlResponseV1) -> (String, Option<String>, usize) {
    match response {
        ControlResponseV1::ResetFence {
            lifecycle,
            active_turn,
            pending_interactions,
            ..
        } => (
            lifecycle.clone(),
            active_turn.clone(),
            *pending_interactions,
        ),
        other => panic!("expected a reset fence answer, got {other:?}"),
    }
}

fn failure_code(response: &ControlResponseV1) -> &str {
    match response {
        ControlResponseV1::Failed { code, .. } => code,
        other => panic!("expected a failure response, got {other:?}"),
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.control.stop();
        // The Run is stopped before the accept loop is joined, so a caller
        // still waiting on a Turn is answered rather than stranded.
        let _ = self.control.detach_run();
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
        self.server.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn accepted(response: &ControlResponseV1) -> String {
    match response {
        ControlResponseV1::Accepted { accepted } => accepted.turn_id.clone(),
        other => panic!("expected an accepted turn, got {other:?}"),
    }
}

/// The Turn one answer names, whether the drain had settled it yet or not.
///
/// A Turn the app-server completes the instant it starts can be answered
/// either way, and a case that only wants the Turn's identity should not have
/// to care which race it won.
fn turn_settled(response: &ControlResponseV1) -> String {
    match response {
        ControlResponseV1::Accepted { accepted } => accepted.turn_id.clone(),
        ControlResponseV1::Terminal { terminal } => terminal.turn_id.clone(),
        other => panic!("expected an accepted or settled turn, got {other:?}"),
    }
}

fn terminal(response: &ControlResponseV1) -> (String, String) {
    match response {
        ControlResponseV1::Terminal { terminal } => {
            (terminal.turn_id.clone(), terminal.status.clone())
        }
        other => panic!("expected a terminal turn, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn checked_submit_rejects_stale_revision_before_any_turn_effect() {
    let run = Run::start(Behaviour::default());
    let before = run.ledger.lock().unwrap().head().unwrap().sequence;
    let stale = ControlRequestV1::CheckedMutation {
        expected_state_revision: before + 1,
        request: Box::new(run.submit("stale")),
    };
    assert_eq!(failure_code(&run.call(&stale)), "RUN_STATE_CONFLICT");
    assert_eq!(run.ledger.lock().unwrap().head().unwrap().sequence, before);
    assert!(run.server.calls("turn/start").is_empty());
    assert!(run.server.calls("thread/start").is_empty());
}

#[test]
fn checked_submit_replay_preserves_the_original_acceptance_and_requires_controller() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    let before = run.ledger.lock().unwrap().head().unwrap().sequence;
    let request = ControlRequestV1::CheckedMutation {
        expected_state_revision: before,
        request: Box::new(run.submit("receipt")),
    };
    let first = run.call(&request);
    let ControlResponseV1::AcceptedReceipt {
        accepted: first_turn,
        operation_id,
        stamp,
        state,
        writer,
        controller,
        effective_policy,
        server_key,
        server_epoch,
    } = first
    else {
        panic!("expected immutable acceptance receipt: {first:?}");
    };
    assert!(!first_turn.replayed);
    assert_eq!(state.lifecycle, dolgorae::domain::RunLifecycle::Running);
    assert_eq!(state.ledger_head.sequence, stamp.run_state_revision);
    run.await_lifecycle("idle");
    // Exercise a real later Writer transaction; replay must retain its earlier observation.
    let binding = RunStore::new(SystemWorkspacePlatform, &run.state_root)
        .load_controller_binding(run.identity.run_id)
        .unwrap();
    let holder = dolgorae::writer::WriterStore::holder(
        run.identity.run_id,
        "default".to_owned(),
        &binding,
        1,
        "a".repeat(64),
        1,
        Some(first_turn.thread_id.clone()),
        dolgorae::domain::RunLifecycle::Idle,
    );
    dolgorae::writer::WriterStore::new(
        &run.state_root,
        &run.identity.workspace_id,
        run.identity.uid,
    )
    .transact(|record| record.prepare_acquire(run.identity.run_id, holder, Uuid::now_v7()))
    .unwrap();
    let replay = run.call(&request);
    let ControlResponseV1::AcceptedReceipt {
        accepted,
        operation_id: replay_op,
        stamp: replay_stamp,
        state: replay_state,
        writer: replay_writer,
        controller: replay_controller,
        effective_policy: replay_policy,
        server_key: replay_server,
        server_epoch: replay_epoch,
    } = replay
    else {
        panic!("expected replay receipt: {replay:?}");
    };
    assert!(accepted.replayed);
    assert_eq!(accepted.turn_id, first_turn.turn_id);
    assert_eq!(
        (
            replay_op,
            replay_stamp,
            replay_state,
            replay_writer,
            replay_controller,
            replay_policy,
            replay_server,
            replay_epoch
        ),
        (
            operation_id,
            stamp,
            state,
            writer,
            controller,
            effective_policy,
            server_key,
            server_epoch
        )
    );
    let unauthenticated = run.call_raw(&declared(&request));
    assert_eq!(unauthenticated["code"], "CONTROLLER_MISMATCH");
    assert_eq!(run.server.calls("turn/start").len(), 1);
}

#[test]
fn prepared_mutation_fences_competitors_and_its_token_survives_reply_loss() {
    let run = Run::start(Behaviour::default());
    let before = run.ledger.lock().unwrap().head().unwrap().sequence;
    let request = run.submit("admitted");
    let begin = ControlRequestV1::BeginPreparedMutation {
        expected_state_revision: before,
        request: Box::new(request.clone()),
    };
    let ControlResponseV1::MutationAdmitted { admission_id } = run.call(&begin) else {
        panic!("admission refused");
    };
    assert!(run.server.calls("turn/start").is_empty());
    let again = run.call(&begin);
    assert!(
        matches!(again, ControlResponseV1::MutationAdmitted { admission_id: same } if same == admission_id)
    );
    assert_eq!(failure_code(&run.call(&run.resume())), "RUN_STATE_CONFLICT");
    let admitted = ControlRequestV1::AdmittedMutation {
        admission_id,
        request: Box::new(request),
    };
    let ControlResponseV1::AcceptedReceipt {
        accepted: first, ..
    } = run.call(&admitted)
    else {
        panic!("admitted Submit refused");
    };
    let ControlResponseV1::AcceptedReceipt {
        accepted: replay, ..
    } = run.call(&admitted)
    else {
        panic!("accepted reply was not replayed");
    };
    assert!(replay.replayed);
    assert_eq!(first.turn_id, replay.turn_id);
    assert_eq!(run.server.calls("turn/start").len(), 1);
    assert_eq!(
        run.recorded_kinds()
            .iter()
            .filter(|kind| **kind == AuditKind::MutationAdmitted)
            .count(),
        1
    );
    assert_eq!(
        run.recorded_kinds()
            .iter()
            .filter(|kind| **kind == AuditKind::MutationCompleted)
            .count(),
        1
    );
}

#[test]
fn a_read_submit_admission_cannot_prepare_writer_access() {
    let run = Run::start(Behaviour::default());
    let revision = run.ledger.lock().unwrap().head().unwrap().sequence;
    let request = run.submit("read-admission");
    let ControlResponseV1::MutationAdmitted { admission_id } =
        run.call(&ControlRequestV1::BeginPreparedMutation {
            expected_state_revision: revision,
            request: Box::new(request.clone()),
        })
    else {
        panic!("read admission refused");
    };
    let binding = RunStore::new(SystemWorkspacePlatform, &run.state_root)
        .load_controller_binding(run.identity.run_id)
        .unwrap();
    let transaction_id = Uuid::now_v7();
    let holder = dolgorae::writer::WriterStore::holder(
        run.identity.run_id,
        "default".to_owned(),
        &binding,
        run.identity.run_generation,
        "a".repeat(64),
        1,
        None,
        dolgorae::domain::RunLifecycle::Idle,
    );
    let (_, (_, writer_generation)) = dolgorae::writer::WriterStore::new(
        &run.state_root,
        &run.identity.workspace_id,
        run.identity.uid,
    )
    .transact(|record| record.prepare_acquire(run.identity.run_id, holder, transaction_id))
    .unwrap();
    let refused = run.call(&ControlRequestV1::AdmittedMutation {
        admission_id,
        request: Box::new(ControlRequestV1::SetWriterAccess {
            expected: run.identity.clone(),
            caller: None,
            write: true,
            writer_generation,
            transaction_id,
        }),
    });
    assert_eq!(failure_code(&refused), "RUN_STATE_CONFLICT");
    assert!(run.server.calls("thread/start").is_empty());
    assert!(run.server.calls("turn/start").is_empty());
    assert!(matches!(
        run.call(&ControlRequestV1::AdmittedMutation {
            admission_id,
            request: Box::new(request),
        }),
        ControlResponseV1::AcceptedReceipt { .. }
    ));
}

#[test]
fn controller_generation_is_checked_before_an_accepted_replay() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("controller-generation")));
    let rejected = ControlRequestV1::ControllerChecked {
        expected_controller_generation: 2,
        request: Box::new(run.submit("controller-generation")),
    };
    assert_eq!(failure_code(&run.call(&rejected)), "CONTROLLER_MISMATCH");
    assert_eq!(run.server.calls("turn/start").len(), 1);
    assert_eq!(run.status().1, Some(turn_id));

    let mut binding = RunStore::new(SystemWorkspacePlatform, &run.state_root)
        .load_controller_binding(run.identity.run_id)
        .unwrap();
    binding.identity.generation += 1;
    let binding_path = run
        .state_root
        .join("runs")
        .join(run.identity.run_id.to_string())
        .join("controller.json");
    fs::write(binding_path, serde_json::to_vec(&binding).unwrap()).unwrap();
    let drifted = ControlRequestV1::CheckedMutation {
        expected_state_revision: 0,
        request: Box::new(run.submit("controller-generation")),
    };
    assert_eq!(failure_code(&run.call(&drifted)), "IDEMPOTENCY_CONFLICT");
    assert_eq!(run.server.calls("turn/start").len(), 1);
}

/// F7, direction one: the drain accepted a Turn before the operator's PREPARE
/// became durable.
///
/// `state.json` is a group-committed projection and can still say "idle" here,
/// which is exactly the race the old file-only reset lost. The fence is
/// answered by the drain itself, so it is ordered strictly after the
/// acceptance and reports the live Turn — and the reset rolls back instead of
/// installing a new Controller over it.
#[test]
fn a_reset_fence_reports_the_turn_the_drain_accepted_before_the_prepare_landed() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("first")));
    run.server.await_call("turn/start", 1);

    run.write_reset_prepare();
    let (lifecycle, active_turn, pending) = fenced(&run.call(&run.reset_fence()));
    assert_eq!(lifecycle, "running");
    assert_eq!(
        active_turn,
        Some(turn_id.clone()),
        "the fence must name the Turn the drain is actually running"
    );
    assert_eq!(pending, 0);

    // And the same durable token has already stopped every further mutation,
    // so nothing else can start under the Controller being replaced.
    assert_eq!(failure_code(&run.call(&run.submit("second"))), "RUN_BUSY");

    run.server.complete_turn(&turn_id, "completed", "done");
    assert_eq!(run.await_lifecycle("idle"), None);
}

/// F7, direction two: the operator's PREPARE became durable before the send
/// reached the drain.
///
/// The worker's authoritative revalidation happens on the drain thread with
/// nothing between it and the effect, so the fence stops the Turn from being
/// accepted at all — and the fence question, queued behind it, sees an idle
/// Run and lets the reset proceed.
#[test]
fn a_send_after_a_durable_prepare_is_refused_and_the_fence_then_sees_an_idle_run() {
    let run = Run::start(Behaviour::default());
    run.write_reset_prepare();

    let refused = run.call(&run.submit("first"));
    // The reset's durable token owns this Run's startup/mutation
    // serialization until it resolves, so a mutation that loses to it is
    // busy; `CONTROLLER_RESET_NOT_ALLOWED` belongs to `run controller reset`.
    assert_eq!(failure_code(&refused), "RUN_BUSY");
    let ControlResponseV1::Failed {
        details, retryable, ..
    } = &refused
    else {
        panic!("expected a failure");
    };
    assert!(retryable, "the fence lifts when the reset resolves");
    assert_eq!(
        details["owner_kind"], "startup",
        "the fence is the prepare in flight, not a corrupt binding"
    );

    let (lifecycle, active_turn, pending) = fenced(&run.call(&run.reset_fence()));
    assert_eq!(lifecycle, "idle");
    assert_eq!(active_turn, None);
    assert_eq!(pending, 0);
    assert!(
        run.server
            .inbound
            .lock()
            .unwrap()
            .iter()
            .all(|message| message.get("method").and_then(Value::as_str) != Some("turn/start")),
        "a refused Turn must never have reached the app-server"
    );
}

/// A pending interaction is live work too, and the fence has to say so.
#[test]
fn a_reset_fence_reports_a_pending_interaction_as_live_work() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("approval")));
    run.server.open_approval(9, &turn_id);
    assert_eq!(
        run.await_lifecycle("waiting_interaction"),
        Some(turn_id.clone())
    );

    run.write_reset_prepare();
    let (lifecycle, active_turn, pending) = fenced(&run.call(&run.reset_fence()));
    assert_eq!(lifecycle, "waiting_interaction");
    assert_eq!(active_turn, Some(turn_id));
    assert_eq!(pending, 1);
}

#[test]
fn a_running_turn_is_interrupted_while_its_send_caller_is_still_blocked() {
    let run = Run::start(Behaviour::default());
    let sending = run.begin(&run.send("long"));
    let active = run
        .await_lifecycle("running")
        .expect("a running turn names itself");

    // The Send caller is blocked on this Turn right now.  Under a worker that
    // drained inside the caller, this is exactly the state in which no second
    // verb could be served at all.
    assert!(
        !sending.is_finished(),
        "the send caller settled before the turn did"
    );
    let interrupted = run.call(&ControlRequestV1::Interrupt {
        caller: None,
        expected: run.identity.clone(),
    });
    assert!(
        matches!(
            &interrupted,
            ControlResponseV1::Interrupted { turn_id, .. } if *turn_id == active
        ),
        "interrupt was not delivered to the running turn: {interrupted:?}"
    );
    run.server.await_call("turn/interrupt", 1);

    assert_eq!(
        terminal(&sending.join().unwrap()),
        (active.clone(), "interrupted".to_owned()),
        "the blocked send caller must learn the outcome the interrupt produced"
    );
    assert_eq!(run.await_lifecycle("idle"), None);
}

#[test]
fn a_submitted_turn_reaches_its_terminal_with_nobody_waiting() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    let turn_id = accepted(&run.call(&run.submit("submitted")));

    // No caller ever waits on this Turn.  The Run advances anyway: its status
    // settles and its durable ledger carries the terminal.
    assert_eq!(run.await_lifecycle("idle"), None);
    assert_eq!(
        run.recorded_terminals(),
        vec![(turn_id.clone(), "completed".to_owned())]
    );
    // The observer page still answers, which a drain that held the ledger for
    // the length of a Turn would have cost.
    assert!(run.events() > 0);

    // A caller that arrives afterwards is told the outcome rather than being
    // left waiting for an event that has already been and gone.
    assert_eq!(
        terminal(&run.call(&run.wait(&turn_id))),
        (turn_id, "completed".to_owned())
    );
}

#[test]
fn an_interaction_pauses_a_blocked_caller_and_its_answer_resumes_the_turn() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("interactive")));

    // A caller is blocked rejoining a running Turn: nothing can settle it yet.
    let paused = run.begin(&run.wait(&turn_id));
    assert!(
        !paused.is_finished(),
        "the waiting caller settled before the turn did"
    );

    // The interaction reaches that blocked caller as a pause, rather than
    // leaving it waiting for a terminal that cannot arrive.
    run.server.open_approval(4_100, &turn_id);
    run.await_lifecycle("waiting_interaction");
    match paused.join().unwrap() {
        ControlResponseV1::WaitingInteraction {
            turn_id: paused_turn,
            requests,
            ..
        } => {
            assert_eq!(paused_turn, turn_id);
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.request_id)
                    .collect::<Vec<_>>(),
                vec![4_100]
            );
        }
        other => panic!("expected a paused turn, got {other:?}"),
    }

    // The answer is accepted and reaches the app-server, not just the Run's
    // own bookkeeping.
    let first_response = run.call(&ControlRequestV1::Respond {
        caller: None,
        expected: run.identity.clone(),
        request_id: 4_100,
        idempotency_key: "approval-4100".to_owned(),
        response: json!({"decision": "accept_once"}),
    });
    assert!(matches!(
        first_response,
        ControlResponseV1::Responded {
            request_id: 4_100,
            resolution_receipt_id: Some(_),
        }
    ));
    let replies = run.server.replies();
    assert!(
        replies.iter().any(|reply| {
            reply.get("id").and_then(Value::as_u64) == Some(4_100)
                && reply.pointer("/result/decision").and_then(Value::as_str) == Some("accept")
        }),
        "the interaction answer never reached the app-server: {replies:?}"
    );
    assert_eq!(
        run.call(&ControlRequestV1::Respond {
            caller: None,
            expected: run.identity.clone(),
            request_id: 4_100,
            idempotency_key: "approval-4100".to_owned(),
            response: json!({"decision": "accept_once"}),
        }),
        first_response
    );
    match run.call(&ControlRequestV1::Respond {
        caller: None,
        expected: run.identity.clone(),
        request_id: 4_100,
        idempotency_key: "approval-other".to_owned(),
        response: json!({"decision": "decline"}),
    }) {
        ControlResponseV1::Failed { code, .. } => {
            assert_eq!(code, "INTERACTION_ALREADY_RESOLVED")
        }
        other => panic!("expected resolved conflict, got {other:?}"),
    }

    // The Turn resumes, and the next caller blocks on it again.
    run.await_lifecycle("running");
    let resumed = run.begin(&run.wait(&turn_id));
    assert!(
        !resumed.is_finished(),
        "the resumed turn settled without completing"
    );
    run.server
        .complete_turn(&turn_id, "completed", "after approval");
    assert_eq!(
        terminal(&resumed.join().unwrap()),
        (turn_id, "completed".to_owned())
    );
}

#[test]
fn pinned_file_change_snapshot_reaches_a_durable_approval_and_upstream_answer() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("file-change")));
    run.server.emit(&json!({"method":"item/started","params":{
        "threadId":"thread-1","turnId":turn_id,"startedAtMs":1000,
        "item":{"type":"fileChange","id":"file-1","status":"inProgress","changes":[{
            "path":"qa.txt","kind":{"type":"update","move_path":null},
            "diff":"@@ -1 +1 @@\n-old\n+new\n"
        }]}
    }}));
    run.server.emit(
        &json!({"id":7100,"method":"item/fileChange/requestApproval","params":{
            "threadId":"thread-1","turnId":turn_id,"itemId":"file-1","reason":null,"grantRoot":null
        }}),
    );
    run.await_lifecycle("waiting_interaction");
    let kinds = run.recorded_kinds();
    assert!(
        kinds
            .iter()
            .position(|kind| *kind == AuditKind::AppServerNotification)
            .unwrap()
            < kinds
                .iter()
                .position(|kind| *kind == AuditKind::ApprovalRequested)
                .unwrap()
    );
    assert!(matches!(
        run.call(&ControlRequestV1::Respond {
            caller: None,
            expected: run.identity.clone(),
            request_id: 7100,
            idempotency_key: "file-answer".to_owned(),
            response: json!({"decision":"accept_once"})
        }),
        ControlResponseV1::Responded {
            request_id: 7100,
            resolution_receipt_id: Some(_)
        }
    ));
    let deadline = Instant::now() + SETTLE;
    while !run
        .server
        .replies()
        .iter()
        .any(|reply| reply["id"] == 7100 && reply["result"]["decision"] == "accept")
    {
        assert!(
            Instant::now() < deadline,
            "approved response never reached upstream"
        );
        thread::sleep(Duration::from_millis(5));
    }
    run.server.complete_turn(&turn_id, "completed", "changed");
    run.await_lifecycle("idle");
}

#[test]
fn a_quarantined_pending_response_cannot_restore_the_run_or_reach_upstream() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("quarantine")));
    run.server.open_approval(7200, &turn_id);
    run.await_lifecycle("waiting_interaction");
    run.server.emit(&json!({"method":"item/started","params":{
        "threadId":"thread-1","turnId":"wrong-turn","startedAtMs":1000,
        "item":{"type":"fileChange","id":"file-1","status":"inProgress","changes":[]}
    }}));
    run.await_lifecycle("outcome_unknown");
    assert_eq!(
        failure_code(&run.call(&ControlRequestV1::Respond {
            caller: None,
            expected: run.identity.clone(),
            request_id: 7200,
            idempotency_key: "late".to_owned(),
            response: json!({"decision":"accept_once"})
        })),
        "OUTCOME_UNKNOWN"
    );
    assert_eq!(run.status().0, "outcome_unknown");
    assert!(!run.server.replies().iter().any(|reply| reply["id"] == 7200));
    assert_eq!(
        run.ledger.lock().unwrap().projection().unwrap().lifecycle,
        dolgorae::domain::RunLifecycle::OutcomeUnknown
    );
}

#[test]
fn terminal_pending_requests_are_durably_stale_and_never_answered() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("terminal-pending")));
    run.server.open_approval(7300, &turn_id);
    run.await_lifecycle("waiting_interaction");
    run.server.complete_turn(&turn_id, "interrupted", "");
    run.await_lifecycle("idle");
    assert!(
        run.ledger
            .lock()
            .unwrap()
            .projection()
            .unwrap()
            .pending_requests
            .is_empty()
    );
    assert_eq!(
        failure_code(&run.call(&ControlRequestV1::Respond {
            caller: None,
            expected: run.identity.clone(),
            request_id: 7300,
            idempotency_key: "late".to_owned(),
            response: json!({"decision":"accept_once"})
        })),
        "INTERACTION_STALE"
    );
    assert!(!run.server.replies().iter().any(|reply| reply["id"] == 7300));
}

#[test]
fn a_caller_that_disconnects_mid_turn_does_not_stop_the_drain() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("abandoned")));

    // A caller rejoins the Turn and then vanishes without ever reading its
    // answer.  Nothing about the Run depends on it.
    run.abandon(&run.wait(&turn_id));
    run.server
        .complete_turn(&turn_id, "completed", "unattended");

    assert_eq!(run.await_lifecycle("idle"), None);
    assert_eq!(
        terminal(&run.call(&run.wait(&turn_id))),
        (turn_id, "completed".to_owned())
    );

    // The Run is still usable, which is the part a stalled drain would cost.
    let next = accepted(&run.call(&run.submit("after-abandon")));
    run.server.complete_turn(&next, "completed", "still alive");
    assert_eq!(run.await_lifecycle("idle"), None);
}

#[test]
fn one_turn_produces_one_terminal_however_many_callers_watch_it() {
    let run = Run::start(Behaviour::default());
    let sending = run.begin(&run.send("single-terminal"));
    let turn_id = run
        .await_lifecycle("running")
        .expect("a running turn names itself");
    let first = run.begin(&run.wait(&turn_id));
    let second = run.begin(&run.wait(&turn_id));

    // The app-server repeats the terminal, as a shared server may.  The repeat
    // is noise, not a second outcome.
    run.server.complete_turn(&turn_id, "completed", "once");
    assert_eq!(
        terminal(&sending.join().unwrap()),
        (turn_id.clone(), "completed".to_owned())
    );
    run.server.complete_turn(&turn_id, "completed", "twice");
    run.await_lifecycle("idle");

    for watcher in [first, second] {
        assert_eq!(
            terminal(&watcher.join().unwrap()),
            (turn_id.clone(), "completed".to_owned())
        );
    }
    assert_eq!(
        run.recorded_terminals(),
        vec![(turn_id.clone(), "completed".to_owned())],
        "the repeated terminal was recorded twice"
    );

    // A repeated terminal must not have quarantined the Run either.
    let next = accepted(&run.call(&run.submit("after-duplicate")));
    run.server.complete_turn(&next, "completed", "still alive");
    assert_eq!(run.await_lifecycle("idle"), None);
}

/// H1 / ADR-011: an identity-verified `shutdown` during an active Turn "first
/// interrupts an active turn and waits up to five seconds for terminal
/// history".
///
/// What it reports is what it observed: the interrupt really reaches the
/// app-server, the terminal really lands in the durable ledger, and only then
/// is `terminal_confirmed` true.
#[test]
fn shutdown_interrupts_the_active_turn_and_confirms_the_terminal_it_observed() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("live-at-shutdown")));
    run.await_lifecycle("running");

    assert!(run.shutdown(), "an observed terminal was not confirmed");
    assert_eq!(
        run.server
            .calls("turn/interrupt")
            .iter()
            .filter_map(|call| call
                .get("params")
                .and_then(|params| params.get("turnId"))
                .and_then(Value::as_str)
                .map(str::to_owned))
            .collect::<Vec<_>>(),
        vec![turn_id.clone()],
        "shutdown never asked the app-server to interrupt the live turn"
    );
    assert_eq!(
        run.recorded_terminals(),
        vec![(turn_id, "interrupted".to_owned())],
        "the terminal shutdown claimed is not in the durable ledger"
    );
}

/// H1, the other half: a Turn whose terminal never arrives is `outcome_unknown`
/// on disk and an unconfirmed shutdown to the caller.
///
/// docs/specs/README.md: shutdown "records `outcome_unknown` on expiry before generation
/// cleanup".  The old fabrication — flipping the published state to `idle` and
/// answering `terminal_confirmed:true` — left the caller believing a Turn had
/// ended that was still running, with nothing durable to recover from.
#[test]
fn shutdown_that_observes_no_terminal_records_outcome_unknown_and_says_so() {
    let run = Run::start(Behaviour {
        silent_interrupt: true,
        ..Behaviour::default()
    });
    let turn_id = accepted(&run.call(&run.submit("never-settles")));
    run.await_lifecycle("running");

    assert!(
        !run.shutdown(),
        "a turn that never settled was reported as confirmed terminal"
    );
    assert_eq!(run.server.calls("turn/interrupt").len(), 1);
    assert!(
        run.recorded_terminals().is_empty(),
        "a terminal was recorded for a turn that never produced one"
    );
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the lost turn {turn_id} left no durable outcome_unknown: {:?}",
        run.recorded_kinds()
    );
}

/// H4: `wait` requires both run and turn IDs, and answers about the Turn it
/// was given.
///
/// A settled Turn returns its own outcome while a different Turn is live, a
/// Turn this Run never had is `TURN_NOT_FOUND`, and a caller-supplied timeout
/// "returns the current nonterminal state without interrupting the worker".
#[test]
fn wait_answers_the_turn_it_was_given_and_honours_the_callers_timeout() {
    let run = Run::start(Behaviour::default());
    let first = accepted(&run.call(&run.submit("first")));
    run.server.complete_turn(&first, "completed", "one");
    run.await_lifecycle("idle");
    let second = accepted(&run.call(&run.submit("second")));
    run.await_lifecycle("running");

    // The settled Turn's own outcome, not whatever happens to be running.
    assert_eq!(
        terminal(&run.call(&run.wait(&first))),
        (first, "completed".to_owned())
    );

    // A Turn this Run never had is refused with the contract's own details.
    match run.call(&run.wait("turn-never-existed")) {
        ControlResponseV1::Failed { code, details, .. } => {
            assert_eq!(code, "TURN_NOT_FOUND");
            assert_eq!(
                details,
                json!({"run_id": run.identity.run_id, "turn_id": "turn-never-existed"})
            );
        }
        other => panic!("expected a turn-not-found refusal, got {other:?}"),
    }

    // The caller's own timeout bounds the reply and nothing else: the live
    // Turn is reported as running and is not interrupted.
    match run.call(&run.wait_within(&second, Some(200))) {
        ControlResponseV1::Running {
            turn_id, effort, ..
        } => {
            assert_eq!(turn_id, second);
            assert_eq!(effort, "medium");
        }
        other => panic!("expected the live nonterminal state, got {other:?}"),
    }
    assert!(
        run.server.calls("turn/interrupt").is_empty(),
        "a caller timeout interrupted the worker's turn"
    );
    assert_eq!(run.await_lifecycle("running"), Some(second.clone()));

    // And the same Turn still settles normally afterwards.
    run.server.complete_turn(&second, "completed", "two");
    assert_eq!(
        terminal(&run.call(&run.wait(&second))),
        (second, "completed".to_owned())
    );
}

/// M1: "Pause and close reject running or waiting runs unless `--interrupt` is
/// present."
///
/// Interrupting a Turn is an external effect, so it happens only when the
/// caller asked for it by name; the refusal names the Run, its live state, and
/// the operation.
#[test]
fn close_refuses_a_live_run_until_the_caller_asks_for_the_interrupt() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("live-at-close")));
    run.await_lifecycle("running");

    match run.call(&run.close(false)) {
        ControlResponseV1::Failed {
            code,
            retryable,
            details,
            ..
        } => {
            assert_eq!(code, "RUN_STATE_CONFLICT");
            assert!(!retryable);
            assert_eq!(
                details,
                json!({
                    "run_id": run.identity.run_id,
                    "state": "running",
                    "operation": "run.close",
                })
            );
        }
        other => panic!("expected a state conflict, got {other:?}"),
    }
    assert!(
        run.server.calls("turn/interrupt").is_empty(),
        "close interrupted a turn the caller never authorised it to interrupt"
    );

    // With the flag the worker interrupts first, but does not claim the Run is
    // closed until terminal evidence arrives and a second idle close seals it.
    match run.call(&run.close(true)) {
        ControlResponseV1::Interrupted {
            turn_id: observed, ..
        } => {
            assert_eq!(observed, turn_id);
        }
        other => panic!("expected an interrupt acknowledgement, got {other:?}"),
    }
    run.server.await_call("turn/interrupt", 1);
    run.server.complete_turn(&turn_id, "interrupted", "stopped");
    assert_eq!(
        terminal(&run.call(&run.wait(&turn_id))),
        (turn_id.clone(), "interrupted".to_owned())
    );
    match run.call(&run.close(false)) {
        ControlResponseV1::Closed { .. } => {}
        other => panic!("expected a closed run, got {other:?}"),
    }
    assert_eq!(run.await_lifecycle("closed"), None);
}

#[test]
fn idle_pause_and_resume_are_durable_and_fence_turns() {
    let run = Run::start(Behaviour::default());

    match run.call(&run.pause(false)) {
        ControlResponseV1::Status { lifecycle, .. } => assert_eq!(lifecycle, "paused"),
        other => panic!("expected paused status, got {other:?}"),
    }
    assert_eq!(run.await_lifecycle("paused"), None);
    match run.call(&run.submit("paused-turn")) {
        ControlResponseV1::Failed { code, .. } => assert_eq!(code, "RUN_BUSY"),
        other => panic!("expected paused turn refusal, got {other:?}"),
    }
    match run.call(&run.resume()) {
        ControlResponseV1::Status { lifecycle, .. } => assert_eq!(lifecycle, "idle"),
        other => panic!("expected idle status, got {other:?}"),
    }
    assert_eq!(run.await_lifecycle("idle"), None);
    let kinds = run.recorded_kinds();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == AuditKind::LifecycleTransition)
            .count(),
        2
    );
}

/// H3: `status` carries the terminal the drain observed.
///
/// docs/specs/README.md sends a Master here for the response, usage, and cursor behind the
/// intentionally minimal exit-7 envelope, so `last_terminal` cannot be a
/// permanent null.
#[test]
fn status_carries_the_last_terminal_the_drain_observed() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    match run.full_status() {
        ControlResponseV1::Status { last_terminal, .. } => assert!(
            last_terminal.is_none(),
            "a run with no terminal reported one"
        ),
        other => panic!("expected a status response, got {other:?}"),
    }

    let turn_id = accepted(&run.call(&run.submit("answered")));
    run.await_lifecycle("idle");
    match run.full_status() {
        ControlResponseV1::Status {
            last_terminal: Some(terminal),
            ..
        } => {
            assert_eq!(terminal.turn_id, turn_id);
            assert_eq!(terminal.status, "completed");
            assert_eq!(terminal.effort, "medium");
            assert!(
                terminal.final_response.is_some(),
                "the terminal carried no final response"
            );
        }
        other => panic!("expected a status carrying the terminal, got {other:?}"),
    }
}

/// L2 and L3: what a reader Turn actually puts on the wire.
///
/// docs/specs/README.md pins the reader's turn policy as
/// `sandboxPolicy:{"type":"readOnly","networkAccess":false}`, stores each
/// image's caller-supplied detail token, and has a terminal report the Turn's
/// own reasoning effort rather than the Run's default.
#[test]
fn a_reader_turn_pins_its_sandbox_and_keeps_its_own_effort_and_image_detail() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    let image = run.root.join("evidence.png");
    fs::write(&image, b"not really a png").unwrap();

    let response = run.call(&run.detailed_turn(
        "detailed",
        "low",
        TurnControlImage {
            detail: ImageDetail::High,
            path: image.clone(),
        },
    ));
    let turn_id = match &response {
        ControlResponseV1::Accepted { accepted } => {
            assert_eq!(accepted.effort, "low", "the turn lost its own effort");
            accepted.turn_id.clone()
        }
        ControlResponseV1::Terminal { terminal } => {
            assert_eq!(
                terminal.effort, "low",
                "the terminal lost the turn's effort"
            );
            terminal.turn_id.clone()
        }
        other => panic!("expected an accepted turn, got {other:?}"),
    };

    let started = run.server.calls("turn/start");
    let params = started
        .last()
        .and_then(|call| call.get("params"))
        .expect("the run started a turn");
    assert_eq!(
        params.get("sandboxPolicy"),
        Some(&json!({"type": "readOnly", "networkAccess": false})),
        "the reader turn did not pin its sandbox policy"
    );
    assert_eq!(params.get("effort"), Some(&json!("low")));
    let images = params
        .get("input")
        .and_then(Value::as_array)
        .expect("a turn carries its input")
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("localImage"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        images,
        vec![json!({
            "type": "localImage",
            "path": fs::canonicalize(&image).unwrap().to_str().unwrap(),
            "detail": "high",
        })],
        "the caller's image detail token was not preserved"
    );

    // And the effort survives to the terminal the drain publishes.
    run.await_lifecycle("idle");
    match run.full_status() {
        ControlResponseV1::Status {
            last_terminal: Some(terminal),
            ..
        } => {
            assert_eq!(terminal.turn_id, turn_id);
            assert_eq!(terminal.effort, "low");
        }
        other => panic!("expected a status carrying the terminal, got {other:?}"),
    }
}

/// SPEC-007 freezes `hello`, `status`, and `shutdown` byte-identically, so the
/// terminal a Master reads cannot ride on the frozen `status` answer.
///
/// `ControlResponseV1` denies unknown fields: a member a v1 caller has never
/// heard of is a parse failure for it, not an extra it can ignore.  A frozen
/// answer that grew one would break exactly the binary skew the freeze exists
/// to survive.  So the wider answer is an ordinary request that declares its
/// build, and the frozen one stays its v1 self whatever this Run has observed.
#[test]
fn the_frozen_status_answer_keeps_its_v1_shape_and_the_terminal_moves_off_it() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    let turn_id = turn_settled(&run.call(&run.submit("answered")));
    run.await_lifecycle("idle");

    // The frozen wire, exactly as v1 wrote it — and answered without any build
    // declaration at all, which is the other half of the freeze.
    let frozen = run.call_raw(&ControlRequestV1::Status {
        expected: run.identity.clone(),
    });
    assert_eq!(
        frozen
            .as_object()
            .expect("a status answer is a JSON object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["active_turn", "identity", "lifecycle", "result"],
        "the frozen control-v1 status answer changed shape: {frozen}"
    );

    // The same state plus the terminal, on the request that names its build.
    match run.full_status() {
        ControlResponseV1::Status {
            last_terminal: Some(terminal),
            ..
        } => {
            assert_eq!(terminal.turn_id, turn_id);
            assert_eq!(terminal.status, "completed");
            assert!(
                terminal.final_response.is_some(),
                "the terminal carried no final response"
            );
        }
        other => panic!("expected a status carrying the terminal, got {other:?}"),
    }

    // And that request really is skew-checked, where the frozen one is exempt.
    assert_eq!(
        run.call_raw(&ControlRequestV1::RunStatus {
            caller: None,
            expected: run.identity.clone(),
        }),
        json!({"result": "rejected", "code": "DOLGORAE_PROTOCOL_MISMATCH"}),
        "the terminal-carrying status was answered to an undeclared build"
    );
}

/// ADR-011: the Turn shutdown interrupts and waits for has to be the last one
/// this generation can have.
///
/// A Turn accepted between the interrupt and the terminal wait would be torn
/// down with no interrupt ever sent and no durable record of its outcome —
/// precisely the loss the sequence exists to prevent.  So the Run is fenced
/// before it is even asked which Turn is live.
#[test]
fn shutdown_fences_new_turns_before_it_settles_the_one_it_found() {
    let run = Run::start(Behaviour {
        silent_interrupt: true,
        ..Behaviour::default()
    });
    let turn_id = accepted(&run.call(&run.submit("live-at-shutdown")));
    run.await_lifecycle("running");

    let settling = thread::spawn({
        let session = Arc::clone(&run.session);
        move || session.settle_before_shutdown()
    });
    // The interrupt is on the wire, so the fence is up and the five-second
    // terminal wait this case never satisfies is running.
    run.server.await_call("turn/interrupt", 1);

    let refused = run.call(&run.submit("after-the-fence"));
    assert_eq!(
        failure_code(&refused),
        "TRANSPORT_FAILURE",
        "a turn was taken on after shutdown had claimed the run: {refused:?}"
    );
    assert!(
        !settling.join().unwrap(),
        "a turn that never settled was reported as confirmed terminal"
    );
    assert_eq!(
        run.server.calls("turn/start").len(),
        1,
        "a second turn reached the app-server behind the shutdown fence"
    );
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the lost turn {turn_id} left no durable outcome_unknown: {:?}",
        run.recorded_kinds()
    );
}

/// Shutdown must serialize its interrupt decision behind an Accept already
/// executing on the drain.  The published progress snapshot still says idle
/// until that Accept returns, so consulting the snapshot would report a
/// confirmed terminal without ever interrupting the newly accepted Turn.
#[test]
fn shutdown_waits_for_an_in_flight_accept_before_deciding_no_turn_is_live() {
    let run = Run::start(Behaviour {
        first_turn_delay_ms: 250,
        ..Behaviour::default()
    });
    let submitting = run.begin(&run.submit("accepting-at-shutdown"));
    run.server.await_call("turn/start", 1);

    assert!(
        run.session.settle_before_shutdown(),
        "the Turn interrupted during Accept did not reach its terminal"
    );
    assert!(
        matches!(
            submitting.join().unwrap(),
            ControlResponseV1::Accepted { .. }
        ),
        "the in-flight Accept did not finish before shutdown settled it"
    );
    assert_eq!(
        run.server.calls("turn/interrupt").len(),
        1,
        "shutdown trusted a stale idle snapshot and skipped the interrupt"
    );
}

/// A mutation that prevents the shutdown interrupt from reaching the drain
/// does not consume the interrupted Turn's terminal window.  Shutdown closes
/// the transport after the queue budget instead of placing an immediate
/// Abandon behind a not-yet-started interrupt, and the in-flight write is
/// durably quarantined by its ordinary transport-loss path.
#[test]
fn shutdown_does_not_queue_abandon_behind_an_interrupt_that_never_started() {
    let run = Run::start(Behaviour {
        first_turn_delay_ms: 6_000,
        ..Behaviour::default()
    });
    let submitting = run.begin(&run.submit("stalled-at-shutdown"));
    run.server.await_call("turn/start", 1);

    assert!(
        !run.session.settle_before_shutdown(),
        "a Turn whose accept was still unresolved was reported as terminal"
    );
    assert!(
        matches!(submitting.join().unwrap(), ControlResponseV1::Failed { .. }),
        "the unresolved accepted write did not fail when shutdown closed its transport"
    );
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "shutdown returned before the unresolved write was durably quarantined"
    );
    assert!(
        run.server.calls("turn/interrupt").is_empty(),
        "an interrupt was reported as started while it was still queued"
    );
}

/// docs/specs/README.md: shutdown "records `outcome_unknown` on expiry before generation
/// cleanup" — a requirement, not an attempt.
///
/// The record is not written behind the caller's back after the reply.  By the
/// time shutdown reports the terminal it could not confirm, the ledger already
/// holds the loss and the Run's own published lifecycle already says so; a
/// shutdown that answered first would leave its caller reasoning from a state
/// nothing durable agrees with.
#[test]
fn shutdown_expiry_records_the_outcome_before_it_answers() {
    let run = Run::start(Behaviour {
        silent_interrupt: true,
        ..Behaviour::default()
    });
    let turn_id = accepted(&run.call(&run.submit("never-settles")));
    run.await_lifecycle("running");

    assert!(
        !run.session.settle_before_shutdown(),
        "a turn that never settled was reported as confirmed terminal"
    );
    // Read with no waiting in between: both facts are already true.
    assert_eq!(
        run.session.control_state().lifecycle,
        "outcome_unknown",
        "shutdown answered before the run had given the turn up"
    );
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "turn {turn_id} was given up with no durable record: {:?}",
        run.recorded_kinds()
    );
    assert!(
        run.recorded_terminals().is_empty(),
        "a terminal was recorded for a turn that never produced one"
    );
    // And the caller that comes back for that Turn is told the uncertainty,
    // never that this Run had no such Turn.
    match run.call(&run.wait(&turn_id)) {
        ControlResponseV1::Failed {
            code, retryable, ..
        } => {
            assert_eq!(code, "OUTCOME_UNKNOWN");
            assert!(!retryable);
        }
        other => panic!("expected an outcome-unknown refusal, got {other:?}"),
    }
}

/// ADR-011 again, on the path with no shutdown verb behind it.
///
/// A Run that is simply released — its control socket closed, its session
/// dropped on the way out — has the same unobservable Turn on its hands as an
/// expired shutdown.  The ledger has to say so while this generation still
/// owns it, or the Turn ran with nothing durable to recover it from.
#[test]
fn a_drain_that_stops_under_a_live_turn_records_the_outcome_it_lost() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("live-at-stop")));
    run.await_lifecycle("running");
    assert!(
        !run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the run gave up a turn it was still running"
    );

    run.session.shutdown();

    assert!(
        run.server.calls("turn/interrupt").is_empty(),
        "stopping the drain interrupted a turn nobody asked it to"
    );
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the drain stopped under turn {turn_id} without recording the loss: {:?}",
        run.recorded_kinds()
    );
}

/// docs/specs/README.md: `TRANSPORT_FAILURE` "is retryable only when the operation made no
/// external write.  Any uncertain acceptance emits `false`."
///
/// A transport that dies under an accepted Turn made every write there is: the
/// Turn is on the app-server and may still be running.  Offering that for
/// retry invites a caller to reissue work already underway, so what it is told
/// is the `outcome_unknown` the ledger has just recorded.
#[test]
fn a_transport_lost_under_an_accepted_turn_is_reported_as_outcome_unknown() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("accepted-then-lost")));
    run.await_lifecycle("running");

    run.server.hang_up();

    match run.call(&run.wait(&turn_id)) {
        ControlResponseV1::Failed {
            code,
            retryable,
            details,
            ..
        } => {
            assert_eq!(
                code, "OUTCOME_UNKNOWN",
                "an accepted turn's loss was misreported"
            );
            assert!(!retryable, "an accepted turn's loss was offered for retry");
            assert_eq!(details["run_id"], json!(run.identity.run_id));
            assert_eq!(details["turn_id"], json!(turn_id));
        }
        other => panic!("expected an outcome-unknown refusal, got {other:?}"),
    }
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the lost turn left no durable outcome_unknown: {:?}",
        run.recorded_kinds()
    );
}

/// An app-server that answers `turn/start` with a JSON-RPC error has decided,
/// not disappeared.
///
/// The request reached it, it refused, and it said so on the very reply the
/// operation was waiting for.  Nothing is uncertain, so the Run is not
/// quarantined over it: the caller is told the turn was not accepted and the
/// next one runs normally.  Treating this as a lost answer would spend a whole
/// Run generation on an app-server saying "no".
#[test]
fn a_definitive_app_server_refusal_does_not_quarantine_the_run() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        refuse_first_turn: true,
        ..Behaviour::default()
    });

    let refused = run.call(&run.submit("refused"));
    match &refused {
        ControlResponseV1::Failed {
            code,
            retryable,
            details,
            ..
        } => {
            assert_eq!(code, "TRANSPORT_FAILURE", "got {refused:?}");
            assert!(retryable, "a turn the app-server never accepted was final");
            assert_eq!(details["acceptance"], json!("not_accepted"));
        }
        other => panic!("expected a refused turn, got {other:?}"),
    }
    assert!(
        !run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "a definitive refusal quarantined the run: {:?}",
        run.recorded_kinds()
    );

    // The Run is still usable, which is the whole point.
    assert_eq!(run.await_lifecycle("idle"), None);
    let turn_id = turn_settled(&run.call(&run.submit("accepted")));
    run.await_lifecycle("idle");
    assert_eq!(
        run.recorded_terminals(),
        vec![(turn_id, "completed".to_owned())],
        "the run could not run a turn after refusing one"
    );
    assert_eq!(
        run.recorded_kinds()
            .into_iter()
            .filter(|kind| *kind == AuditKind::ThreadBound)
            .count(),
        1,
        "the first accepted retry never made the provisional Thread permanent"
    );
}

/// An interrupt is an external write like any other.
///
/// Losing its answer leaves the Turn's outcome uncertain rather than merely
/// unsent: the app-server may have interrupted it, may have let it run on, and
/// this Run can no longer tell.  docs/specs/README.md forbids reporting that as a
/// retryable transport hiccup, so the loss quarantines the Run durably and is
/// restated as the uncertainty it is.
#[test]
fn an_interrupt_whose_answer_is_lost_is_outcome_unknown_and_never_retryable() {
    let run = Run::start(Behaviour {
        hang_up_on_interrupt: true,
        ..Behaviour::default()
    });
    let turn_id = accepted(&run.call(&run.submit("interrupted-then-lost")));
    run.await_lifecycle("running");

    let response = run.call(&ControlRequestV1::Interrupt {
        caller: None,
        expected: run.identity.clone(),
    });
    match &response {
        ControlResponseV1::Failed {
            code,
            retryable,
            details,
            ..
        } => {
            assert_eq!(code, "OUTCOME_UNKNOWN", "got {response:?}");
            assert!(
                !retryable,
                "a written interrupt's loss was offered for retry"
            );
            assert_eq!(details["turn_id"], json!(turn_id));
        }
        other => panic!("expected an outcome-unknown refusal, got {other:?}"),
    }
    assert!(
        run.recorded_kinds().contains(&AuditKind::OutcomeUnknown),
        "the lost interrupt left no durable outcome_unknown: {:?}",
        run.recorded_kinds()
    );
}

/// docs/specs/README.md gives a closed Run its own registered lifecycle refusal.
///
/// The identical request was legal a moment ago and no rewording of it will
/// ever be accepted again, so calling it an invalid argument sends the caller
/// to fix input that was never wrong.  `RUN_STATE_CONFLICT` names the state
/// and the operation instead, and carries the exit class that says so.
#[test]
fn a_closed_run_refuses_new_turns_as_a_state_conflict() {
    let run = Run::start(Behaviour::default());
    match run.call(&run.close(false)) {
        ControlResponseV1::Closed { .. } => {}
        other => panic!("expected a closed run, got {other:?}"),
    }
    run.await_lifecycle("closed");

    for (request, operation) in [
        (run.submit("after-close"), "run.submit"),
        (run.send("after-close-send"), "run.send"),
    ] {
        match run.call(&request) {
            ControlResponseV1::Failed {
                code,
                retryable,
                details,
                ..
            } => {
                assert_eq!(code, "RUN_STATE_CONFLICT");
                assert!(!retryable);
                assert_eq!(
                    details,
                    json!({
                        "run_id": run.identity.run_id,
                        "state": "closed",
                        "operation": operation,
                    })
                );
            }
            other => panic!("expected a state conflict for {operation}, got {other:?}"),
        }
    }
    assert!(
        run.server.calls("turn/start").is_empty(),
        "a closed run still started a turn on the app-server"
    );
}

/// H4 again, past the edge of what a live Run can remember.
///
/// A worker's terminal memory is bounded process memory; the `turn_terminal`
/// record is the durable authority for the same fact and is kept for the life
/// of the Run.  A caller that comes back for an older Turn is asking about a
/// Turn this Run really had, so it is answered from the ledger rather than
/// told the Run never started it.
#[test]
fn wait_finds_a_turn_older_than_the_runs_bounded_memory_in_the_durable_ledger() {
    let run = Run::start(Behaviour {
        complete_on_start: true,
        ..Behaviour::default()
    });
    let first = turn_settled(&run.call(&run.submit("oldest")));
    run.await_lifecycle("idle");
    // One more terminal than the Run remembers, so the first has fallen out.
    for index in 0..REMEMBERED_TERMINAL_TURNS {
        let _ = run.call(&run.submit(&format!("filler-{index}")));
        run.await_lifecycle("idle");
    }

    match run.call(&run.wait(&first)) {
        ControlResponseV1::Terminal { terminal } => {
            assert_eq!(terminal.turn_id, first);
            assert_eq!(terminal.status, "completed");
            assert!(
                terminal.final_response.is_some(),
                "the durable record answered without the response it holds"
            );
        }
        other => panic!("expected the recorded terminal for {first}, got {other:?}"),
    }

    // A Turn no ledger record names is still absent, so the fallback did not
    // simply stop refusing.
    assert_eq!(
        failure_code(&run.call(&run.wait("turn-never-existed"))),
        "TURN_NOT_FOUND"
    );
}

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

fn make_dir(path: &Path) {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn prepare_ledger_root(root: &Path) {
    if fs::symlink_metadata(root).is_err() {
        make_dir(root);
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let recovery = root.join("recovery");
    if fs::symlink_metadata(&recovery).is_err() {
        make_dir(&recovery);
    }
    let audit = root.join("audit.jsonl");
    if fs::symlink_metadata(&audit).is_err() {
        fs::write(&audit, b"").unwrap();
    }
    fs::set_permissions(&audit, fs::Permissions::from_mode(0o600)).unwrap();
}

fn worker_identity(run_id: Uuid) -> WorkerIdentity {
    WorkerIdentity {
        workspace_id: "1".repeat(64),
        run_id,
        run_generation: 1,
        boot_uuid: Uuid::parse_str("2e349290-1744-4fc3-bb62-9cbf9f5859c0").unwrap(),
        pid: std::process::id(),
        process_group_id: std::process::id(),
        session_id: std::process::id(),
        uid: fs::metadata(".").unwrap().uid(),
        start_tvsec: 1,
        start_tvusec: 0,
        executable_path: PathBuf::from("/usr/bin/true"),
        executable_device: 1,
        executable_inode: 1,
        executable_sha256: "2".repeat(64),
    }
}

fn manifest(run_id: Uuid, controller: ControllerBinding) -> RunManifest {
    let mut environment = BTreeMap::new();
    environment.insert("LANG".to_owned(), "C".to_owned());
    let mut capabilities = BTreeMap::new();
    capabilities.insert("reader".to_owned(), CapabilityState::Supported);
    let instructions_text = "Do the task.".to_owned();
    let instructions = InstructionSnapshot {
        schema: "dolgorae.instructions/v1".to_owned(),
        common_prefix_version: 1,
        mode_prefix_version: 1,
        purpose_prefix_version: 1,
        normalized_byte_length: instructions_text.len() as u64,
        normalized_sha256: dolgorae::jcs::sha256_hex(instructions_text.as_bytes()),
    };
    let purpose = Purpose {
        kind: PurposeKind::Implementation,
        external_label: None,
    };
    let mut profile = ProfileSnapshot {
        schema_version: 1,
        profile_name: "default".to_owned(),
        canonical_codex_home: "/tmp/codex-home".to_owned(),
        normalized_argv: vec!["/usr/local/bin/codex".to_owned()],
        launch_cwd_policy: "profile_state_directory_v1".to_owned(),
        derived_launch_cwd: "/tmp/dolgorae/profiles/server".to_owned(),
        sanitized_environment: environment,
        enabled_features: Vec::new(),
        disabled_features: Vec::new(),
        process_static_configuration: BTreeMap::new(),
        initial_configuration_observation: BTreeMap::new(),
        executable_identity: ExecutableIdentity {
            resolved_path: LosslessPath::Utf8("/usr/local/bin/codex".to_owned()),
            device: 1,
            inode: 2,
            sha256: "8".repeat(64),
        },
        codex_version: "0.147.0".to_owned(),
        app_server_schema_sha256: "4".repeat(64),
        compatibility_manifest_sha256: "a".repeat(64),
        launch_contract_sha256: "0".repeat(64),
        initial_server_key: "3".repeat(64),
    };
    profile.launch_contract_sha256 = launch_contract_digest(&profile).unwrap();
    let agent_configuration = AgentConfigurationSnapshot {
        schema_version: 1,
        runtime_profile: "default".to_owned(),
        runtime_profile_snapshot_sha256: runtime_profile_snapshot_digest(&profile).unwrap(),
        model: "gpt-5".to_owned(),
        default_effort: "medium".to_owned(),
        purpose: purpose.clone(),
        required_capabilities: vec!["reader".to_owned()],
        role_reference: None,
        normalized_instructions: instructions_text,
        instructions: instructions.clone(),
        execution_lane: ExecutionLane::SharedReadonly,
        required_assurance: Assurance::BestEffortPersonalAlpha,
        native_subagent_policy: "enabled".to_owned(),
    };
    RunManifest {
        schema_version: 1,
        run_id,
        workspace_id: "1".repeat(64),
        canonical_workspace: LosslessPath::Utf8("/tmp/workspace".to_owned()),
        workspace_mode: WorkspaceMode::Git,
        start_baseline: GitBaseline::empty(),
        created_at: "2026-08-22T12:34:56.123456Z".to_owned(),
        initial_access: Access::Read,
        control_mode: ControlMode::DirectInteractive,
        execution_lane: ExecutionLane::SharedReadonly,
        requested_assurance: Assurance::BestEffortPersonalAlpha,
        achieved_assurance: Assurance::BestEffortPersonalAlpha,
        profile,
        global_profile_binding: None,
        agent_configuration,
        profile_capability_snapshot: ProfileCapabilitySnapshot {
            schema_version: 1,
            profile_name: "default".to_owned(),
            server_key: "3".repeat(64),
            server_epoch: 1,
            app_server_version: "0.147.0".to_owned(),
            schema_sha256: "4".repeat(64),
            capabilities,
        },
        app_server: AppServerFacts {
            version: Some("0.147.0".to_owned()),
            schema_status: Some("accepted".to_owned()),
            actual_codex_home: Some("/tmp/codex-home".to_owned()),
        },
        dolgorae: DolgoraeBuild {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            binary_sha256: "5".repeat(64),
            ipc_protocol_version: 1,
        },
        model: "gpt-5".to_owned(),
        initial_reasoning_effort: "medium".to_owned(),
        default_reasoning_effort: "medium".to_owned(),
        instructions,
        controller,
        purpose,
        parent_ref: None,
        required_capabilities: vec!["reader".to_owned()],
        thread_id: None,
        fork_provenance: None,
        write_continuation_provenance: None,
        aggregate_binding: None,
        audit: AuditPolicy::default(),
        compatibility: CompatibilityVerdict::Accepted,
    }
}

/// Restate one control request as the build these fixtures' worker publishes.
///
/// SPEC-004 has a non-frozen request declare the Dolgorae build that composed
/// it, and the worker refuse one that is not its own; these fixtures serve a
/// worker whose hello names build `0.1.0`/`3...3`, so a caller in this test
/// declares exactly that.  Skew is proved by a case that declares something
/// else on purpose, never by every case forgetting to declare anything.
fn declared(request: &ControlRequestV1) -> ControlRequestV1 {
    let mut declared = request.clone();
    declared.declare_caller(ExecutingBuild {
        version: "0.1.0".to_owned(),
        mutation_protocol_version: 1,
        binary_sha256: "3".repeat(64),
    });
    declared
}

/// SPEC-006: `run events` emits "records through the head captured at command
/// start", and a cursor "beyond the authoritative Run ledger head" is
/// `EVENT_CURSOR_INVALID` — a registered cursor refusal with the head in it,
/// never a bare complaint about an argument.
#[test]
fn an_event_page_publishes_its_head_and_refuses_a_cursor_beyond_it() {
    let run = Run::start(Behaviour::default());
    let turn_id = accepted(&run.call(&run.submit("first")));
    run.server.complete_turn(&turn_id, "completed", "done");
    assert_eq!(run.await_lifecycle("idle"), None);

    let page = run.call(&ControlRequestV1::Events {
        caller: None,
        expected: run.identity.clone(),
        after: 0,
        projection: EventProjection::Operational,
        limit: 256,
    });
    let ControlResponseV1::Events {
        next_cursor,
        head_cursor,
        ..
    } = &page
    else {
        panic!("expected an events response, got {page:?}");
    };
    let head = head_cursor.parse::<u64>().unwrap();
    assert!(head > 0, "a Run with a terminal Turn has a durable head");
    assert_eq!(
        next_cursor, head_cursor,
        "an exhausted page resumes at the head it captured"
    );

    let beyond = run.call(&ControlRequestV1::Events {
        caller: None,
        expected: run.identity.clone(),
        after: head + 1,
        projection: EventProjection::Operational,
        limit: 256,
    });
    assert_eq!(failure_code(&beyond), "EVENT_CURSOR_INVALID");
    let ControlResponseV1::Failed { details, .. } = &beyond else {
        panic!("expected a failure");
    };
    assert_eq!(details["run_id"], json!(run.identity.run_id));
    assert_eq!(details["requested_cursor"], (head + 1).to_string());
    assert_eq!(details["head_cursor"], head.to_string());
}
