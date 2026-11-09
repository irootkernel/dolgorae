//! Accountless public-v2 Primary lifecycle proof through the ordinary artifact.
use super::*;
use dolgorae::protocol::public_v2 as pb;
use dolgorae::provisioning::{ProvisioningAdmission, ProvisioningSources, capture_provisioning};
use pb::run_service_client::RunServiceClient;

struct Gateway {
    child: std::process::Child,
    identity: dolgorae::darwin::BsdProcessIdentity,
    image: (PathBuf, u64, u64, String),
    retained: bool,
    response_loss_replacements: u32,
}
impl Gateway {
    fn start(root: &Path, socket: &Path) -> Self {
        use std::io::BufRead as _;
        use std::os::unix::fs::MetadataExt as _;
        let image =
            fs::canonicalize(ordinary_binary().expect("ordinary artifact required")).unwrap();
        let metadata = fs::metadata(&image).unwrap();
        let image = (
            image.clone(),
            metadata.dev(),
            metadata.ino(),
            dolgorae::jcs::sha256_hex(&fs::read(&image).unwrap()),
        );
        let stderr = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join(format!("session-gateway-{}.stderr", Uuid::now_v7())))
            .unwrap();
        let mut child = std::process::Command::new(&image.0)
            .args(["serve", "--socket"])
            .arg(socket)
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let envelope: dolgorae::machine::SuccessEnvelope = serde_json::from_str(&ready).unwrap();
        assert!(envelope.ok);
        assert_eq!(envelope.command, "serve");
        assert_eq!(envelope.data["socket_path"], json!(socket));
        let identity = DarwinSystem.bsd_process_identity(child.id()).unwrap();
        let result = Self {
            child,
            identity,
            image,
            retained: false,
            response_loss_replacements: 0,
        };
        result.check_identity();
        result
    }
    fn check_identity(&self) {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(
            DarwinSystem.bsd_process_identity(self.child.id()).unwrap(),
            self.identity
        );
        assert!(!self.identity.zombie && self.identity.uid == DarwinSystem.current_uid());
        assert_eq!(
            DarwinSystem.live_process_path(self.child.id()).as_ref(),
            Some(&self.image.0)
        );
        let metadata = fs::metadata(&self.image.0).unwrap();
        assert_eq!(
            (metadata.dev(), metadata.ino()),
            (self.image.1, self.image.2)
        );
        assert_eq!(
            dolgorae::jcs::sha256_hex(&fs::read(&self.image.0).unwrap()),
            self.image.3
        );
    }
    fn retain(&mut self, root: &Path) {
        if !self.retained {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(root.join("session-gateway-retained.json"))
                .unwrap();
            file.write_all(
                &serde_json::to_vec(&json!({"identity":self.identity,
                "image":self.image,"reason":"accepted_response_lost"}))
                .unwrap(),
            )
            .unwrap();
            self.retained = true;
        }
    }
    fn clear_retained(&mut self, root: &Path) {
        if self.retained {
            fs::remove_file(root.join("session-gateway-retained.json")).unwrap();
            self.retained = false;
        }
    }
    fn replace_after_response_loss(&mut self, root: &Path, socket: &Path) {
        assert_eq!(
            self.response_loss_replacements, 0,
            "only one response-loss gateway replacement"
        );
        self.check_identity();
        let watch = DarwinSystem.watch_process_exit(self.child.id()).unwrap();
        self.check_identity();
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned gateway did not exit; no force fallback"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(watch.exited().unwrap());
        for _ in 0..5 {
            let error = DarwinSystem
                .bsd_process_identity(self.child.id())
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
            std::thread::sleep(Duration::from_millis(100));
        }
        fs::write(
            root.join("session-response-loss-gateway-exit.json"),
            serde_json::to_vec(
                &json!({"identity":self.identity,"kernel_exit_observed":true,
                "consecutive_absent_samples":5,"signals_sent":1,"scope":"gateway_executor_only"}),
            )
            .unwrap(),
        )
        .unwrap();
        self.clear_retained(root);
        let mut replacement = Self::start(root, socket);
        replacement.response_loss_replacements = 1;
        std::mem::swap(self, &mut replacement);
        replacement.retained = true; // Its old child has already exited and been reaped.
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        if self.retained || self.child.try_wait().unwrap().is_some() {
            return;
        }
        self.check_identity();
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        self.child.wait().unwrap();
    }
}
struct CloseTransport<'a> {
    connection: tonic::transport::Channel,
    gateway: &'a mut Gateway,
    root: &'a Path,
    socket: &'a Path,
    reconcile_calls: u32,
    response_lost: bool,
}
fn context() -> Option<pb::RequestContext> {
    Some(pb::RequestContext {
        protocol_version: 2,
        client_request_id: Uuid::now_v7().to_string(),
        client_instance_id: "task094-native-session".into(),
    })
}
async fn channel(socket: &Path) -> tonic::transport::Channel {
    let socket = socket.to_owned();
    tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let socket = socket.clone();
            async move {
                tokio::net::UnixStream::connect(socket)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap()
}
async fn get(
    runs: &mut RunServiceClient<tonic::transport::Channel>,
    run: &Option<pb::RunRef>,
) -> pb::RunProjection {
    runs.get_run(pb::GetRunRequest {
        context: context(),
        run: run.clone(),
    })
    .await
    .unwrap()
    .into_inner()
    .run
    .unwrap()
}
async fn submit(
    runs: &mut RunServiceClient<tonic::transport::Channel>,
    run: &Option<pb::RunRef>,
    controller: &Option<pb::ControllerCarrierRef>,
    case: &str,
) -> pb::SubmitTurnAccepted {
    let current = get(runs, run).await;
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: run.clone(),
        controller: controller.clone(),
        idempotency_key: format!("task094-{case}"),
        write_intent: pb::WriteIntent::Write as i32,
        message: format!("TASK094 {case}"),
        expected_state_revision: current.state_revision,
        ..Default::default()
    })
    .await
    .unwrap()
    .into_inner()
}
async fn idle(
    runs: &mut RunServiceClient<tonic::transport::Channel>,
    run: &Option<pb::RunRef>,
    case: &str,
) -> pb::RunProjection {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let current = get(runs, run).await;
        if current.lifecycle == pb::RunLifecycle::Idle as i32
            && current
                .last_final_response
                .as_ref()
                .and_then(|r| r.value.as_ref())
                == Some(&pb::final_response::Value::InlineUtf8(format!(
                    "TASK094 {case} final"
                )))
        {
            return current;
        }
        assert!(Instant::now() < deadline, "session Turn did not settle");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
fn snapshot(home: &DolgoraeHome, view: &WorkspaceView, id: &str) -> RunSnapshot {
    RunSnapshot::load(
        &home.workspace_root(&view.workspace_id),
        Uuid::parse_str(id).unwrap(),
        0,
    )
    .unwrap()
}
#[derive(Clone, PartialEq, prost::Message)]
struct RichStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

fn close_detail(status: &tonic::Status) -> pb::DolgoraeErrorDetail {
    use prost::Message as _;
    let rich = RichStatus::decode(status.details()).unwrap();
    assert_eq!(rich.code, status.code() as i32);
    assert_eq!(rich.message, status.message());
    assert_eq!(rich.details.len(), 1);
    assert_eq!(
        rich.details[0].type_url,
        "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail"
    );
    let detail = pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.detail_version, 2);
    assert_eq!(detail.dolgorae_error_code, status.message());
    detail
}

fn accepted_close_recovery(status: &tonic::Status, run_id: &str) {
    let detail = close_detail(status);
    assert_eq!(detail.dolgorae_error_code, "RECOVERY_REQUIRED");
    assert_eq!(detail.operation_kind, pb::OperationKind::CloseRun as i32);
    assert_eq!(detail.run_id.as_deref(), Some(run_id));
    assert_eq!(detail.effect, pb::EffectState::Accepted as i32);
    assert_eq!(
        detail.classification,
        pb::FailureClassification::RecoveryNeeded as i32
    );
    assert_eq!(detail.action, pb::RequiredClientAction::RecoverRun as i32);
    assert_eq!(
        detail.retry_classification,
        pb::RetryClassification::Forbidden as i32
    );
    assert_eq!(
        detail.recovery_classification,
        pb::RecoveryClassification::SnapshotRequired as i32
    );
    if let Some(operation) = detail.operation_id {
        Uuid::parse_str(&operation).unwrap();
    }
}

fn unresolved(status: &tonic::Status, kind: pb::OperationKind) -> pb::DolgoraeErrorDetail {
    let detail = close_detail(status);
    assert_eq!(detail.dolgorae_error_code, "TRANSPORT_FAILURE");
    assert_eq!(detail.operation_kind, kind as i32);
    assert_eq!(detail.effect, pb::EffectState::Unresolved as i32);
    assert_eq!(
        detail.classification,
        pb::FailureClassification::Unknown as i32
    );
    assert_eq!(
        detail.action,
        pb::RequiredClientAction::ReconcileOperation as i32
    );
    assert_eq!(
        detail.retry_classification,
        pb::RetryClassification::Forbidden as i32
    );
    assert_eq!(
        detail.recovery_classification,
        pb::RecoveryClassification::ReconcileRequired as i32
    );
    detail
}
fn preserve_response(root: &Path, run: &str, phase: &str, status: &tonic::Status) {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(root.join(format!("session-{run}-{phase}-error.pb")))
        .unwrap();
    file.write_all(status.details()).unwrap();
}
fn retained_operation(
    session: &pb::OrchestratedSessionProjection,
    run: &str,
    interrupt: bool,
) -> String {
    assert_eq!(session.session_id, run);
    assert_eq!(
        session.close_intent,
        if interrupt {
            pb::SessionCloseIntent::Abort
        } else {
            pb::SessionCloseIntent::Complete
        } as i32
    );
    let operation = session.close_operation_id.as_ref().unwrap();
    Uuid::parse_str(operation).unwrap();
    operation.clone()
}
fn same_run(observed: &pb::RunProjection, before: &pb::RunProjection) {
    assert_eq!(observed.run_id, before.run_id);
    assert_eq!(observed.workspace_id, before.workspace_id);
    assert_eq!(observed.controller, before.controller);
    assert_eq!(
        observed.thread.as_ref().map(|t| &t.thread_id),
        before.thread.as_ref().map(|t| &t.thread_id)
    );
}
struct CloseState<'a> {
    run: &'a Option<pb::RunRef>,
    controller: &'a Option<pb::ControllerCarrierRef>,
    before: &'a pb::RunProjection,
    operation: &'a str,
    interrupt: bool,
}
async fn finish_unresolved(
    runs: &mut RunServiceClient<tonic::transport::Channel>,
    state: &CloseState<'_>,
    transport: &mut CloseTransport<'_>,
) -> pb::RunProjection {
    transport.response_lost = true;
    transport.gateway.retain(transport.root);
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let observed = get(runs, state.run).await;
        same_run(&observed, state.before);
        let aggregate = session(transport.connection.clone(), state.run, state.controller).await;
        assert_eq!(
            retained_operation(&aggregate, &observed.run_id, state.interrupt),
            state.operation
        );
        let terminal = if state.interrupt {
            pb::SessionCloseProgress::Aborted
        } else {
            pb::SessionCloseProgress::Completed
        } as i32;
        if aggregate.close_progress == terminal
            && observed.lifecycle == pb::RunLifecycle::Closed as i32
            && observed.writer_authority.as_ref().unwrap().state
                == pb::WriterAuthorityState::None as i32
        {
            transport.gateway.clear_retained(transport.root);
            return observed;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A terminal Run alone does not prove the old executor released its lease.
    // Retire only the exact owned gateway, then reconcile the original operation.
    transport
        .gateway
        .replace_after_response_loss(transport.root, transport.socket);
    transport.connection = channel(transport.socket).await;
    *runs = RunServiceClient::new(transport.connection.clone());
    let fresh = get(runs, state.run).await;
    same_run(&fresh, state.before);
    assert!(fresh.active_turn.is_none() && fresh.pending_interaction_count == 0);
    assert!(matches!(
        pb::RunLifecycle::try_from(fresh.lifecycle).unwrap(),
        pb::RunLifecycle::Idle | pb::RunLifecycle::Closed
    ));
    let aggregate = session(transport.connection.clone(), state.run, state.controller).await;
    assert_eq!(
        retained_operation(&aggregate, &fresh.run_id, state.interrupt),
        state.operation
    );
    assert_eq!(transport.reconcile_calls, 0);
    transport.reconcile_calls += 1;
    match runs
        .reconcile_run(pb::ReconcileRunRequest {
            context: context(),
            run: state.run.clone(),
            controller: state.controller.clone(),
            expected_state_revision: fresh.state_revision,
        })
        .await
    {
        Ok(response) => response.into_inner().run.unwrap(),
        Err(status) => {
            preserve_response(transport.root, &fresh.run_id, "reconcile", &status);
            transport.gateway.retain(transport.root);
            panic!("single original-operation Reconcile did not complete: {status:?}");
        }
    }
}
async fn session(
    connection: tonic::transport::Channel,
    run: &Option<pb::RunRef>,
    controller: &Option<pb::ControllerCarrierRef>,
) -> pb::OrchestratedSessionProjection {
    pb::orchestration_service_client::OrchestrationServiceClient::new(connection)
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: run.clone(),
            controller: controller.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap()
}

// Keep the client read window aligned with the three-attempt snapshot policy.
const WATCH_CAPTURE_ATTEMPTS: usize = 3;

struct WatchReadWindow<'a> {
    before: &'a pb::RunProjection,
    operation: &'a str,
    opened: usize,
}
impl WatchReadWindow<'_> {
    fn record_request(&self, root: &Path, request: &pb::WatchRunEventsRequest) {
        use prost::Message as _;
        let turn = self.before.active_turn.as_ref().unwrap();
        let metadata = json!({
            "original_run_id":self.before.run_id,"original_workspace_id":self.before.workspace_id,
            "original_thread_id":turn.thread_id,"original_turn_id":turn.turn_id,
            "original_exclusive_cursor":self.before.event_cursor,"original_state_revision":self.before.state_revision,
            "original_stamp":self.before.stamp.as_ref().map(|stamp|json!({
                "captured_head_cursor":stamp.captured_head_cursor,"run_state_revision":stamp.run_state_revision,
                "writer_state_revision":stamp.writer_state_revision,"interaction_state_revision":stamp.interaction_state_revision})),
            "retained_close_operation":self.operation,"opening":self.opened,
            "client_request_id":request.context.as_ref().unwrap().client_request_id,
            "client_instance_id":request.context.as_ref().unwrap().client_instance_id,
            "requested_after_cursor":request.after_cursor
        });
        for (extension, bytes) in [
            ("pb", request.encode_to_vec()),
            ("json", serde_json::to_vec(&metadata).unwrap()),
        ] {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(root.join(format!(
                    "session-{}-watch-open-{}.{}",
                    self.before.run_id, self.opened, extension
                )))
                .unwrap();
            file.write_all(&bytes).unwrap();
        }
    }
    fn record_refresh(
        &self,
        root: &Path,
        fresh: &pb::RunProjection,
        aggregate: &pb::OrchestratedSessionProjection,
    ) {
        let metadata = json!({"run_id":fresh.run_id,"workspace_id":fresh.workspace_id,
            "thread_id":fresh.thread.as_ref().map(|t|&t.thread_id),
            "active_turn_id":fresh.active_turn.as_ref().map(|t|&t.turn_id),
            "controller_id":fresh.controller.as_ref().map(|c|&c.controller_id),
            "controller_generation":fresh.controller.as_ref().map(|c|c.generation),
            "captured_head":fresh.event_cursor,"state_revision":fresh.state_revision,
            "session_id":aggregate.session_id,"close_operation_id":aggregate.close_operation_id,
            "close_intent":aggregate.close_intent,"close_progress":aggregate.close_progress});
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join(format!(
                "session-{}-watch-refresh-{}.json",
                self.before.run_id, self.opened
            )))
            .unwrap();
        file.write_all(&serde_json::to_vec(&metadata).unwrap())
            .unwrap();
    }
    fn request(&mut self, run: &Option<pb::RunRef>) -> pb::WatchRunEventsRequest {
        assert!(
            self.opened < WATCH_CAPTURE_ATTEMPTS,
            "Watch capture window exhausted"
        );
        assert_eq!(run.as_ref().unwrap().run_id, self.before.run_id);
        self.opened += 1;
        pb::WatchRunEventsRequest {
            context: context(),
            run: run.clone(),
            after_cursor: self.before.event_cursor.clone(),
            projection: pb::ProjectionProfile::Minimal as i32,
            projection_version: 1,
        }
    }
    fn require_refresh(&self, status: &tonic::Status) {
        let detail = close_detail(status);
        assert_eq!(detail.dolgorae_error_code, "RUN_STATE_CONFLICT");
        assert_eq!(
            detail.operation_kind,
            pb::OperationKind::WatchRunEvents as i32
        );
        assert_eq!(detail.run_id.as_deref(), Some(self.before.run_id.as_str()));
        assert_eq!(detail.effect, pb::EffectState::None as i32);
        assert_eq!(
            detail.classification,
            pb::FailureClassification::Rejected as i32
        );
        assert_eq!(
            detail.action,
            pb::RequiredClientAction::RefreshSnapshot as i32
        );
        assert_eq!(
            detail.retry_classification,
            pb::RetryClassification::Forbidden as i32
        );
        assert_eq!(
            detail.recovery_classification,
            pb::RecoveryClassification::SnapshotRequired as i32
        );
        assert!(
            self.opened < WATCH_CAPTURE_ATTEMPTS,
            "Watch capture window exhausted"
        );
    }
    fn refreshed(&self, fresh: &pb::RunProjection, aggregate: &pb::OrchestratedSessionProjection) {
        same_run(fresh, self.before);
        let original = self
            .before
            .active_turn
            .as_ref()
            .expect("original active Turn");
        if let Some(active) = fresh.active_turn.as_ref() {
            assert_eq!(
                (
                    active.run_id.as_str(),
                    active.thread_id.as_str(),
                    active.turn_id.as_str()
                ),
                (
                    original.run_id.as_str(),
                    original.thread_id.as_str(),
                    original.turn_id.as_str()
                )
            );
        }
        assert_eq!(
            retained_operation(aggregate, &fresh.run_id, true),
            self.operation
        );
        assert!(
            fresh.event_cursor.parse::<u64>().unwrap()
                >= self.before.event_cursor.parse::<u64>().unwrap()
        );
    }
}

async fn interrupted(
    connection: tonic::transport::Channel,
    run: &Option<pb::RunRef>,
    before: &pb::RunProjection,
    controller: &Option<pb::ControllerCarrierRef>,
    root: &Path,
    operation: &str,
) {
    let turn = before.active_turn.as_ref().expect("original active Turn");
    let mut window = WatchReadWindow {
        before,
        operation,
        opened: 0,
    };
    let mut runs = RunServiceClient::new(connection.clone());
    let mut observations =
        pb::observation_service_client::ObservationServiceClient::new(connection.clone());
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let request = window.request(run);
            window.record_request(root, &request);
            let delivered = async {
                let mut stream = observations.watch_run_events(request).await?.into_inner();
                loop {
                    let event = stream.message().await?.expect("terminal event stream");
                    if let Some(pb::run_event_envelope::Item::DurableEvent(event)) = event.item
                        && event.run_id == before.run_id
                        && event.thread_id.as_deref() == Some(turn.thread_id.as_str())
                        && event.turn_id.as_deref() == Some(turn.turn_id.as_str())
                        && let Some(pb::durable_run_event::Event::TurnStateChanged(state)) =
                            event.event
                        && state.current == pb::TurnStatus::Interrupted as i32
                    {
                        return Ok::<(), tonic::Status>(());
                    }
                }
            }
            .await;
            match delivered {
                Ok(()) => break,
                Err(status) => {
                    preserve_response(
                        root,
                        &before.run_id,
                        &format!("watch-capture-{}", window.opened),
                        &status,
                    );
                    window.require_refresh(&status);
                    let fresh = get(&mut runs, run).await;
                    let aggregate = session(connection.clone(), run, controller).await;
                    window.record_refresh(root, &fresh, &aggregate);
                    window.refreshed(&fresh, &aggregate);
                    // The refreshed head is not a consumed event cursor. The
                    // next request retains the original exclusive lower bound.
                }
            }
        }
    })
    .await
    .expect("original Turn interruption was not observed");
}

async fn closed(
    runs: &mut RunServiceClient<tonic::transport::Channel>,
    run: &Option<pb::RunRef>,
    controller: &Option<pb::ControllerCarrierRef>,
    interrupt: bool,
    home: &DolgoraeHome,
    view: &WorkspaceView,
    mut transport: CloseTransport<'_>,
) {
    let connection = transport.connection.clone();
    let mut recover_calls = 0;
    let current = get(runs, run).await;
    let original = snapshot(home, view, &current.run_id);
    let close_result = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: run.clone(),
            controller: controller.clone(),
            interrupt,
            expected_state_revision: current.state_revision,
        })
        .await;
    let (response, expected_operation) = match close_result {
        Ok(response) => (response.into_inner().run.unwrap(), None),
        Err(status) => {
            if status.message() == "TRANSPORT_FAILURE" {
                preserve_response(transport.root, &current.run_id, "close", &status);
                unresolved(&status, pb::OperationKind::CloseRun);
                transport.gateway.retain(transport.root);
                let aggregate = session(connection.clone(), run, controller).await;
                let operation = retained_operation(&aggregate, &current.run_id, interrupt);
                if interrupt {
                    interrupted(
                        connection.clone(),
                        run,
                        &current,
                        controller,
                        transport.root,
                        &operation,
                    )
                    .await;
                }
                let state = CloseState {
                    run,
                    controller,
                    before: &current,
                    operation: &operation,
                    interrupt,
                };
                let response = finish_unresolved(runs, &state, &mut transport).await;
                (response, Some(operation))
            } else {
                // A live-owner SESSION_CLOSE_IN_PROGRESS is snapshot guidance, not
                // acceptance of this request. Never recover through a live owner.
                if status.message() == "SESSION_CLOSE_IN_PROGRESS" {
                    let detail = close_detail(&status);
                    assert_eq!(detail.effect, pb::EffectState::None as i32);
                    assert_eq!(
                        detail.classification,
                        pb::FailureClassification::Rejected as i32
                    );
                    assert_eq!(
                        detail.action,
                        pb::RequiredClientAction::RefreshSnapshot as i32
                    );
                    assert_eq!(
                        detail.retry_classification,
                        pb::RetryClassification::Forbidden as i32
                    );
                    assert_eq!(
                        detail.recovery_classification,
                        pb::RecoveryClassification::SnapshotRequired as i32
                    );
                    let owner = session(connection.clone(), run, controller).await;
                    assert!(owner.close_operation_id.is_some());
                    panic!(
                        "live Close owner observed; current request not accepted; no Recover issued"
                    );
                }
                accepted_close_recovery(&status, &current.run_id);
                let retained = session(connection.clone(), run, controller).await;
                let operation = retained_operation(&retained, &current.run_id, interrupt);
                assert_eq!(
                    retained.close_progress,
                    pb::SessionCloseProgress::RecoveryRequired as i32
                );
                if interrupt {
                    interrupted(
                        connection.clone(),
                        run,
                        &current,
                        controller,
                        transport.root,
                        &operation,
                    )
                    .await;
                }
                let deadline = Instant::now() + Duration::from_secs(90);
                let terminal = loop {
                    let observed = get(runs, run).await;
                    if matches!(
                        pb::RunLifecycle::try_from(observed.lifecycle).unwrap(),
                        pb::RunLifecycle::Idle | pb::RunLifecycle::Closed
                    ) && observed.active_turn.is_none()
                        && observed.pending_interaction_count == 0
                    {
                        break observed;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "interrupted Primary did not settle"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                };
                recover_calls += 1;
                let recovery = runs
                    .recover_run(pb::RecoverRunRequest {
                        context: context(),
                        run: run.clone(),
                        controller: controller.clone(),
                        expected_state_revision: terminal.state_revision,
                    })
                    .await;
                let response = match recovery {
                    Ok(response) => response.into_inner().run.unwrap(),
                    Err(status) => {
                        preserve_response(transport.root, &current.run_id, "recover", &status);
                        unresolved(&status, pb::OperationKind::RecoverRun);
                        let state = CloseState {
                            run,
                            controller,
                            before: &current,
                            operation: &operation,
                            interrupt,
                        };
                        finish_unresolved(runs, &state, &mut transport).await
                    }
                };
                (response, Some(operation))
            }
        }
    };
    let connection = transport.connection.clone();
    if interrupt && expected_operation.is_none() {
        let aggregate = session(connection.clone(), run, controller).await;
        let operation = retained_operation(&aggregate, &current.run_id, true);
        interrupted(
            connection.clone(),
            run,
            &current,
            controller,
            transport.root,
            &operation,
        )
        .await;
    }
    assert_eq!(response.lifecycle, pb::RunLifecycle::Closed as i32);
    assert_eq!(
        response.writer_authority.unwrap().state,
        pb::WriterAuthorityState::None as i32
    );
    let after = snapshot(home, view, &current.run_id);
    assert_eq!(after.projection.lifecycle, RunLifecycle::Closed);
    assert!(
        after
            .writer
            .holder
            .as_ref()
            .is_none_or(|h| h.run_id.to_string() != current.run_id)
    );
    for record in [
        original.runtime_record.as_ref(),
        after.runtime_record.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        dolgorae::worker::prove_worker_generation_absent(record).unwrap();
    }
    if after.runtime_record.is_none() {
        let marker = dolgorae::worker::verified_absence_marker(
            &dolgorae::worker::runtime_root(&home.workspace_root(&view.workspace_id)),
            Uuid::parse_str(&current.run_id).unwrap(),
        )
        .unwrap()
        .expect("latest generation absence marker");
        assert_eq!(marker.run_generation, after.projection.run_generation);
    }
    let inventory = ordinary(
        home.root().parent().unwrap(),
        &[
            "runtime".into(),
            "orphan".into(),
            "inspect".into(),
            "--run-id".into(),
            current.run_id.clone(),
        ],
    )
    .unwrap();
    for candidate in inventory["candidates"].as_array().unwrap() {
        assert_eq!(candidate["verdict"], "absent");
    }
    let retained = session(connection, run, controller).await;
    assert_eq!(retained.session_id, current.run_id);
    assert_eq!(
        retained.close_intent,
        if interrupt {
            pb::SessionCloseIntent::Abort
        } else {
            pb::SessionCloseIntent::Complete
        } as i32
    );
    assert_eq!(
        retained.close_progress,
        if interrupt {
            pb::SessionCloseProgress::Aborted
        } else {
            pb::SessionCloseProgress::Completed
        } as i32
    );
    let operation = retained.close_operation_id.unwrap();
    Uuid::parse_str(&operation).unwrap();
    if let Some(expected) = expected_operation.as_ref() {
        assert_eq!(&operation, expected);
    }
    fs::write(
        home.root()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("session-close-{}.json", current.run_id)),
        serde_json::to_vec(&json!({
            "run_id":current.run_id, "operation_id":operation, "interrupt":interrupt,
            "original_turn_id":current.active_turn.as_ref().map(|turn| &turn.turn_id),
            "exclusive_after_cursor":current.event_cursor,
            "original_turn_interrupted_observed":interrupt,
            "public_recover_calls":recover_calls,"public_reconcile_calls":transport.reconcile_calls,
            "response_lost":transport.response_lost,
            "closed":true, "writer_released":true, "owned_generations_absent":true
        }))
        .unwrap(),
    )
    .unwrap();
}
async fn lose_idle_worker(root: &Path, home: &DolgoraeHome, view: &WorkspaceView, id: &str) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let source = snapshot(home, view, id);
    assert_eq!(source.projection.lifecycle, RunLifecycle::Idle);
    assert!(
        source.projection.active_turn_id.is_none() && source.projection.pending_requests.is_empty()
    );
    let record = source.runtime_record.unwrap();
    assert_eq!(record.identity.run_id.to_string(), id);
    assert_eq!(record.identity.workspace_id, view.workspace_id);
    assert_eq!(record.identity.uid, DarwinSystem.current_uid());
    assert_eq!(
        classify_worker_identity(&record),
        ProcessIdentityVerdict::Match
    );
    let image = fs::canonicalize(ordinary_binary().unwrap()).unwrap();
    let metadata = fs::metadata(&image).unwrap();
    assert_eq!(record.identity.executable_path, image);
    assert_eq!(record.identity.executable_device, metadata.dev());
    assert_eq!(record.identity.executable_inode, metadata.ino());
    let digest = dolgorae::jcs::sha256_hex(&fs::read(&image).unwrap());
    assert_eq!(record.identity.executable_sha256, digest);
    assert_eq!(record.binary_sha256, digest);
    assert_eq!(
        DarwinSystem.live_process_path(record.identity.pid),
        Some(image)
    );
    let inventory = ordinary(
        root,
        &[
            "runtime".into(),
            "orphan".into(),
            "inspect".into(),
            "--run-id".into(),
            id.into(),
        ],
    )
    .unwrap();
    let registrations = inventory["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "worker" && c["pid"] == record.identity.pid && c["run_id"] == id)
        .collect::<Vec<_>>();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0]["verdict"], "owned");
    let before = DarwinSystem
        .bsd_process_identity(record.identity.pid)
        .unwrap();
    assert_eq!(
        (
            before.uid,
            before.start_tvsec,
            before.start_tvusec,
            before.process_group_id,
            before.session_id
        ),
        (
            record.identity.uid,
            record.identity.start_tvsec,
            record.identity.start_tvusec,
            record.identity.process_group_id,
            record.identity.session_id
        )
    );
    let exit = DarwinSystem
        .watch_process_exit(record.identity.pid)
        .unwrap();
    assert_eq!(
        DarwinSystem
            .bsd_process_identity(record.identity.pid)
            .unwrap(),
        before
    );
    assert_eq!(
        classify_worker_identity(&record),
        ProcessIdentityVerdict::Match
    );
    // The fault is confined to the exact, freshly verified idle fixture Worker.
    // Codex and its companion are never signalled by this fixture.
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-TERM", &record.identity.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut exited = false;
    let mut absent = 0;
    while absent < 5 {
        assert!(
            Instant::now() < deadline,
            "owned Worker SIGTERM did not settle"
        );
        exited |= exit.exited().unwrap();
        let _ = DarwinSystem.reap_child_nonblocking(record.identity.pid);
        match DarwinSystem.bsd_process_identity(record.identity.pid) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) && exited => absent += 1,
            Ok(current) => {
                assert_eq!(
                    (
                        current.pid,
                        current.uid,
                        current.start_tvsec,
                        current.start_tvusec,
                        current.process_group_id,
                        current.session_id
                    ),
                    (
                        before.pid,
                        before.uid,
                        before.start_tvsec,
                        before.start_tvusec,
                        before.process_group_id,
                        before.session_id
                    )
                );
                absent = 0;
            }
            Err(error) => panic!("worker identity unavailable after fault: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    fs::write(
        root.join("idle-worker-loss.json"),
        serde_json::to_vec(&json!({
            "identity": record.identity, "registration_id": registrations[0]["id"],
            "kernel_exit_observed": exited, "consecutive_absent_samples": absent,
            "signal": "SIGTERM", "signals_sent": 1, "codex_or_companion_signals": 0
        }))
        .unwrap(),
    )
    .unwrap();
    record.identity.run_generation
}

fn primary_admission() -> ProvisioningAdmission {
    ProvisioningAdmission {
        model: "gpt-5.6-luna".into(),
        effort: "low".into(),
        purpose_kind: "interactive".into(),
        purpose_label: None,
        required_capabilities: vec![],
        execution_lane: "dedicated".into(),
        requested_access: "canonical_workspace_write".into(),
        required_assurance: "best_effort_personal_alpha".into(),
        native_subagent_policy: "enabled".into(),
    }
}

pub(super) fn campaign(
    root: &Path,
    home: &DolgoraeHome,
    view: &WorkspaceView,
    credential: &Path,
) -> Value {
    let binary = ordinary_binary().expect("TASK094 requires ordinary artifact");
    assert!(binary.is_file());
    // Policy validation observes a live base-qualified Profile. Open and close
    // one read-only Caller Run through the product CLI, as the integrated
    // fixture does, before admitting the successor Primary.
    let bootstrap = root.join("session-bootstrap-controller.json");
    ordinary(
        root,
        &[
            "controller".into(),
            "credential".into(),
            "create".into(),
            "--kind".into(),
            "automation".into(),
            "--instance-id".into(),
            "task094-observer-bootstrap".into(),
            "--output".into(),
            bootstrap.to_str().unwrap().into(),
        ],
    )
    .unwrap();
    let observer = start(
        view,
        &bootstrap,
        "fixture-b",
        "task094-observer-bootstrap",
        true,
    )
    .unwrap_or_else(|e| fail(root, "observer Profile bootstrap", e));
    let observer_closed = mutation(
        view,
        &bootstrap,
        observer.snapshot.manifest.run_id,
        RunMutationOperation::Close { interrupt: false },
        "observer bootstrap close",
    );
    assert_eq!(observer_closed.projection.lifecycle, RunLifecycle::Closed);
    let mut policy: Value = serde_json::from_str(include_str!(
        "../../docs/protocol/examples/workspace-policy-input-v3.valid.json"
    ))
    .unwrap();
    policy["policy_name"] = json!("task094-session");
    policy["approval_policy"] = json!("fully_delegated");
    let config = policy["roles"][0]["agent_configuration"]
        .as_object_mut()
        .unwrap();
    config.insert("selected_profile".into(), json!("fixture-b"));
    config.insert("model".into(), json!("gpt-5.6-luna"));
    config.insert("default_effort".into(), json!("low"));
    config.remove("global_profile_binding_sha256");
    let policy_path = root.join("session-policy.json");
    fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    ordinary(
        root,
        &[
            "specialist".into(),
            "policy".into(),
            "add".into(),
            "task094-session".into(),
            "--workspace".into(),
            view.canonical_path
                .to_path_buf()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            "--file".into(),
            policy_path.to_str().unwrap().into(),
        ],
    )
    .unwrap();
    let prepared = dolgorae::markdown_policy::MarkdownPolicyRegistry::discover(Some(
        &view.canonical_path.to_path_buf().unwrap(),
    ))
    .unwrap()
    .prepare_session("task094-session")
    .unwrap();
    let policy_digest = format!("sha256:{}", prepared.policy().digest().unwrap());
    let instruct = dolgorae::instructions::InstructionSourceStore::new(home);
    let binding = dolgorae::global_runtime::ResolvedGlobalProfile::resolve(home, "fixture-a")
        .unwrap()
        .prepare_specialist(home, true)
        .unwrap();
    let captured = capture_provisioning(&ProvisioningSources {
        instruct: &instruct,
        role_name: "general",
        role: dolgorae::instructions::SelectedInstructionSource::Inline(
            "Execute the isolated TASK094 native session cases.\n".into(),
        ),
        dependencies: vec![],
        selected_guidance: vec![],
        profile_binding: binding,
        admission: primary_admission(),
    })
    .unwrap();
    let (instructions, _) = captured.admitted_configuration().unwrap();
    let identity = dolgorae::controller::binding_from_carrier(
        &CredentialCarrier::open_path(credential).unwrap(),
        1,
    )
    .unwrap()
    .identity;
    assert_eq!(identity.kind, ControllerKind::InteractiveClient);
    let controller = Some(pb::ControllerCarrierRef {
        absolute_file_path: credential.to_str().unwrap().into(),
        expected_controller_id: identity.controller_id.to_string(),
        expected_controller_generation: identity.generation,
    });
    let workspace = Some(pb::WorkspaceRef {
        absolute_path: view
            .canonical_path
            .to_path_buf()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        expected_workspace_id: view.workspace_id.clone(),
    });
    let request = pb::StartRunRequest {
        context: context(),
        workspace: workspace.clone(),
        controller: controller.clone(),
        idempotency_key: "task094-first".into(),
        provisioning: Some(pb::ProvisioningSelection {
            profile_name: "fixture-a".into(),
            role_name: "general".into(),
            policy_name: "task094-session".into(),
            policy_snapshot_digest: policy_digest,
            configuration: Some(
                dolgorae::gateway_projection_v2::admitted_configuration(&captured.admitted)
                    .unwrap(),
            ),
            agent_configuration_digest: captured.admitted.digest().unwrap(),
        }),
        requested_mode: pb::CreationMode::Writable as i32,
        execution_lane: pb::ExecutionLane::Dedicated as i32,
        purpose: pb::PurposeKind::Interactive as i32,
        required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
        instructions: Some(dolgorae::gateway_projection_v2::captured_instructions(
            &instructions,
        )),
        ..Default::default()
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let socket = root.join("task094.sock");
    let mut gateway = Some(Gateway::start(root, &socket));
    let (first, run, original_thread, server_id, connection) = runtime.block_on(async {
        let connection = channel(&socket).await;
        let capabilities =
            pb::runtime_service_client::RuntimeServiceClient::new(connection.clone())
                .get_capabilities(pb::GetCapabilitiesRequest {
                    context: context(),
                    minimum_protocol_version: 2,
                    maximum_protocol_version: 2,
                })
                .await
                .unwrap()
                .into_inner();
        assert_eq!(capabilities.context.unwrap().protocol_version, 2);
        assert_eq!(
            capabilities.descriptor_sha256,
            dolgorae::jcs::sha256_hex(dolgorae::protocol::PUBLIC_V2_DESCRIPTOR)
        );
        let mut runs = RunServiceClient::new(connection.clone());
        let created = runs.start_run(request.clone()).await.unwrap().into_inner();
        let server_id = created.context.unwrap().server_instance_id;
        let first = created.run.unwrap();
        let run = Some(pb::RunRef {
            workspace: workspace.clone(),
            run_id: first.run_id.clone(),
        });
        let initial = snapshot(home, view, &first.run_id);
        assert_eq!(initial.manifest.schema_version, 4);
        let aggregate = initial.manifest.aggregate_binding.as_ref().unwrap();
        assert_eq!(
            aggregate.member_kind,
            dolgorae::run::AggregateMemberKind::Primary
        );
        assert_eq!(aggregate.aggregate_id, initial.manifest.run_id);
        submit(&mut runs, &run, &controller, "first-write").await;
        let settled = idle(&mut runs, &run, "first-write").await;
        assert_eq!(
            fs::read(root.join("home/workspace/task094-write")).unwrap(),
            b"task094-write"
        );
        let original_thread = settled.thread.unwrap().thread_id;

        // A yielded command can outlive an idle native Thread. It must still
        // block WRITE admission until its owned child exits naturally.
        submit(&mut runs, &run, &controller, "background").await;
        idle(&mut runs, &run, "background").await;
        assert!(!root.join("home/workspace/task094-background-done").exists());
        let job_pid = fs::read_to_string(root.join("home/workspace/task094-background-pid"))
            .unwrap().parse::<u32>().unwrap();
        let job = DarwinSystem.bsd_process_identity(job_pid).unwrap();
        assert_eq!(job.uid, DarwinSystem.current_uid());
        assert!(!job.zombie);
        let live = snapshot(home, view, &first.run_id);
        let dedicated = live.runtime_record.as_ref().unwrap().dedicated_server_identity.as_ref().unwrap();
        let census = DarwinSystem.all_process_identities().unwrap();
        let mut ancestor = job.parent_pid;
        let mut hops = 0;
        while ancestor != dedicated.pid {
            ancestor = census.iter().find(|entry| entry.pid == ancestor).expect("owned job ancestry").parent_pid;
            hops += 1;
            assert!(hops < 128, "job ancestry must reach its exact Dedicated Server");
        }
        let exit = DarwinSystem.watch_process_exit(job_pid).unwrap();
        assert_eq!(DarwinSystem.bsd_process_identity(job_pid).unwrap(), job);
        fs::write(root.join("background-job-identity.json"), serde_json::to_vec(&job).unwrap()).unwrap();
        let current = get(&mut runs, &run).await;
        let refusal = runs
            .submit_turn(pb::SubmitTurnRequest {
                context: context(),
                run: run.clone(),
                controller: controller.clone(),
                idempotency_key: "task094-background-refusal".into(),
                write_intent: pb::WriteIntent::Write as i32,
                message: "TASK094 must-not-dispatch".into(),
                expected_state_revision: current.state_revision,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(refusal.message(), "RUN_BUSY");
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut exited = false;
        let mut absent = 0;
        while absent < 5 {
            assert!(Instant::now() < deadline, "owned yielded job did not exit naturally");
            exited |= exit.exited().unwrap();
            match DarwinSystem.bsd_process_identity(job_pid) {
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) && exited => absent += 1,
                Ok(current) => {
                    assert_eq!((current.pid, current.uid, current.start_tvsec, current.start_tvusec,
                        current.parent_pid, current.process_group_id, current.session_id),
                        (job.pid, job.uid, job.start_tvsec, job.start_tvusec,
                        job.parent_pid, job.process_group_id, job.session_id));
                    absent = 0;
                }
                Err(error) => panic!("yielded job identity unavailable: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(root.join("home/workspace/task094-background-done").exists());
        fs::write(root.join("background-job-exit.json"),
            serde_json::to_vec(&json!({"pid":job_pid,"birth":[job.start_tvsec,job.start_tvusec],
                "kernel_exit_observed":exited,"consecutive_absent_samples":absent,"signals_sent":0})).unwrap()).unwrap();
        (first, run, original_thread, server_id, connection)
    });
    // Reuse the existing typed answer, receipt replay and lookup checks.
    let interactions = interaction_cases(
        root,
        view,
        credential,
        &runtime,
        connection.clone(),
        controller.clone(),
        Some(Uuid::parse_str(&first.run_id).unwrap()),
    );
    assert_eq!(interactions["sequential_write_admissions_verified"], true);
    drop(connection);
    runtime.block_on(async {
        gateway.take();
        gateway = Some(Gateway::start(root, &socket));
        let connection = channel(&socket).await;
        let mut runs = RunServiceClient::new(connection.clone());
        let observed = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: run.clone(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_ne!(observed.context.unwrap().server_instance_id, server_id);
        let observed = observed.run.unwrap();
        assert_eq!(observed.run_id, first.run_id);
        assert_eq!(observed.thread.as_ref().unwrap().thread_id, original_thread);
        let healthy = snapshot(home, view, &first.run_id);
        let refused = runs
            .recover_run(pb::RecoverRunRequest {
                context: context(),
                run: run.clone(),
                controller: controller.clone(),
                expected_state_revision: observed.state_revision,
            })
            .await
            .unwrap_err();
        assert_eq!(refused.message(), "RUN_STATE_CONFLICT");
        let after_refusal = snapshot(home, view, &first.run_id);
        assert_eq!(after_refusal.projection, healthy.projection);
        assert_eq!(after_refusal.writer, healthy.writer);
        let old_generation = lose_idle_worker(root, home, view, &first.run_id).await;
        let recoverable = get(&mut runs, &run).await;
        let recovered = runs
            .recover_run(pb::RecoverRunRequest {
                context: context(),
                run: run.clone(),
                controller: controller.clone(),
                expected_state_revision: recoverable.state_revision,
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        let restored = snapshot(home, view, &first.run_id);
        assert!(restored.projection.run_generation > old_generation);
        assert_eq!(
            recovered.effective_policy.as_ref().unwrap().access,
            pb::EffectiveAccess::Read as i32
        );
        assert!(
            restored
                .writer
                .holder
                .as_ref()
                .is_none_or(|h| h.run_id.to_string() != first.run_id)
        );
        assert_eq!(recovered.run_id, first.run_id);
        assert_eq!(recovered.thread.unwrap().thread_id, original_thread);
        closed(
            &mut runs,
            &run,
            &controller,
            false,
            home,
            view,
            CloseTransport {
                connection: connection.clone(),
                gateway: gateway.as_mut().unwrap(),
                root,
                socket: &socket,
                reconcile_calls: 0,
                response_lost: false,
            },
        )
        .await;

        let mut second_request = request;
        second_request.context = context();
        second_request.idempotency_key = "task094-interrupt-root".into();
        let second = runs
            .start_run(second_request)
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        assert_eq!(
            snapshot(home, view, &second.run_id).manifest.schema_version,
            4
        );
        let second_run = Some(pb::RunRef {
            workspace,
            run_id: second.run_id,
        });
        submit(&mut runs, &second_run, &controller, "interrupt").await;
        let deadline = Instant::now() + Duration::from_secs(90);
        let active = loop {
            let active = get(&mut runs, &second_run).await;
            if active.lifecycle == pb::RunLifecycle::WaitingInteraction as i32 {
                break active;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let refused = runs
            .close_run(pb::CloseRunRequest {
                context: context(),
                run: second_run.clone(),
                controller: controller.clone(),
                interrupt: false,
                expected_state_revision: active.state_revision,
            })
            .await
            .unwrap_err();
        assert_eq!(refused.message(), "RUN_STATE_CONFLICT");
        closed(
            &mut runs,
            &second_run,
            &controller,
            true,
            home,
            view,
            CloseTransport {
                connection,
                gateway: gateway.as_mut().unwrap(),
                root,
                socket: &socket,
                reconcile_calls: 0,
                response_lost: false,
            },
        )
        .await;
    });
    drop(gateway);
    json!({"schema":"dolgorae.native-caller-pipeline/v1","mode":"task094_session","public_v2_uds":true,
        "first_write_verified":true,"sequential_write_admissions_verified":true,"schema4_primary":true,
        "background_work_refused":true,"natural_child_exit_before_next_write":true,
        "gateway_replacement_original_identity":true,"recover_original_identity":true,"healthy_recover_refused":true,"idle_worker_loss_verified":true,
        "idle_close_writer_released":true,"interrupt_close_writer_released":true,"owned_generations_absent":true})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_recovery_decoder_requires_accepted_typed_recovery() {
        use prost::Message as _;
        let run = Uuid::now_v7().to_string();
        let detail = pb::DolgoraeErrorDetail {
            detail_version: 2,
            dolgorae_error_code: "RECOVERY_REQUIRED".into(),
            operation_kind: pb::OperationKind::CloseRun as i32,
            run_id: Some(run.clone()),
            operation_id: None,
            effect: pb::EffectState::Accepted as i32,
            classification: pb::FailureClassification::RecoveryNeeded as i32,
            action: pb::RequiredClientAction::RecoverRun as i32,
            retry_classification: pb::RetryClassification::Forbidden as i32,
            recovery_classification: pb::RecoveryClassification::SnapshotRequired as i32,
            ..Default::default()
        };
        let status = |detail: &pb::DolgoraeErrorDetail| {
            let code = tonic::Code::FailedPrecondition;
            let rich = RichStatus {
                code: code as i32,
                message: detail.dolgorae_error_code.clone(),
                details: vec![prost_types::Any {
                    type_url: "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail".into(),
                    value: detail.encode_to_vec(),
                }],
            };
            tonic::Status::with_details(
                code,
                &detail.dolgorae_error_code,
                rich.encode_to_vec().into(),
            )
        };
        accepted_close_recovery(&status(&detail), &run);
        let mut rejected = detail;
        rejected.effect = pb::EffectState::None as i32;
        assert!(
            std::panic::catch_unwind(|| accepted_close_recovery(&status(&rejected), &run)).is_err()
        );
        rejected.dolgorae_error_code = "SESSION_CLOSE_IN_PROGRESS".into();
        rejected.classification = pb::FailureClassification::Rejected as i32;
        rejected.action = pb::RequiredClientAction::RefreshSnapshot as i32;
        assert!(
            std::panic::catch_unwind(|| accepted_close_recovery(&status(&rejected), &run)).is_err()
        );
    }

    #[test]
    fn unresolved_response_loss_requires_reconciliation_without_acceptance_claim() {
        use prost::Message as _;
        let mut detail = pb::DolgoraeErrorDetail {
            detail_version: 2,
            dolgorae_error_code: "TRANSPORT_FAILURE".into(),
            operation_kind: pb::OperationKind::RecoverRun as i32,
            effect: pb::EffectState::Unresolved as i32,
            classification: pb::FailureClassification::Unknown as i32,
            action: pb::RequiredClientAction::ReconcileOperation as i32,
            retry_classification: pb::RetryClassification::Forbidden as i32,
            recovery_classification: pb::RecoveryClassification::ReconcileRequired as i32,
            ..Default::default()
        };
        let status = |detail: &pb::DolgoraeErrorDetail| {
            let rich = RichStatus {
                code: tonic::Code::Unavailable as i32,
                message: "TRANSPORT_FAILURE".into(),
                details: vec![prost_types::Any {
                    type_url: "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail".into(),
                    value: detail.encode_to_vec(),
                }],
            };
            tonic::Status::with_details(
                tonic::Code::Unavailable,
                "TRANSPORT_FAILURE",
                rich.encode_to_vec().into(),
            )
        };
        unresolved(&status(&detail), pb::OperationKind::RecoverRun);
        detail.effect = pb::EffectState::Accepted as i32;
        assert!(
            std::panic::catch_unwind(|| unresolved(
                &status(&detail),
                pb::OperationKind::RecoverRun
            ))
            .is_err()
        );
        detail.effect = pb::EffectState::Unresolved as i32;
        detail.action = pb::RequiredClientAction::FixRequest as i32;
        assert!(
            std::panic::catch_unwind(|| unresolved(
                &status(&detail),
                pb::OperationKind::RecoverRun
            ))
            .is_err()
        );
    }

    #[test]
    fn watch_refresh_retains_original_cursor_identity_and_finite_window() {
        use prost::Message as _;
        let run_id = Uuid::now_v7().to_string();
        let thread_id = Uuid::now_v7().to_string();
        let turn_id = Uuid::now_v7().to_string();
        let operation = Uuid::now_v7().to_string();
        let before = pb::RunProjection {
            run_id: run_id.clone(),
            workspace_id: "workspace".into(),
            event_cursor: "16".into(),
            controller: Some(pb::ControllerProjection {
                controller_id: "controller".into(),
                generation: 1,
                ..Default::default()
            }),
            thread: Some(pb::ThreadProjection {
                thread_id: thread_id.clone(),
                ..Default::default()
            }),
            active_turn: Some(pb::TurnProjection {
                run_id: run_id.clone(),
                thread_id,
                turn_id,
                ..Default::default()
            }),
            ..Default::default()
        };
        let run = Some(pb::RunRef {
            run_id: run_id.clone(),
            ..Default::default()
        });
        let aggregate = pb::OrchestratedSessionProjection {
            session_id: run_id.clone(),
            close_intent: pb::SessionCloseIntent::Abort as i32,
            close_operation_id: Some(operation.clone()),
            ..Default::default()
        };
        let fresh = pb::RunProjection {
            event_cursor: "19".into(),
            active_turn: None,
            ..before.clone()
        };
        let mut detail = pb::DolgoraeErrorDetail {
            detail_version: 2,
            dolgorae_error_code: "RUN_STATE_CONFLICT".into(),
            run_id: Some(run_id),
            operation_kind: pb::OperationKind::WatchRunEvents as i32,
            effect: pb::EffectState::None as i32,
            classification: pb::FailureClassification::Rejected as i32,
            action: pb::RequiredClientAction::RefreshSnapshot as i32,
            retry_classification: pb::RetryClassification::Forbidden as i32,
            recovery_classification: pb::RecoveryClassification::SnapshotRequired as i32,
            ..Default::default()
        };
        let status = |detail: &pb::DolgoraeErrorDetail| {
            let rich = RichStatus {
                code: tonic::Code::Aborted as i32,
                message: detail.dolgorae_error_code.clone(),
                details: vec![prost_types::Any {
                    type_url: "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail".into(),
                    value: detail.encode_to_vec(),
                }],
            };
            tonic::Status::with_details(
                tonic::Code::Aborted,
                &detail.dolgorae_error_code,
                rich.encode_to_vec().into(),
            )
        };
        let mut window = WatchReadWindow {
            before: &before,
            operation: &operation,
            opened: 0,
        };
        let evidence =
            PathBuf::from("/tmp").join(format!("native-watch-replay-{}", Uuid::now_v7()));
        fs::create_dir(&evidence).unwrap();
        let first = window.request(&run);
        window.record_request(&evidence, &first);
        window.require_refresh(&status(&detail));
        window.record_refresh(&evidence, &fresh, &aggregate);
        window.refreshed(&fresh, &aggregate);
        let second = window.request(&run);
        window.record_request(&evidence, &second);
        let retained_first = pb::WatchRunEventsRequest::decode(
            fs::read(evidence.join(format!("session-{}-watch-open-1.pb", before.run_id)))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let retained_second = pb::WatchRunEventsRequest::decode(
            fs::read(evidence.join(format!("session-{}-watch-open-2.pb", before.run_id)))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(retained_first, first);
        assert_eq!(retained_second, second);
        let metadata: Value = serde_json::from_slice(
            &fs::read(evidence.join(format!("session-{}-watch-open-1.json", before.run_id)))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            metadata["original_turn_id"],
            before.active_turn.as_ref().unwrap().turn_id
        );
        assert_eq!(metadata["original_exclusive_cursor"], "16");

        assert_eq!(
            (first.after_cursor.as_str(), second.after_cursor.as_str()),
            ("16", "16")
        );
        assert_ne!(
            first.context.unwrap().client_request_id,
            second.context.unwrap().client_request_id
        );
        assert!(window.opened == 2);
        let mut wrong = fresh.clone();
        wrong.thread.as_mut().unwrap().thread_id = "other".into();
        assert!(std::panic::catch_unwind(|| window.refreshed(&wrong, &aggregate)).is_err());
        let mut wrong_turn = fresh.clone();
        wrong_turn.active_turn = before.active_turn.clone();
        wrong_turn.active_turn.as_mut().unwrap().turn_id = "other".into();
        assert!(std::panic::catch_unwind(|| window.refreshed(&wrong_turn, &aggregate)).is_err());
        let mut wrong_operation = aggregate.clone();
        wrong_operation.close_operation_id = Some(Uuid::now_v7().to_string());
        assert!(std::panic::catch_unwind(|| window.refreshed(&fresh, &wrong_operation)).is_err());
        detail.action = pb::RequiredClientAction::FixRequest as i32;
        assert!(std::panic::catch_unwind(|| window.require_refresh(&status(&detail))).is_err());
        detail.action = pb::RequiredClientAction::RefreshSnapshot as i32;
        window.require_refresh(&status(&detail));
        let third = window.request(&run);
        assert_eq!(third.after_cursor, "16");
        assert_eq!(window.opened, WATCH_CAPTURE_ATTEMPTS);
        assert!(std::panic::catch_unwind(|| window.require_refresh(&status(&detail))).is_err());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window.request(&run)))
                .is_err()
        );
        fs::remove_dir_all(evidence).unwrap();
    }

    #[test]
    fn primary_fixture_admission_matches_the_checked_public_wire_contract() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../docs/protocol/examples/v014-identity-admitted-configuration.valid.json"
        ))
        .unwrap();
        for (name, field) in serde_json::to_value(primary_admission())
            .unwrap()
            .as_object()
            .unwrap()
        {
            value[name] = field.clone();
        }
        let admitted: dolgorae::delegation_identity::AdmittedConfiguration =
            serde_json::from_value(value).unwrap();
        admitted.digest().unwrap();
        let wire = dolgorae::gateway_projection_v2::admitted_configuration(&admitted).unwrap();
        assert_eq!(wire.requested_access, pb::CreationMode::Writable as i32);
        assert_eq!(wire.execution_lane, pb::ExecutionLane::Dedicated as i32);
        assert_eq!(wire.purpose_kind, pb::PurposeKind::Interactive as i32);
        assert_eq!(
            wire.native_subagent_policy,
            pb::NativeSubagentPolicy::Enabled as i32
        );
        assert_eq!(
            wire.required_assurance,
            pb::AssuranceLevel::BestEffortPersonalAlpha as i32
        );
        let mut wrong = admitted;
        wrong.requested_access = "writable".into();
        assert!(wrong.digest().is_err());
    }
}
