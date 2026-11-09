//! Explicitly opted-in, isolated caller service acceptance driver.
#![forbid(unsafe_code)]

use dolgorae::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use dolgorae::darwin::DarwinSystem;
use dolgorae::domain::{Access, ControllerKind, PolicyVerification, RunLifecycle};
use dolgorae::global_profile::GlobalProfileStore;
use dolgorae::paths::DolgoraeHome;
use dolgorae::run::RunStore;
use dolgorae::semantic::{
    CoreSemanticService, RunMutationInput, RunMutationOperation, SemanticCommand, SemanticResult,
    SemanticService,
};
use dolgorae::snapshot::RunSnapshot;
use dolgorae::worker::{ProcessIdentityVerdict, classify_worker_identity, read_runtime_record};
use dolgorae::workspace::{
    NativeSubagents, RuntimeProfile, SystemWorkspacePlatform, WorkspaceMode, WorkspaceView,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[path = "native_caller_pipeline/session.rs"]
mod session;

fn ordinary_binary() -> Option<PathBuf> {
    std::env::var_os("DOLGORAE_NATIVE_ORDINARY_BIN").map(PathBuf::from)
}

fn ordinary(root: &Path, args: &[String]) -> Result<Value, dolgorae::machine::MachineError> {
    let binary = ordinary_binary().expect("explicit ordinary product image");
    let output = std::process::Command::new(binary)
        .args(args)
        .output()
        .unwrap();
    let name = format!("ordinary-{}", Uuid::now_v7());
    for (suffix, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(format!("{name}.{suffix}")))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    if output.status.success() {
        let envelope: dolgorae::machine::SuccessEnvelope =
            serde_json::from_slice(&output.stdout).unwrap();
        assert!(envelope.ok);
        Ok(envelope.data)
    } else {
        let envelope: dolgorae::machine::FailureEnvelope =
            serde_json::from_slice(&output.stdout).unwrap();
        assert!(!envelope.ok);
        Err(envelope.error)
    }
}

fn try_mutation(
    view: &WorkspaceView,
    credential: &Path,
    run_id: Uuid,
    operation: RunMutationOperation,
) -> Result<RunSnapshot, dolgorae::machine::MachineError> {
    if ordinary_binary().is_some() {
        let verb = match operation {
            RunMutationOperation::AcquireWriter => "acquire-write",
            RunMutationOperation::ReleaseWriter => "release-write",
            RunMutationOperation::Recover => "recover",
            RunMutationOperation::Resume => "resume",
            RunMutationOperation::Close { interrupt: false } => "close",
            _ => panic!("ordinary caller adapter has no selector for this operation"),
        };
        ordinary(
            credential.parent().unwrap(),
            &[
                "run".into(),
                verb.into(),
                run_id.to_string(),
                "--workspace".into(),
                view.canonical_path
                    .to_path_buf()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                "--controller-file".into(),
                credential.to_str().unwrap().into(),
            ],
        )?;
        let home = DolgoraeHome::system()?;
        return RunSnapshot::load(&home.workspace_root(&view.workspace_id), run_id, 0);
    }
    CoreSemanticService
        .mutate_caller_run(RunMutationInput {
            workspace: view.canonical_path.to_path_buf().unwrap(),
            expected_workspace_id: view.workspace_id.clone(),
            run_id,
            expected_state_revision: None,
            carrier: CredentialCarrier::open_path(credential)?,
            operation,
        })
        .map(|receipt| receipt.snapshot)
}

fn record_failure(root: &Path, stage: &str, error: &dolgorae::machine::MachineError) {
    let mut bytes = serde_json::to_vec(&json!({
        "schema": "dolgorae.native-caller-pipeline-diagnostic/v1",
        "stage": stage,
        "error": error,
    }))
    .unwrap();
    if bytes.len() > 65_536 {
        bytes = serde_json::to_vec(&json!({
            "schema":"dolgorae.native-caller-pipeline-diagnostic/v1", "stage":stage,
            "error_omitted":{"code":error.code,"bytes":bytes.len(),
                "sha256":dolgorae::jcs::sha256_hex(&bytes)},
        }))
        .unwrap();
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(format!("failure-{}.json", Uuid::now_v7())))
        .and_then(|mut file| file.write_all(&bytes))
        .expect("private failure diagnostic could not be retained");
}

fn fail(root: &Path, stage: &str, error: dolgorae::machine::MachineError) -> ! {
    record_failure(root, stage, &error);
    panic!("{stage} failed: {}", error.code);
}

fn mutation(
    view: &WorkspaceView,
    credential: &Path,
    run_id: Uuid,
    operation: RunMutationOperation,
    stage: &str,
) -> RunSnapshot {
    try_mutation(view, credential, run_id, operation)
        .unwrap_or_else(|error| fail(credential.parent().unwrap(), stage, error))
}

fn start(
    view: &WorkspaceView,
    credential: &Path,
    profile: &str,
    key: &str,
    reader: bool,
) -> Result<dolgorae::semantic::RunStartReceipt, dolgorae::machine::MachineError> {
    if ordinary_binary().is_some() {
        let input = start_input(view, profile, key, reader);
        let home = DolgoraeHome::system()?;
        let state = home.workspace_root(&view.workspace_id);
        let binding = dolgorae::controller::binding_from_carrier(
            &CredentialCarrier::open_path(credential)?,
            1,
        )?;
        let digest = dolgorae::run::caller_start_request_digest(
            &input.resolve()?,
            &view.canonical_path,
            &binding,
        )?;
        let reservations =
            dolgorae::run::StartReservationStore::new(SystemWorkspacePlatform, &state);
        let prior = reservations.load_caller(key, &digest)?;
        let value = ordinary(
            credential.parent().unwrap(),
            &[
                "run".into(),
                "start".into(),
                "--workspace".into(),
                view.canonical_path
                    .to_path_buf()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                "--controller-file".into(),
                credential.to_str().unwrap().into(),
                "--profile".into(),
                profile.into(),
                "--control-mode".into(),
                "managed-agent".into(),
                "--access".into(),
                if reader { "read-only" } else { "writable" }.into(),
                "--role".into(),
                if reader { "reviewer" } else { "general" }.into(),
                "--purpose".into(),
                "other".into(),
                "--idempotency-key".into(),
                key.into(),
            ],
        )?;
        let id = Uuid::parse_str(value["run_id"].as_str().unwrap()).unwrap();
        let accepted = reservations.load_caller(key, &digest)?.unwrap();
        assert_eq!(accepted.run_id, id);
        if let Some(prior) = &prior {
            assert_eq!(prior.run_id, id);
        }
        let receipt = RunStore::new(SystemWorkspacePlatform, &state)
            .load_caller_start_receipt(id, &accepted.normalized_identity_sha256)?
            .unwrap();
        assert_eq!(receipt.run_id, id);
        // The CLI intentionally omits exact_replay. This is the isolated
        // fixture's before/after reservation observation, not a CLI field.
        return Ok(dolgorae::semantic::RunStartReceipt {
            snapshot: RunSnapshot::load(&state, id, 0)?,
            exact_replay: prior.is_some(),
        });
    }
    CoreSemanticService.start_caller_run(
        start_input(view, profile, key, reader),
        CredentialCarrier::open_path(credential).unwrap(),
    )
}

fn start_input(
    view: &WorkspaceView,
    profile: &str,
    key: &str,
    reader: bool,
) -> dolgorae::run::CallerRunStartRequest {
    let mut input: Value = serde_json::from_str(include_str!(
        "../docs/protocol/examples/workspace-execution-start-defaults.valid.json"
    ))
    .unwrap();
    input["workspace"] = json!(view.canonical_path.to_path_buf().unwrap());
    input["profile"] = json!(profile);
    input["idempotency_key"] = json!(key);
    if reader {
        input["access"] = json!("read-only");
        input["role"] = json!("reviewer");
    }
    serde_json::from_value(input).unwrap()
}

fn uncertain_start_run(error: &dolgorae::machine::MachineError) -> Option<Uuid> {
    if serde_json::to_vec(error).ok()?.len() > 65_536
        || error.code != "TRANSPORT_FAILURE"
        || error.retryable
        || error.details.as_object()?.len() != 4
        || error.details["acceptance"] != "uncertain"
        || error.details["stage"] != "correlate"
        || error.details.get("request_id") != Some(&Value::Null)
    {
        return None;
    }
    Uuid::parse_str(error.details["run_id"].as_str()?)
        .ok()
        .filter(|run| run.get_version_num() == 7)
}

fn startup_census_refusal(log: &str) -> Option<Value> {
    let mut refusals = log.lines().filter_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value["schema"] == "dolgorae.writer-census-failure/v1").then_some(value)
    });
    let refusal = refusals.next()?;
    if refusals.next().is_some()
        || refusal["error"] != "InvalidIdentity"
        || !matches!(
            refusal["details"]["predicate"].as_str(),
            Some("dedicated_census_membership" | "worker_scope")
        )
    {
        return None;
    }
    Some(refusal)
}

fn default_feature_start(
    root: &Path,
    view: &WorkspaceView,
    credential: &Path,
    state_root: &Path,
) -> (dolgorae::semantic::RunStartReceipt, Value) {
    use dolgorae::domain::{CallerWorkspaceAccess, ExecutionLane};
    use dolgorae::writer::{WriterAuthorityState, WriterStore};

    let error = match start(view, credential, "fixture-a", "first", false) {
        Ok(receipt) => {
            assert!(!receipt.exact_replay);
            let observation = json!({"path":"first_attempt", "initial_error":null,
                "recovery":null, "run_id":receipt.snapshot.manifest.run_id,
                "thread_id":receipt.snapshot.projection.thread_id});
            return (receipt, observation);
        }
        Err(error) => error,
    };
    record_failure(root, "initial default-feature startup", &error);
    let Some(run_id) = uncertain_start_run(&error) else {
        fail(root, "ineligible default-feature startup", error);
    };
    // These retained reads only select the existing recovery entrypoint. Actual
    // Controller, policy and workload proof still belongs to the service/Worker.
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    assert_eq!(fs::read_dir(state_root.join("runs")).unwrap().count(), 1);
    let manifest = store.load_manifest(run_id).unwrap();
    let manifest_path = state_root
        .join("runs")
        .join(run_id.to_string())
        .join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    assert_eq!(manifest.schema_version, 3);
    assert_eq!(
        manifest.requested_access,
        Some(CallerWorkspaceAccess::Writable)
    );
    assert_eq!(manifest.execution_lane, ExecutionLane::Dedicated);
    let carrier = CredentialCarrier::open_path(credential).unwrap();
    assert_eq!(
        dolgorae::controller::binding_from_carrier(
            &carrier,
            manifest.controller.identity.generation
        )
        .unwrap(),
        manifest.controller
    );
    let request = start_input(view, "fixture-a", "first", false)
        .resolve()
        .unwrap();
    let request_digest = dolgorae::run::caller_start_request_digest(
        &request,
        &view.canonical_path,
        &manifest.controller,
    )
    .unwrap();
    let reservation =
        dolgorae::run::StartReservationStore::new(SystemWorkspacePlatform, state_root)
            .load_caller("first", &request_digest)
            .unwrap()
            .unwrap();
    assert_eq!(reservation.run_id, run_id);
    assert_eq!(reservation.accepted_intent.request, request);
    assert!(
        store
            .load_caller_start_receipt(run_id, &reservation.normalized_identity_sha256)
            .unwrap()
            .is_none()
    );
    let projection = store.load_state_projection(run_id).unwrap();
    assert_eq!(projection.lifecycle, RunLifecycle::Idle);
    assert_eq!(projection.access, Access::Write);
    assert!(projection.active_turn_id.is_none() && projection.latest_turn_id.is_none());
    assert!(projection.pending_requests.is_empty());
    let thread = projection.thread_id.clone().unwrap();
    let runtime_path =
        dolgorae::worker::runtime_record_path(&dolgorae::worker::runtime_root(state_root), run_id)
            .unwrap();
    let runtime = read_runtime_record(&runtime_path, DarwinSystem.current_uid()).unwrap();
    assert_eq!(runtime.identity.run_id, run_id);
    assert_eq!(runtime.identity.run_generation, projection.run_generation);
    assert_eq!(
        classify_worker_identity(&runtime),
        ProcessIdentityVerdict::Match
    );
    assert!(!runtime.control_recovery_required);
    let dedicated = runtime.dedicated_server_identity.as_ref().unwrap();
    assert_eq!(
        dolgorae::worker::classify_dedicated_server_identity(dedicated),
        ProcessIdentityVerdict::Match
    );
    let writer_store = WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid());
    let writer = writer_store.load().unwrap();
    assert_eq!(writer.state, WriterAuthorityState::BlockedUnknown);
    assert_eq!(writer.transaction_id, Some(run_id));
    assert_eq!(
        writer.recovery_action.as_deref(),
        Some("reverify_writer_policy")
    );
    assert!(
        json!(writer)
            .get("generation_release")
            .is_none_or(Value::is_null)
    );
    let mut holder = WriterStore::holder(
        run_id,
        manifest.profile.profile_name.clone(),
        &manifest.controller,
        runtime.identity.run_generation,
        manifest.profile_capability_snapshot.server_key.clone(),
        runtime.app_server_epoch.unwrap(),
        Some(thread.clone()),
        RunLifecycle::Idle,
    );
    holder.last_event_cursor = projection.last_event_cursor.clone();
    assert_eq!(writer.holder.as_ref(), Some(&holder));
    let log = fs::read_to_string(
        state_root
            .join("runs")
            .join(run_id.to_string())
            .join(format!(
                "dedicated-server-{}.log",
                runtime.identity.run_generation
            )),
    )
    .unwrap();
    let census =
        startup_census_refusal(&log).expect("one eligible original census refusal required");

    // One explicit recovery, before replay. No delay, policy update or new Run.
    let recovered = mutation(
        view,
        credential,
        run_id,
        RunMutationOperation::Recover,
        "explicit default-feature recovery",
    );
    let recovery_result = json!({"run_id":recovered.manifest.run_id,
        "projection":recovered.projection,"writer":recovered.writer});
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("default-startup-recovery-result.json"))
        .unwrap()
        .write_all(&serde_json::to_vec(&recovery_result).unwrap())
        .unwrap();
    check_policy(state_root, &recovered, Access::Write, &thread);
    assert_eq!(recovered.writer.state, WriterAuthorityState::Active);
    assert_eq!(recovered.writer.writer_generation, writer.writer_generation);
    assert!(recovered.writer.transaction_id.is_none());
    assert_eq!(recovered.manifest, manifest);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_bytes);
    assert_eq!(
        read_runtime_record(&runtime_path, DarwinSystem.current_uid()).unwrap(),
        runtime
    );
    let replay = start(view, credential, "fixture-a", "first", false)
        .unwrap_or_else(|error| fail(root, "explicit default-feature replay", error));
    let replay_result = json!({"exact_replay":replay.exact_replay,
        "manifest":replay.snapshot.manifest,"projection":replay.snapshot.projection});
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("default-startup-replay-result.json"))
        .unwrap()
        .write_all(&serde_json::to_vec(&replay_result).unwrap())
        .unwrap();
    assert!(replay.exact_replay);
    assert_eq!(replay.snapshot.manifest, manifest);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_bytes);
    assert_eq!(
        replay.snapshot.projection.run_generation,
        projection.run_generation
    );
    check_policy(state_root, &replay.snapshot, Access::Write, &thread);
    assert_eq!(
        read_runtime_record(&runtime_path, DarwinSystem.current_uid()).unwrap(),
        runtime
    );
    assert_eq!(
        classify_worker_identity(&runtime),
        ProcessIdentityVerdict::Match
    );
    assert_eq!(
        dolgorae::worker::classify_dedicated_server_identity(dedicated),
        ProcessIdentityVerdict::Match
    );
    assert_eq!(fs::read_dir(state_root.join("runs")).unwrap().count(), 1);
    let receipt = store
        .load_caller_start_receipt(run_id, &reservation.normalized_identity_sha256)
        .unwrap()
        .expect("replay must publish the original bound receipt");
    assert_eq!(receipt.run_id, run_id);
    assert_eq!(receipt.original_thread_id, thread);
    assert_eq!(receipt.run_generation, projection.run_generation);
    let observation = json!({"path":"recovered", "initial_error":error, "run_id":run_id,
        "thread_id":thread, "recovery":{"operation":"Recover", "census_refusal":census,
        "request_sha256":request_digest, "reservation_sha256":reservation.normalized_identity_sha256,
        "run_generation":projection.run_generation, "writer_generation":writer.writer_generation,
        "writer_state":recovered.writer.state, "policy_access":Access::Write,
        "worker_identity":runtime.identity, "dedicated_identity":dedicated,
        "manifest_sha256":dolgorae::jcs::sha256_hex(&manifest_bytes),
        "manifest_unchanged":true, "worker_identity_unchanged":true,
        "dedicated_identity_unchanged":true, "runs":1,
        "receipt":receipt,
        "replay":{"exact_replay":replay.exact_replay, "run_id":replay.snapshot.manifest.run_id,
        "thread_id":replay.snapshot.projection.thread_id}}});
    (replay, observation)
}

fn check_policy(state_root: &Path, snapshot: &RunSnapshot, access: Access, thread: &str) {
    let (current, observed) = RunSnapshot::observe(
        state_root,
        snapshot.manifest.run_id,
        snapshot.control_socket_epoch,
    )
    .unwrap();
    assert_eq!(
        current.projection.run_generation,
        snapshot.projection.run_generation
    );
    assert_eq!(current.projection.thread_id, snapshot.projection.thread_id);
    assert_eq!(snapshot.projection.thread_id.as_deref(), Some(thread));
    assert_eq!(observed.effective_policy.access, access);
    assert_eq!(
        observed.effective_policy.verification,
        PolicyVerification::Verified
    );
    assert!(snapshot.projection.active_turn_id.is_none());
    assert!(snapshot.projection.latest_turn_id.is_none());
    assert!(snapshot.projection.pending_requests.is_empty());
    if let Some(binary) = ordinary_binary() {
        let image = fs::canonicalize(binary).unwrap();
        let record = current.runtime_record.as_ref().unwrap();
        assert_eq!(record.identity.executable_path, image);
        assert_eq!(
            record.binary_sha256,
            dolgorae::jcs::sha256_hex(&fs::read(&image).unwrap())
        );
        assert_eq!(
            classify_worker_identity(record),
            ProcessIdentityVerdict::Match
        );
        assert_eq!(
            DarwinSystem.live_process_path(record.identity.pid).unwrap(),
            image
        );
    }
}

fn stop_profiles(home: &DolgoraeHome, operator: &Path) -> bool {
    // This unique fixture has no concurrent launcher. Every listening Server
    // requires a recorded binding and a durable Profile root before spawn.
    // A rejected preparation must not be repeated merely to stop nothing.
    match no_profile_prepared(home) {
        Ok(true) => return true,
        Ok(false) => {}
        Err(_) => return false,
    }
    let mut clean = true;
    let mut bindings = BTreeMap::new();
    if let Ok(workspaces) = fs::read_dir(home.root().join("workspaces")) {
        for workspace in workspaces.flatten() {
            if let Ok(runs) = fs::read_dir(workspace.path().join("runs")) {
                for run in runs.flatten() {
                    let Ok(bytes) = fs::read(run.path().join("manifest.json")) else {
                        continue;
                    };
                    let Ok(manifest) = serde_json::from_slice::<dolgorae::run::RunManifest>(&bytes)
                    else {
                        clean = false;
                        continue;
                    };
                    if let Some(binding) = manifest.global_profile_binding {
                        bindings.insert(binding.server_key.clone(), binding);
                    }
                }
            }
        }
    }
    if bindings.is_empty() {
        return false;
    }
    for binding in bindings.values() {
        if ordinary_binary().is_some() {
            let root = operator.parent().unwrap();
            let name = &binding.selected_name;
            let stop = |key: &str| {
                ordinary(
                    root,
                    &[
                        "profile".into(),
                        "server".into(),
                        "stop".into(),
                        name.clone(),
                        "--operator-file".into(),
                        operator.to_str().unwrap().into(),
                        "--confirm-server-key".into(),
                        key.into(),
                    ],
                )
            };
            let result = match stop(&binding.server_key) {
                Err(error)
                    if error.code == "PROFILE_MISMATCH"
                        && error.details["field"] == "confirm_server_key" =>
                {
                    ordinary(
                        root,
                        &[
                            "profile".into(),
                            "server".into(),
                            "status".into(),
                            name.clone(),
                        ],
                    )
                    .and_then(|current| {
                        let key = current["server_key"].as_str().unwrap();
                        ordinary(
                            root,
                            &[
                                "profile".into(),
                                "server".into(),
                                "migrate".into(),
                                name.clone(),
                                "--operator-file".into(),
                                operator.to_str().unwrap().into(),
                                "--confirm-old-server-key".into(),
                                binding.server_key.clone(),
                                "--confirm-new-server-key".into(),
                                key.into(),
                            ],
                        )?;
                        stop(key)
                    })
                }
                result => result,
            };
            if let Err(error) = result {
                record_failure(root, "ordinary Profile cleanup", &error);
                clean = false;
            }
            continue;
        }
        let result = CredentialCarrier::open_path(operator).and_then(|carrier| {
            dolgorae::profile::retire_qualified_global_server(binding, &carrier)
        });
        if let Err(error) = result {
            record_failure(operator.parent().unwrap(), "Profile cleanup", &error);
            clean = false;
        }
    }
    clean
}

fn no_profile_prepared(home: &DolgoraeHome) -> Result<bool, Box<dyn std::error::Error>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct History {
        schema_version: u32,
        bindings: BTreeMap<String, dolgorae::global_profile::ProfileBindingRecord>,
    }
    let history: History =
        serde_json::from_slice(&fs::read(home.root().join("profile-bindings.json"))?)?;
    Ok(history.schema_version == 1
        && history.bindings.is_empty()
        && !home.root().join("profiles").try_exists()?)
}

fn inject_writer_guard(state_root: &Path, workspace_id: &str, run_id: Uuid) {
    let transaction = Uuid::now_v7();
    dolgorae::writer::WriterStore::new(state_root, workspace_id, DarwinSystem.current_uid())
        .transact(|writer| {
            writer.prepare_release(run_id, transaction)?;
            writer.block_unknown(transaction);
            Ok(())
        })
        .unwrap();
}

fn observed_startup_features(socket: &Path, home: &str, thread: Option<&str>) -> Value {
    let mut rpc =
        dolgorae::app_server::JsonRpcConnection::connect(socket, Duration::from_secs(5)).unwrap();
    let initialized = rpc.request("initialize", json!({"clientInfo":{"name":"dolgorae-m6-observer","version":"1"},"capabilities":{"experimentalApi":false}})).unwrap();
    assert_eq!(initialized["codexHome"], home);
    rpc.notify("initialized", json!({})).unwrap();
    let mut cursor = Value::Null;
    let mut observed = serde_json::Map::new();
    for _ in 0..16 {
        let page = rpc
            .request(
                "experimentalFeature/list",
                json!({"cursor":cursor,"limit":100,"threadId":thread}),
            )
            .unwrap();
        for feature in page["data"].as_array().unwrap() {
            if matches!(feature["name"].as_str(), Some("shell_snapshot" | "plugins")) {
                assert_eq!(
                    feature["enabled"],
                    feature["name"] == "plugins",
                    "native effective feature disagrees with default-feature qualification"
                );
                observed.insert(
                    feature["name"].as_str().unwrap().to_owned(),
                    feature.clone(),
                );
            }
        }
        if observed.len() == 2 {
            return Value::Object(observed);
        }
        cursor = page["nextCursor"].clone();
        assert!(
            !cursor.is_null(),
            "native startup feature observation missing"
        );
    }
    panic!("native feature pagination exceeded bound");
}

fn default_feature_observation(root: &Path, snapshot: &RunSnapshot, startup: Value) -> Value {
    use sha2::{Digest as _, Sha256};
    let binding = snapshot.manifest.global_profile_binding.as_ref().unwrap();
    binding.validate_for_recovery().unwrap();
    let launch = &binding.launch_snapshot;
    let contract = json!({"schema_version":1,"canonical_codex_home":launch.canonical_codex_home,
        "normalized_argv":launch.normalized_argv,"launch_cwd_policy":launch.launch_cwd_policy,
        "launch_mode":"app_server_unix_socket_v1","sanitized_environment":launch.sanitized_environment,
        "executable_identity":launch.executable_identity,"process_static_configuration":launch.process_static_configuration,
        "codex_version":launch.codex_version,"app_server_schema_sha256":launch.schema_bundle_sha256,
        "compatibility_manifest_sha256":launch.compatibility_manifest_sha256,"enabled_features":launch.enabled_features,"disabled_features":launch.disabled_features});
    let canonical = |value: &Value| {
        dolgorae::jcs::canonicalize(&dolgorae::jcs::parse(&value.to_string()).unwrap()).unwrap()
    };
    let bytes = canonical(&contract);
    assert_eq!(
        dolgorae::jcs::sha256_hex(&bytes),
        launch.launch_contract_sha256
    );
    let mut hasher = Sha256::new();
    hasher.update(b"dolgorae-profile-server-key-v1\0");
    hasher.update(&bytes);
    assert_eq!(format!("{:x}", hasher.finalize()), binding.server_key);
    assert_eq!(
        binding.launch_snapshot_sha256,
        dolgorae::jcs::sha256_hex(&canonical(&json!(launch)))
    );
    assert_eq!(
        dolgorae::run::launch_contract_digest(&snapshot.manifest.profile).unwrap(),
        snapshot.manifest.profile.launch_contract_sha256
    );
    let home = DolgoraeHome::system().unwrap();
    let state: Value = serde_json::from_slice(
        &fs::read(
            home.root()
                .join("profiles")
                .join(&binding.server_key)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["server_key"], binding.server_key);
    assert_eq!(state["snapshot"], json!(launch));
    let pid = state["pid"].as_u64().unwrap();
    let actual = std::process::Command::new("/bin/ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .unwrap();
    assert!(actual.status.success());
    let actual = String::from_utf8(actual.stdout).unwrap();
    let tokens = actual.split_whitespace().collect::<Vec<_>>();
    let expected = launch
        .normalized_argv
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert!(
        tokens
            .windows(expected.len())
            .any(|window| window == expected),
        "actual process argv differs from immutable launch"
    );
    let shared_features = observed_startup_features(
        Path::new(state["socket_path"].as_str().unwrap()),
        &launch.canonical_codex_home,
        None,
    );
    let state_root = home.workspace_root(&snapshot.manifest.workspace_id);
    let runtime = dolgorae::worker::runtime_root(&state_root);
    let bootstrap = dolgorae::worker::read_worker_bootstrap(
        &dolgorae::worker::bootstrap_path(&runtime, snapshot.manifest.run_id),
        DarwinSystem.current_uid(),
    )
    .unwrap();
    let session = bootstrap.session.as_ref().unwrap();
    let dedicated = session.dedicated_server.as_ref().unwrap();
    assert!(dedicated.argv.starts_with(&launch.normalized_argv));
    let dedicated_features = observed_startup_features(
        &session.app_server_socket,
        &launch.canonical_codex_home,
        snapshot.projection.thread_id.as_deref(),
    );
    let mut effective = None;
    for pair in launch.normalized_argv.windows(2) {
        if pair[1] == "shell_snapshot" {
            match pair[0].as_str() {
                "--enable" => effective = Some(true),
                "--disable" => effective = Some(false),
                _ => {}
            }
        }
    }
    let evidence = json!({"variant":fs::read_to_string(root.join("default-feature-variant")).unwrap(),
        "actual_pid":pid,"actual_command":actual.trim(),"accepted_launch":launch,"recomputed_launch_digest":launch.launch_contract_sha256,"recomputed_server_key":binding.server_key,
        "run_launch_digest":snapshot.manifest.profile.launch_contract_sha256,"effective_shell_snapshot":dedicated_features["shell_snapshot"]["enabled"],"shared_feature_observation":shared_features["shell_snapshot"],"dedicated_feature_observation":dedicated_features["shell_snapshot"],"shared_plugin_observation":shared_features["plugins"],"dedicated_plugin_observation":dedicated_features["plugins"],"dedicated_argv":dedicated.argv,"quiescent_startup":startup["path"]=="first_attempt","quiescent_verified_result":true,"startup":startup});
    fs::write(
        root.join("default-feature-observation.json"),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    assert!(
        launch
            .disabled_features
            .iter()
            .any(|v| v == "shell_snapshot"),
        "default-feature policy has not disabled shell_snapshot"
    );
    assert_eq!(
        effective,
        Some(false),
        "actual final argv must disable shell_snapshot"
    );
    assert!(
        snapshot
            .manifest
            .profile
            .disabled_features
            .iter()
            .any(|v| v == "shell_snapshot")
    );
    evidence
}

fn campaign(root: &Path, codex: &Path) -> Value {
    assert_eq!(
        std::env::var("DOLGORAE_RUN_NATIVE_CALLER_PIPELINE").as_deref(),
        Ok("1")
    );
    assert_eq!(
        PathBuf::from(std::env::var_os("HOME").unwrap()),
        root.join("home")
    );
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-caller-pipeline/v1\n"
    );
    let service = CoreSemanticService;
    let view = if ordinary_binary().is_some() {
        ordinary(
            root,
            &[
                "init".into(),
                "--non-git".into(),
                root.join("home/workspace").to_str().unwrap().into(),
            ],
        )
        .unwrap();
        dolgorae::workspace::WorkspaceService::system()
            .unwrap()
            .discover(Some(&root.join("home/workspace")))
            .unwrap()
    } else {
        let SemanticResult::Workspace(view) = service
            .execute(&SemanticCommand::Initialize {
                path: Some(root.join("home/workspace")),
                mode: WorkspaceMode::NonGit,
            })
            .unwrap()
        else {
            panic!("workspace initialization result");
        };
        view
    };
    let home = DolgoraeHome::system().unwrap();
    for name in ["fixture-a", "fixture-b"] {
        let definition = RuntimeProfile {
            argv: {
                let mut argv = vec![codex.to_str().unwrap().to_owned()];
                if let Ok(variant) = fs::read_to_string(root.join("default-feature-variant")) {
                    match variant.as_str() {
                        "argv-enable" => argv.extend(["--enable".into(), "shell_snapshot".into()]),
                        "ordered-overrides" => argv.extend([
                            "--enable".into(),
                            "shell_snapshot".into(),
                            "--disable".into(),
                            "shell_snapshot".into(),
                            "--enable".into(),
                            "shell_snapshot".into(),
                        ]),
                        "default" | "config-enable" => {}
                        _ => panic!("unknown default-feature variant"),
                    }
                }
                argv
            },
            codex_home: root.join(name).to_str().unwrap().to_owned(),
            environment: BTreeMap::from([
                (
                    "PATH".to_owned(),
                    "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
                ),
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ("LC_ALL".to_owned(), "en_US.UTF-8".to_owned()),
            ]),
            native_subagents: NativeSubagents::Enabled,
        };
        if ordinary_binary().is_some() {
            let mut args = vec![
                "profile".into(),
                "add".into(),
                name.into(),
                "--codex-home".into(),
                definition.codex_home.clone(),
                "--native-subagents".into(),
                "enabled".into(),
            ];
            for (key, value) in &definition.environment {
                args.extend(["--env".into(), format!("{key}={value}")]);
            }
            args.push("--".into());
            args.extend(definition.argv);
            ordinary(root, &args).unwrap();
        } else {
            GlobalProfileStore::new(&home)
                .add(name.to_owned(), definition)
                .unwrap();
        }
    }
    let credential = if std::env::var("DOLGORAE_NATIVE_READONLY_POOL").as_deref() == Ok("1")
        || root.join("interaction-cases").exists()
        || root.join("task094-session").exists()
    {
        let directory = home.root().join("controller-carriers");
        fs::create_dir(&directory).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let client = directory.join("native-pool");
        fs::create_dir(&client).unwrap();
        fs::set_permissions(&client, fs::Permissions::from_mode(0o700)).unwrap();
        let installation = client.join("test-installation");
        fs::create_dir(&installation).unwrap();
        fs::set_permissions(&installation, fs::Permissions::from_mode(0o700)).unwrap();
        installation.join("pool.json")
    } else {
        root.join("controller.json")
    };
    if ordinary_binary().is_some() {
        let mut args = vec![
            "controller".into(),
            "credential".into(),
            "create".into(),
            "--kind".into(),
            if root.join("task094-session").exists() {
                "interactive-client"
            } else {
                "automation"
            }
            .into(),
            "--instance-id".into(),
            "native-caller-fixture".into(),
            "--output".into(),
            credential.to_str().unwrap().into(),
        ];
        if root.join("task094-session").exists() {
            args.extend(["--orchestration-policy".into(), "task094-session".into()]);
        }
        ordinary(root, &args).unwrap();
    } else {
        create_controller_credential(
            &credential,
            ControllerKind::Automation,
            "native-caller-fixture".to_owned(),
            None,
            None,
        )
        .unwrap();
    }
    let operator = root.join("operator.json");
    if ordinary_binary().is_some() {
        ordinary(
            root,
            &[
                "operator".into(),
                "credential".into(),
                "initialize".into(),
                "--output".into(),
                operator.to_str().unwrap().into(),
            ],
        )
        .unwrap();
    } else {
        OperatorStore::new(home.operator_root())
            .initialize(&operator)
            .unwrap();
    }
    let state_root = home.workspace_root(&view.workspace_id);
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let result = std::panic::catch_unwind(|| {
        if std::env::var("DOLGORAE_NATIVE_READONLY_POOL").as_deref() == Ok("1")
            || root.join("interaction-cases").exists()
            || root.join("task094-session").exists()
        {
            return pool_campaign(root, &home, &view, &credential);
        }
        let default_mode = root.join("default-feature-variant").exists();
        let (first, startup) = if default_mode {
            default_feature_start(root, &view, &credential, &state_root)
        } else {
            (
                start(&view, &credential, "fixture-a", "first", false)
                    .unwrap_or_else(|error| fail(root, "caller startup", error)),
                Value::Null,
            )
        };
        let first_id = first.snapshot.manifest.run_id;
        let first_thread = first.snapshot.projection.thread_id.clone().unwrap();
        check_policy(&state_root, &first.snapshot, Access::Write, &first_thread);
        let default_features =
            default_mode.then(|| default_feature_observation(root, &first.snapshot, startup));
        if let Some(features) = &default_features {
            return json!({"schema":"dolgorae.native-caller-pipeline/v1",
                "mode":"default_feature_startup_only","runs":1,"threads":1,"turns":0,
                "native_account_environment":"platform_owned","default_feature_startup":features});
        }
        let original_receipt = fs::read(
            state_root
                .join("runs")
                .join(first_id.to_string())
                .join("caller-start-receipt.json"),
        )
        .unwrap();
        let second = start(&view, &credential, "fixture-b", "second", true)
            .unwrap_or_else(|error| fail(root, "reader startup", error));
        let second_id = second.snapshot.manifest.run_id;
        let second_thread = second.snapshot.projection.thread_id.clone().unwrap();
        check_policy(&state_root, &second.snapshot, Access::Read, &second_thread);
        let count = fs::read_dir(state_root.join("runs")).unwrap().count();
        let busy = start(&view, &credential, "fixture-b", "competing", false)
            .err()
            .expect("competing Writer must reject");
        assert_eq!(busy.code, "WRITER_BUSY");
        assert_eq!(
            fs::read_dir(state_root.join("runs")).unwrap().count(),
            count
        );
        check_policy(
            &state_root,
            &mutation(
                &view,
                &credential,
                first_id,
                RunMutationOperation::ReleaseWriter,
                "first_writer_release",
            ),
            Access::Read,
            &first_thread,
        );
        check_policy(
            &state_root,
            &mutation(
                &view,
                &credential,
                second_id,
                RunMutationOperation::AcquireWriter,
                "second_writer_acquire",
            ),
            Access::Write,
            &second_thread,
        );
        let busy = try_mutation(
            &view,
            &credential,
            first_id,
            RunMutationOperation::AcquireWriter,
        )
        .expect_err("one Writer across Profiles");
        assert_eq!(busy.code, "WRITER_BUSY");
        check_policy(
            &state_root,
            &mutation(
                &view,
                &credential,
                second_id,
                RunMutationOperation::ReleaseWriter,
                "second_writer_release",
            ),
            Access::Read,
            &second_thread,
        );
        check_policy(
            &state_root,
            &mutation(
                &view,
                &credential,
                first_id,
                RunMutationOperation::AcquireWriter,
                "first_writer_reacquire",
            ),
            Access::Write,
            &first_thread,
        );
        fs::remove_file(home.root().join("home/AGENTS.md")).unwrap();
        fs::remove_file(home.root().join("roles/general/AGENTS.md")).unwrap();
        let replay = start(&view, &credential, "fixture-a", "first", false).unwrap();
        assert!(replay.exact_replay);
        assert_eq!(replay.snapshot.manifest.run_id, first_id);
        assert_eq!(
            fs::read(
                state_root
                    .join("runs")
                    .join(first_id.to_string())
                    .join("caller-start-receipt.json")
            )
            .unwrap(),
            original_receipt
        );
        inject_writer_guard(&state_root, &view.workspace_id, first_id);
        let live_recovered = mutation(
            &view,
            &credential,
            first_id,
            RunMutationOperation::Recover,
            "guarded_live_recovery",
        );
        check_policy(&state_root, &live_recovered, Access::Write, &first_thread);
        assert_eq!(
            live_recovered.writer.state,
            dolgorae::writer::WriterAuthorityState::Active
        );
        inject_writer_guard(&state_root, &view.workspace_id, first_id);
        let path = dolgorae::worker::runtime_record_path(
            &dolgorae::worker::runtime_root(&state_root),
            first_id,
        )
        .unwrap();
        let previous = read_runtime_record(&path, DarwinSystem.current_uid()).unwrap();
        assert_eq!(
            classify_worker_identity(&previous),
            ProcessIdentityVerdict::Match
        );
        DarwinSystem
            .signal_process_group(previous.identity.process_group_id, libc::SIGKILL)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while classify_worker_identity(&previous) != ProcessIdentityVerdict::Absent {
            let _ = DarwinSystem.reap_child_nonblocking(previous.identity.pid);
            assert!(Instant::now() < deadline, "owned Worker did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        let recovered = mutation(
            &view,
            &credential,
            first_id,
            RunMutationOperation::Recover,
            "guarded_generation_recovery",
        );
        check_policy(&state_root, &recovered, Access::Read, &first_thread);
        assert!(recovered.projection.run_generation > previous.identity.run_generation);
        assert_eq!(
            recovered.writer.state,
            dolgorae::writer::WriterAuthorityState::None
        );
        assert_eq!(
            store.load_manifest(first_id).unwrap(),
            first.snapshot.manifest
        );
        assert_eq!(
            fs::read(
                state_root
                    .join("runs")
                    .join(first_id.to_string())
                    .join("caller-start-receipt.json")
            )
            .unwrap(),
            original_receipt
        );
        json!({"schema":"dolgorae.native-caller-pipeline/v1","runs":2,"threads":2,"turns":0,
            "cross_profile_writer_exclusion":true,"same_thread_transitions":true,"original_receipt_replay":true,
            "source_removal_replay":true,"guarded_live_recovery":true,
            "guarded_generation_recovery_reader":true,"automatic_writer_reacquisition":false,
            "native_account_environment":"platform_owned","default_feature_startup":default_features})
    });
    if root.join("session-gateway-retained.json").exists() {
        fs::write(root.join("cleanup-result"), "incomplete\n").unwrap();
        result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        panic!("retained gateway requires explicit original-operation recovery");
    }
    let mut clean = true;
    for entry in fs::read_dir(state_root.join("runs")).unwrap() {
        let entry = entry.unwrap();
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            continue;
        };
        let bootstrap = root.join("session-bootstrap-controller.json");
        let cleanup_credential = if bootstrap.exists()
            && store
                .load_manifest(id)
                .unwrap()
                .controller
                .identity
                .instance_id
                == "task094-observer-bootstrap"
        {
            let binding = dolgorae::controller::binding_from_carrier(
                &CredentialCarrier::open_path(&bootstrap).unwrap(),
                1,
            )
            .unwrap();
            assert_eq!(binding, store.load_manifest(id).unwrap().controller);
            &bootstrap
        } else {
            &credential
        };
        let mut current = RunSnapshot::load(&state_root, id, 0).unwrap();
        if current.projection.lifecycle == RunLifecycle::Closed {
            continue;
        }
        let retained = current
            .manifest
            .aggregate_binding
            .as_ref()
            .filter(|binding| binding.member_kind == dolgorae::run::AggregateMemberKind::Primary)
            .and_then(|_| {
                dolgorae::orchestration::OrchestrationStore::open_observer(&state_root)
                    .unwrap()
                    .session_close(id)
                    .unwrap()
            });
        let operation = if let Some(close) = retained {
            // Cleanup continues the accepted intent; it never replaces Abort
            // with an incompatible graceful Close or reconciles unknown work.
            let deadline = Instant::now() + Duration::from_secs(90);
            while close.progress != "outcome_unknown"
                && (current.projection.active_turn_id.is_some()
                    || !current.projection.pending_requests.is_empty())
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(50));
                current = RunSnapshot::load(&state_root, id, 0).unwrap();
            }
            if close.progress == "outcome_unknown"
                || current.projection.lifecycle != RunLifecycle::Idle
                || current.projection.active_turn_id.is_some()
                || !current.projection.pending_requests.is_empty()
            {
                record_failure(
                    root,
                    "Run cleanup",
                    &dolgorae::machine::MachineError::new(
                        "RECOVERY_REQUIRED",
                        "retained Close is not ready for cleanup recovery",
                        false,
                        json!({"run_id":id,"operation_id":close.operation_id,"progress":close.progress}),
                    ),
                );
                clean = false;
                continue;
            }
            RunMutationOperation::Recover
        } else {
            RunMutationOperation::Close { interrupt: false }
        };
        let close = try_mutation(&view, cleanup_credential, id, operation);
        match close {
            Ok(snapshot) => clean &= snapshot.projection.lifecycle == RunLifecycle::Closed,
            Err(error) => {
                record_failure(root, "Run cleanup", &error);
                clean = false;
            }
        }
    }
    clean &= stop_profiles(&home, &operator);
    fs::write(
        root.join("cleanup-result"),
        if clean { "complete\n" } else { "incomplete\n" },
    )
    .unwrap();
    assert!(clean, "isolated native cleanup incomplete");
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn interaction_cases(
    root: &Path,
    view: &WorkspaceView,
    credential: &Path,
    runtime: &tokio::runtime::Runtime,
    channel: tonic::transport::Channel,
    controller: Option<dolgorae::protocol::public_v2::ControllerCarrierRef>,
    existing_run: Option<Uuid>,
) -> Value {
    use dolgorae::protocol::public_v2 as pb;
    let home = DolgoraeHome::system().unwrap();
    let snapshot = existing_run.map_or_else(
        || {
            start(view, credential, "fixture-a", "interaction-run", false)
                .unwrap_or_else(|e| fail(root, "interaction startup", e))
                .snapshot
        },
        |id| RunSnapshot::load(&home.workspace_root(&view.workspace_id), id, 0).unwrap(),
    );
    let id = snapshot.manifest.run_id;
    let run = Some(pb::RunRef {
        workspace: Some(pb::WorkspaceRef {
            absolute_path: view
                .canonical_path
                .to_path_buf()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            expected_workspace_id: view.workspace_id.clone(),
        }),
        run_id: id.to_string(),
    });
    let context = || {
        Some(pb::RequestContext {
            protocol_version: 2,
            client_request_id: Uuid::now_v7().to_string(),
            client_instance_id: "task079-native".into(),
        })
    };
    let native_socket = dolgorae::worker::dedicated_server_socket_path(
        dolgorae::workspace::WorkspacePlatform::current_uid(&SystemWorkspacePlatform),
        id,
        snapshot.projection.run_generation,
    )
    .unwrap();
    let mut native =
        dolgorae::app_server::JsonRpcConnection::connect(&native_socket, Duration::from_secs(10))
            .unwrap();
    native.request("initialize",json!({"clientInfo":{"name":"task079-inventory","version":"1"},"capabilities":{"experimentalApi":true}})).unwrap();
    native.notify("initialized", json!({})).unwrap();
    let inventory=native.request("mcpServerStatus/list",json!({"threadId":snapshot.projection.thread_id,"serverName":"fixture","detail":"toolsAndAuthOnly"})).unwrap();
    assert!(
        inventory["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "fixture" && v["tools"].get("ask").is_some())
    );
    drop(native);
    let identity = &snapshot.controller_authority().unwrap().identity;
    let principal_value = json!({"controller_id":identity.controller_id,"kind":identity.kind,"instance_id":identity.instance_id,"subject_id":identity.subject_id});
    let principal = format!(
        "sha256:{}",
        dolgorae::jcs::sha256_hex(
            &dolgorae::jcs::canonicalize(
                &dolgorae::jcs::parse(&principal_value.to_string()).unwrap()
            )
            .unwrap()
        )
    );
    runtime.block_on(async {
        let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
        let mut operations =
            pb::operation_service_client::OperationServiceClient::new(channel.clone());
        let mut interactions =
            pb::interaction_service_client::InteractionServiceClient::new(channel);
        let selected = fs::read_to_string(root.join(if existing_run.is_some() {
            "task094-session"
        } else {
            "interaction-cases"
        }))
        .unwrap();
        let selected = selected.trim().split(',').collect::<Vec<_>>();
        for case in selected.iter().copied() {
            let current = runs
                .get_run(pb::GetRunRequest {
                    context: context(),
                    run: run.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .run
                .unwrap();
            runs.submit_turn(pb::SubmitTurnRequest {
                context: context(),
                run: run.clone(),
                controller: controller.clone(),
                idempotency_key: case.into(),
                write_intent: pb::WriteIntent::Write as i32,
                message: format!(
                    "{} {case}",
                    if existing_run.is_some() {
                        "TASK094"
                    } else {
                        "TASK079"
                    }
                ),
                expected_state_revision: current.state_revision,
                ..Default::default()
            })
            .await
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(90);
            let summary = loop {
                let page = interactions
                    .list_pending_interactions(pb::ListPendingInteractionsRequest {
                        context: context(),
                        run: run.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner();
                if let Some(item) = page.items.into_iter().next() {
                    break item;
                }
                assert!(
                    Instant::now() < deadline,
                    "{case} did not open; fixture retained at {}",
                    root.display()
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            assert_eq!(
                summary.kind,
                if case == "permission" {
                    pb::InteractionKind::PermissionRequest
                } else {
                    pb::InteractionKind::McpElicitation
                } as i32
            );
            assert!(summary.contains_protected_input);
            let mut detail = interactions
                .get_controller_interaction(pb::GetControllerInteractionRequest {
                    context: context(),
                    run: run.clone(),
                    controller: controller.clone(),
                    interaction_id: summary.interaction_id.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .interaction
                .unwrap();
            let answer = match detail.payload.clone().unwrap() {
                pb::controller_interaction::Payload::Permission(p) => {
                    assert!(!p.requested_permissions.is_empty());
                    pb::InteractionAnswer {
                        decision: 1,
                        granted_permissions: p.requested_permissions,
                        ..Default::default()
                    }
                }
                pb::controller_interaction::Payload::McpElicitation(p) => {
                    assert_eq!(p.questions.len(), 1);
                    pb::InteractionAnswer {
                        decision: 1,
                        answers: vec![pb::QuestionAnswer {
                            question_id: p.questions[0].id.clone(),
                            selected_labels: vec![],
                            text: Some("task079-protected-answer-canary".into()),
                        }],
                        ..Default::default()
                    }
                }
                _ => panic!("wrong typed payload"),
            };
            let current = loop {
                let current = runs
                    .get_run(pb::GetRunRequest {
                        context: context(),
                        run: run.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .run
                    .unwrap();
                let fresh = interactions
                    .get_controller_interaction(pb::GetControllerInteractionRequest {
                        context: context(),
                        run: run.clone(),
                        controller: controller.clone(),
                        interaction_id: summary.interaction_id.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .interaction
                    .unwrap();
                assert_eq!(fresh.payload, detail.payload);
                let coherent = current.lifecycle == pb::RunLifecycle::WaitingInteraction as i32
                    && fresh.stamp.as_ref().unwrap().run_state_revision == current.state_revision;
                detail = fresh;
                if coherent {
                    break current;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let thread = current.thread.unwrap();
            let request = pb::ResolveInteractionRequest {
                context: context(),
                run: run.clone(),
                controller: controller.clone(),
                interaction_id: summary.interaction_id.clone(),
                idempotency_key: format!("{case}-answer"),
                answer: Some(answer),
                expected_interaction_revision: detail.stamp.unwrap().interaction_state_revision,
                expected_run_revision: current.state_revision,
                expected_turn_id: current.active_turn.unwrap().turn_id,
                expected_thread_generation: thread.thread_generation,
            };
            let receipt = interactions
                .resolve_interaction(request.clone())
                .await
                .unwrap()
                .into_inner();
            let mut replay = request;
            replay.answer = Some(pb::InteractionAnswer {
                protected_input: Some(pb::ProtectedInputRef {
                    absolute_file_path: "/not/read/on/receipt/replay".into(),
                    maximum_bytes: 1,
                }),
                ..Default::default()
            });
            let replay = interactions
                .resolve_interaction(replay)
                .await
                .unwrap()
                .into_inner();
            assert_eq!(receipt.resolution_receipt, replay.resolution_receipt);
            let operation = pb::OperationRef {
                caller_scope: Some(pb::CallerScope {
                    controller_kind: dolgorae::gateway_projection_v2::controller_kind(identity.kind)
                        as i32,
                    principal_ref: principal.clone(),
                    controller_generation: identity.generation,
                    workspace_id: view.workspace_id.clone(),
                    aggregate: Some(pb::caller_scope::Aggregate::RunId(id.to_string())),
                }),
                operation_kind: pb::OperationKind::ResolveInteraction as i32,
                idempotency_key: format!("{case}-answer"),
            };
            let lookup = operations
                .lookup_operation(pb::LookupOperationRequest {
                    context: context(),
                    controller: controller.clone(),
                    operation: Some(operation.clone()),
                })
                .await
                .unwrap()
                .into_inner();
            let Some(pb::lookup_operation_response::Result::Accepted(accepted)) = lookup.result
            else {
                panic!("Resolve lookup lost authentic acceptance");
            };
            let accepted = accepted.receipt.unwrap();
            assert_eq!(accepted.operation, Some(operation));
            assert_eq!(accepted.receipt_id, receipt.resolution_receipt);
            assert!(accepted.accepted_input_digest.starts_with("sha256:"));
            let subjects = accepted.subjects.unwrap();
            assert_eq!(subjects.run_id, Some(id.to_string()));
            assert_eq!(subjects.interaction_id, Some(summary.interaction_id));
            loop {
                let current = runs
                    .get_run(pb::GetRunRequest {
                        context: context(),
                        run: run.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .run
                    .unwrap();
                if current.lifecycle == pb::RunLifecycle::Idle as i32 {
                    break;
                }
                assert!(Instant::now() < deadline, "native terminal missing");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    });
    let state = home
        .workspace_root(&view.workspace_id)
        .join("runs")
        .join(id.to_string());
    for entry in fs::read_dir(&state).unwrap().flatten() {
        if entry.file_type().unwrap().is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            assert!(
                !bytes
                    .windows(b"task079-protected-answer-canary".len())
                    .any(|w| w == b"task079-protected-answer-canary")
            );
        }
    }
    let cases = fs::read_to_string(root.join(if existing_run.is_some() {
        "task094-session"
    } else {
        "interaction-cases"
    }))
    .unwrap();
    let permission = cases.trim().split(',').any(|v| v == "permission");
    let mcp = cases.trim().split(',').any(|v| v == "mcp");
    json!({"schema":"dolgorae.native-caller-pipeline/v1","mode":"interactions","public_v2_uds":true,"permission_accept":permission,"mcp_accept":mcp,"protected_receipt_replay":true,"native_turns":cases.trim().split(',').count(),"sequential_write_admissions_verified":permission&&mcp,"pre_dispatch_admission_settled":permission&&mcp})
}

fn pool_campaign(
    root: &Path,
    home: &DolgoraeHome,
    view: &WorkspaceView,
    credential: &Path,
) -> Value {
    use dolgorae::protocol::public_v2 as pb;
    use dolgorae::provisioning::{
        ProvisioningAdmission, ProvisioningSources, capture_provisioning,
    };
    if root.join("task094-session").exists() {
        return session::campaign(root, home, view, credential);
    }
    let pool_limit = fs::read_to_string(root.join("pool-limit"))
        .unwrap_or_else(|_| "1".into())
        .parse::<u32>()
        .unwrap();
    assert!([1, 4].contains(&pool_limit));
    let config = dolgorae::pool::PoolConfiguration {
        readonly_server_limit: pool_limit,
        ..Default::default()
    };
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(home.root().join("pool-configuration.json"))
        .unwrap()
        .write_all(&serde_json::to_vec(&config).unwrap())
        .unwrap();
    let instruct = dolgorae::instructions::InstructionSourceStore::new(home);
    if ordinary_binary().is_none() {
        instruct.initialize_templates().unwrap();
    }
    let carrier = CredentialCarrier::open_path(credential).unwrap();
    let identity = dolgorae::controller::binding_from_carrier(&carrier, 1)
        .unwrap()
        .identity;
    let controller = Some(pb::ControllerCarrierRef {
        absolute_file_path: credential.to_str().unwrap().into(),
        expected_controller_id: identity.controller_id.to_string(),
        expected_controller_generation: 1,
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let socket_path = root.join("gateway.sock");
    struct GatewayGuard {
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
        child: Option<std::process::Child>,
    }
    impl Drop for GatewayGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                assert!(
                    std::process::Command::new("/bin/kill")
                        .arg("-TERM")
                        .arg(child.id().to_string())
                        .status()
                        .unwrap()
                        .success()
                );
                child.wait().unwrap();
            } else {
                let _ = self.stop.take().unwrap().send(());
                self.thread.take().unwrap().join().unwrap();
            }
        }
    }
    let _gateway = if let Some(binary) = ordinary_binary() {
        use std::io::BufRead as _;
        let stderr = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("ordinary-serve.stderr"))
            .unwrap();
        let mut child = std::process::Command::new(binary)
            .arg("serve")
            .arg("--socket")
            .arg(&socket_path)
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        fs::write(root.join("ordinary-serve-ready.json"), &line).unwrap();
        let ready: dolgorae::machine::SuccessEnvelope = serde_json::from_str(&line).unwrap();
        assert!(ready.ok);
        assert_eq!(ready.command, "serve");
        assert_eq!(ready.data["socket_path"], socket_path.to_str().unwrap());
        GatewayGuard {
            stop: None,
            thread: None,
            child: Some(child),
        }
    } else {
        let mut socket = dolgorae::gateway_socket::GatewaySocket::bind(home, &socket_path).unwrap();
        let backend = std::sync::Arc::new(dolgorae::gateway_service::CoreGatewayBackend::new(
            home.clone(),
            socket.record().server_instance_id,
        ));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        GatewayGuard {
            stop: Some(stop),
            child: None,
            thread: Some(std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime
                    .block_on(dolgorae::gateway::run_server(&mut socket, backend, async {
                        let _ = stopped.await;
                    }))
                    .unwrap();
                socket.cleanup().unwrap();
            })),
        }
    };
    let channel = runtime
        .block_on(
            tonic::transport::Endpoint::try_from("http://[::]:50051")
                .unwrap()
                .connect_with_connector(tower::service_fn(move |_| {
                    let path = socket_path.clone();
                    async move {
                        tokio::net::UnixStream::connect(path)
                            .await
                            .map(hyper_util::rt::TokioIo::new)
                    }
                })),
        )
        .unwrap();
    if root.join("interaction-cases").exists() {
        return interaction_cases(root, view, credential, &runtime, channel, controller, None);
    }
    let mut transport = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut runtime_client = pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
    let mut operations = pb::operation_service_client::OperationServiceClient::new(channel.clone());
    let mut legacy_writers =
        dolgorae::protocol::public_v1::writer_service_client::WriterServiceClient::new(
            channel.clone(),
        );
    let public = serde_json::json!({"controller_id":identity.controller_id,"kind":identity.kind,"instance_id":identity.instance_id,"subject_id":identity.subject_id});
    let principal = format!(
        "sha256:{}",
        dolgorae::jcs::sha256_hex(
            &dolgorae::jcs::canonicalize(&dolgorae::jcs::parse(&public.to_string()).unwrap())
                .unwrap()
        )
    );
    let original_operation =
        |kind: pb::OperationKind, key: &str, run: Option<String>| pb::OperationRef {
            caller_scope: Some(pb::CallerScope {
                controller_kind: pb::ControllerKind::Automation as i32,
                principal_ref: principal.clone(),
                controller_generation: 1,
                workspace_id: view.workspace_id.clone(),
                aggregate: Some(run.map_or_else(
                    || pb::caller_scope::Aggregate::ProvisioningScope("run-start".into()),
                    pb::caller_scope::Aggregate::RunId,
                )),
            }),
            operation_kind: kind as i32,
            idempotency_key: key.into(),
        };

    let context = || {
        Some(pb::RequestContext {
            protocol_version: 2,
            client_request_id: Uuid::now_v7().to_string(),
            client_instance_id: "native-pool".into(),
        })
    };
    let capabilities = runtime
        .block_on(runtime_client.get_capabilities(pb::GetCapabilitiesRequest {
            context: context(),
            minimum_protocol_version: 2,
            maximum_protocol_version: 2,
        }))
        .unwrap()
        .into_inner();
    assert_eq!(capabilities.context.unwrap().protocol_version, 2);
    assert_eq!(
        capabilities.descriptor_sha256,
        dolgorae::jcs::sha256_hex(dolgorae::protocol::PUBLIC_V2_DESCRIPTOR)
    );
    let mut ids = Vec::new();
    let mut threads = std::collections::BTreeSet::new();
    let mut servers = std::collections::BTreeSet::new();
    let mut compatibility = Vec::new();
    let mut first_snapshot = None;
    let mut dedicated_request = None;
    for (profile, count) in [("fixture-a", 16), ("fixture-b", 1)] {
        let binding = dolgorae::global_runtime::ResolvedGlobalProfile::resolve(home, profile)
            .unwrap()
            .prepare_specialist(home, false)
            .unwrap();
        let captured = capture_provisioning(&ProvisioningSources {
            instruct: &instruct,
            role_name: "pool-reader",
            role: dolgorae::instructions::SelectedInstructionSource::Inline(
                "POOL_EXPLICIT_ROLE: inspect the caller workspace without modifications.\n".into(),
            ),
            dependencies: vec![],
            selected_guidance: vec![],
            profile_binding: binding.clone(),
            admission: ProvisioningAdmission {
                model: "gpt-5.6-luna".into(),
                effort: "low".into(),
                purpose_kind: "planning".into(),
                purpose_label: None,
                required_capabilities: vec![],
                execution_lane: "shared_readonly".into(),
                requested_access: "read_only".into(),
                required_assurance: "best_effort_personal_alpha".into(),
                native_subagent_policy: "disabled".into(),
            },
        })
        .unwrap();
        compatibility.push(json!({"profile":profile,"codex_home":binding.launch_snapshot.canonical_codex_home,"server_key":binding.server_key,"process_static_configuration":binding.launch_snapshot.process_static_configuration,"native_subagent_policy":captured.admitted.native_subagent_policy,"normalized_argv":binding.launch_snapshot.normalized_argv}));
        let (instructions, _) = captured.admitted_configuration().unwrap();
        for member in 0..count {
            let request = pb::StartRunRequest {
                context: context(),
                workspace: workspace.clone(),
                controller: controller.clone(),
                idempotency_key: format!("{profile}-pool-{member}"),
                provisioning: Some(pb::ProvisioningSelection {
                    profile_name: profile.into(),
                    role_name: "pool-reader".into(),
                    policy_name: String::new(),
                    policy_snapshot_digest: String::new(),
                    configuration: Some(
                        dolgorae::gateway_projection_v2::admitted_configuration(&captured.admitted)
                            .unwrap(),
                    ),
                    agent_configuration_digest: captured.admitted.digest().unwrap(),
                }),
                requested_mode: pb::CreationMode::ReadOnly as i32,
                execution_lane: pb::ExecutionLane::SharedReadonly as i32,
                purpose: pb::PurposeKind::Planning as i32,
                required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
                instructions: Some(dolgorae::gateway_projection_v2::captured_instructions(
                    &instructions,
                )),
                ..Default::default()
            };
            if profile == "fixture-a" && member == 0 {
                let mut configuration = captured.admitted.clone();
                configuration.execution_lane = "dedicated".into();
                let mut dedicated = request.clone();
                dedicated.execution_lane = pb::ExecutionLane::Dedicated as i32;
                let selection = dedicated.provisioning.as_mut().unwrap();
                selection.configuration = Some(
                    dolgorae::gateway_projection_v2::admitted_configuration(&configuration)
                        .unwrap(),
                );
                selection.agent_configuration_digest = configuration.digest().unwrap();
                dedicated_request = Some(dedicated);
            }
            let original =
                original_operation(pb::OperationKind::StartRun, &request.idempotency_key, None);
            let response = runtime
                .block_on(transport.start_run(tonic::Request::new(request.clone())))
                .unwrap();
            let run = if profile == "fixture-a" && member == 0 {
                drop(response);
                let recovered = runtime
                    .block_on(operations.lookup_operation(pb::LookupOperationRequest {
                        context: context(),
                        controller: controller.clone(),
                        operation: Some(original.clone()),
                    }))
                    .unwrap()
                    .into_inner();
                let Some(pb::lookup_operation_response::Result::Accepted(recovered)) =
                    recovered.result
                else {
                    panic!("lost Start acknowledgement was not found");
                };
                let receipt = recovered.receipt.unwrap();
                assert_eq!(receipt.operation, Some(original));
                let id = receipt.subjects.unwrap().run_id.unwrap();
                runtime
                    .block_on(transport.get_run(pb::GetRunRequest {
                        context: context(),
                        run: Some(pb::RunRef {
                            workspace: workspace.clone(),
                            run_id: id,
                        }),
                    }))
                    .unwrap()
                    .into_inner()
                    .run
                    .unwrap()
            } else {
                response.into_inner().run.unwrap()
            };
            let id = Uuid::parse_str(&run.run_id).unwrap();
            assert_eq!(run.execution_lane, pb::ExecutionLane::SharedReadonly as i32);
            assert_eq!(run.requested_mode, pb::CreationMode::ReadOnly as i32);
            assert_eq!(
                run.effective_policy.as_ref().unwrap().access,
                pb::EffectiveAccess::Read as i32
            );
            assert_eq!(
                run.effective_policy.as_ref().unwrap().verification,
                pb::PolicyVerification::Verified as i32
            );
            assert!(threads.insert(run.thread.as_ref().unwrap().thread_id.clone()));
            ids.push(id);
            let state_root = home.workspace_root(&view.workspace_id);
            let snapshot = RunSnapshot::load(&state_root, id, 0).unwrap();
            let bootstrap = dolgorae::worker::read_worker_bootstrap(
                &dolgorae::worker::bootstrap_path(&dolgorae::worker::runtime_root(&state_root), id),
                DarwinSystem.current_uid(),
            )
            .unwrap();
            assert!(
                bootstrap
                    .session
                    .as_ref()
                    .unwrap()
                    .dedicated_server
                    .is_none()
            );
            let state: Value = serde_json::from_slice(
                &fs::read(
                    home.root()
                        .join("profiles")
                        .join(&binding.server_key)
                        .join("state.json"),
                )
                .unwrap(),
            )
            .unwrap();
            servers.insert((
                profile.to_owned(),
                state["pid"].as_u64().unwrap(),
                binding.server_key.clone(),
            ));
            let denial = try_mutation(view, credential, id, RunMutationOperation::AcquireWriter)
                .err()
                .unwrap();
            assert_eq!(denial.code, "SHARED_RUN_WRITE_FORBIDDEN");
            if first_snapshot.is_none() {
                first_snapshot = Some(snapshot);
            }
            let replay = runtime
                .block_on(transport.start_run(tonic::Request::new(request)))
                .unwrap()
                .into_inner();
            assert!(replay.exact_replay);
            assert_eq!(replay.run.unwrap().run_id, id.to_string());
        }
    }
    assert_eq!(
        servers.iter().filter(|(p, _, _)| p == "fixture-a").count(),
        1
    );
    assert_eq!(servers.len(), 2);
    let command = fs::read_to_string(root.join("pool-command")).unwrap();
    let first = first_snapshot.as_ref().unwrap();
    let accepted = runtime
        .block_on(transport.submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: Some(pb::RunRef {
                workspace: workspace.clone(),
                run_id: ids[0].to_string(),
            }),
            controller: controller.clone(),
            idempotency_key: "native-readonly-command".into(),
            write_intent: pb::WriteIntent::Read as i32,
            message: format!("Run exactly this command in the working directory: {command}\n"),
            images: vec![],
            effort: None,
            expected_state_revision: first.stamp.run_state_revision,
        }))
        .unwrap()
        .into_inner();
    let accepted_turn = accepted.accepted_turn.unwrap();
    if ordinary_binary().is_some() {
        ordinary(
            root,
            &[
                "run".into(),
                "wait".into(),
                ids[0].to_string(),
                accepted_turn.turn_id.clone(),
                "--workspace".into(),
                view.canonical_path
                    .to_path_buf()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
            ],
        )
        .unwrap();
    } else {
        CoreSemanticService
            .execute(&SemanticCommand::Run {
                verb: dolgorae::semantic::RunVerb::Wait,
                args: vec![
                    ids[0].to_string().into(),
                    accepted_turn.turn_id.into(),
                    "--workspace".into(),
                    view.canonical_path.to_path_buf().unwrap().into_os_string(),
                ],
            })
            .unwrap();
    }
    assert!(!root.join("home/workspace/pool-write-denied").exists());
    let original = original_operation(
        pb::OperationKind::SubmitTurn,
        "native-readonly-command",
        Some(ids[0].to_string()),
    );
    let collected = runtime
        .block_on(operations.lookup_operation(pb::LookupOperationRequest {
            context: context(),
            controller: controller.clone(),
            operation: Some(original.clone()),
        }))
        .unwrap()
        .into_inner();
    let Some(pb::lookup_operation_response::Result::Accepted(collected)) = collected.result else {
        panic!("Submit lookup was not accepted");
    };
    assert_eq!(collected.receipt.unwrap().operation, Some(original));
    assert_eq!(
        collected.current_outcome,
        pb::OperationOutcome::Completed as i32
    );
    let unknown = runtime
        .block_on(operations.lookup_operation(pb::LookupOperationRequest {
            context: context(),
            controller: controller.clone(),
            operation: Some(original_operation(
                pb::OperationKind::StartRun,
                "unknown-original-start",
                None,
            )),
        }))
        .unwrap()
        .into_inner();
    assert!(matches!(
        unknown.result,
        Some(pb::lookup_operation_response::Result::Unresolved(_))
    ));

    let mut dedicated = dedicated_request.unwrap();
    for index in 0..6 {
        dedicated.idempotency_key = format!("dedicated-{index}");
        dedicated.context = context();
        let response = runtime
            .block_on(transport.start_run(tonic::Request::new(dedicated.clone())))
            .unwrap()
            .into_inner();
        let run = response.run.unwrap();
        let id = Uuid::parse_str(&run.run_id).unwrap();
        ids.push(id);
        if index == 0 {
            let thread = run.thread.unwrap().thread_id;
            let writer = mutation(
                view,
                credential,
                id,
                RunMutationOperation::AcquireWriter,
                "dedicated acquire",
            );
            assert_eq!(
                writer.projection.thread_id.as_deref(),
                Some(thread.as_str())
            );
            let observed = runtime
                .block_on(
                    legacy_writers.get_workspace_writer_status(
                        dolgorae::protocol::public_v1::GetWorkspaceWriterStatusRequest {
                            context: Some(dolgorae::protocol::public_v1::RequestContext {
                                protocol_version: 1,
                                client_request_id: Uuid::now_v7().to_string(),
                                client_instance_id: "mixed-native-writer".into(),
                            }),
                            workspace: Some(dolgorae::protocol::public_v1::WorkspaceRef {
                                absolute_path: view
                                    .canonical_path
                                    .to_path_buf()
                                    .unwrap()
                                    .to_string_lossy()
                                    .into(),
                                expected_workspace_id: view.workspace_id.clone(),
                            }),
                        },
                    ),
                )
                .unwrap()
                .into_inner()
                .writer
                .unwrap();
            assert_eq!(observed.owner_run_id, Some(id.to_string()));
            let reader = mutation(
                view,
                credential,
                id,
                RunMutationOperation::ReleaseWriter,
                "dedicated release",
            );
            assert_eq!(
                reader.projection.thread_id.as_deref(),
                Some(thread.as_str())
            );
        }
    }
    let config = dolgorae::pool::PoolConfiguration {
        readonly_server_limit: pool_limit,
        dedicated_generation_cap: 1,
        ..Default::default()
    };
    fs::write(
        home.root().join("pool-configuration.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let replay = runtime
        .block_on(transport.start_run(tonic::Request::new(dedicated.clone())))
        .unwrap()
        .into_inner();
    assert!(replay.exact_replay);
    let accepted_request = dedicated.clone();
    dedicated.idempotency_key = "over-capacity".into();
    dedicated.context = context();
    let refused = runtime
        .block_on(transport.start_run(tonic::Request::new(dedicated)))
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::ResourceExhausted);
    let diagnostics = runtime
        .block_on(
            runtime_client.list_profile_diagnostics(pb::ListProfileDiagnosticsRequest {
                context: context(),
                profile_name: "fixture-a".into(),
                after_cursor: String::new(),
                limit: 100,
                projection: pb::ProjectionProfile::Minimal as i32,
            }),
        )
        .unwrap()
        .into_inner();
    assert!(
        diagnostics
            .items
            .iter()
            .any(|item| item.category == "sixth_dedicated_generation")
    );
    let config_path = root.join("fixture-a/config.toml");
    let original_config = fs::read(&config_path).unwrap();
    let mut upgraded = b"model_context_window = 32768\n".to_vec();
    upgraded.extend_from_slice(&original_config);
    fs::write(&config_path, upgraded).unwrap();
    let replay = runtime.block_on(transport.start_run(tonic::Request::new(accepted_request)));
    fs::write(&config_path, &original_config).unwrap();
    assert!(replay.unwrap().into_inner().exact_replay);
    let first = first_snapshot.unwrap();
    let closed = mutation(
        view,
        credential,
        ids[0],
        RunMutationOperation::Close { interrupt: false },
        "pool member close",
    );
    assert_eq!(closed.projection.lifecycle, RunLifecycle::Closed);
    let sibling = RunSnapshot::load(&home.workspace_root(&view.workspace_id), ids[1], 0).unwrap();
    let record = sibling.runtime_record.as_ref().unwrap();
    assert_eq!(
        classify_worker_identity(record),
        ProcessIdentityVerdict::Match
    );
    assert!(root.join("home/workspace").exists());
    json!({"schema":"dolgorae.native-caller-pipeline/v1","mode":"readonly_pool","members":16,"configured_k":pool_limit,"fixture_a_servers":1,"independent_account_profile_servers":1,"total_threads":threads.len(),"server_measurement":servers,"compatibility_measurements":compatibility,"same_key_replay":true,"pooled_writer_denials":17,"member_close_preserved_sibling":true,"accepted_native_policy":first.manifest.agent_configuration.native_subagent_policy,"dedicated_generations":6,"sixth_generation_warning":true,"lowered_cap_refusal":true,"same_thread_dedicated_transition":true,"profile_upgrade_original_key_replay":true,"public_v1_writer_observed_modern_holder":true,"public_v2_transport":"private_uds_generated_client","lost_start_ack_lookup_without_resubmit":true,"submit_original_key_lookup":true,"unfenced_start_absence_remains_unresolved":true,"native_model_turns":1})
}

fn main() {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.len() == 4 && args[1] == "__worker" && args[2] == "--bootstrap" {
        if let Err(error) = dolgorae::worker::run_hidden_worker(Path::new(&args[3])) {
            let code = error.code();
            let _ = dolgorae::worker::write_startup_handoff(
                &dolgorae::worker::StartupHandoff::Failed {
                    code: code.to_owned(),
                },
            );
            std::process::exit(dolgorae::machine::exit_status_for(code).into());
        }
    } else if args.len() == 4
        && args[1] == dolgorae::profile::PROFILE_LOG_DRAINER_COMMAND
        && args[2] == "--root"
    {
        dolgorae::profile::run_log_drainer(Path::new(&args[3])).unwrap();
    } else if args.len() == 3
        && args[1] == "--retire-profiles"
        && std::env::var("DOLGORAE_RUN_NATIVE_CALLER_PIPELINE").as_deref() == Ok("1")
    {
        let root = Path::new(&args[2]);
        assert_eq!(
            fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
            root.join("home")
        );
        assert_eq!(
            fs::read(root.join("fixture-owned")).unwrap(),
            b"native-integrated-acceptance/v1\n"
        );
        let clean = stop_profiles(&DolgoraeHome::system().unwrap(), &root.join("operator"));
        println!("{}", json!({"retained_profiles_retired":clean}));
        assert!(clean, "accepted Profile cleanup incomplete");
    } else if args.len() == 3
        && std::env::var("DOLGORAE_RUN_NATIVE_CALLER_PIPELINE").as_deref() == Ok("1")
    {
        println!("{}", campaign(Path::new(&args[1]), Path::new(&args[2])));
    } else {
        eprintln!("native caller pipeline requires explicit opt-in and isolated root");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn only_the_named_uncertain_census_failure_can_enter_default_recovery() {
        let run = Uuid::now_v7();
        let error = dolgorae::machine::MachineError::new(
            "TRANSPORT_FAILURE",
            "private worker transport failed",
            false,
            json!({"stage":"correlate","acceptance":"uncertain","request_id":null,"run_id":run}),
        );
        assert_eq!(uncertain_start_run(&error), Some(run));
        for (key, value) in [
            ("stage", json!("connect")),
            ("acceptance", json!("not_dispatched")),
            ("request_id", json!("foreign")),
            ("run_id", json!("unknown")),
        ] {
            let mut changed = error.clone();
            changed.details[key] = value;
            assert!(uncertain_start_run(&changed).is_none());
        }
        let mut changed = error.clone();
        changed.retryable = true;
        assert!(uncertain_start_run(&changed).is_none());
        changed = error.clone();
        changed.code = "RUN_STATE_CONFLICT".to_owned();
        assert!(uncertain_start_run(&changed).is_none());
        changed = error;
        changed
            .details
            .as_object_mut()
            .unwrap()
            .remove("request_id");
        changed.details["foreign"] = Value::Null;
        assert!(uncertain_start_run(&changed).is_none());
        let refusal =
            json!({"schema":"dolgorae.writer-census-failure/v1","error":"InvalidIdentity",
            "details":{"predicate":"dedicated_census_membership"}})
            .to_string();
        assert!(startup_census_refusal(&format!("native log\n{refusal}\n")).is_some());
        assert!(startup_census_refusal(&format!("{refusal}\n{refusal}\n")).is_none());
        assert!(
            startup_census_refusal(
                &refusal.replace("dedicated_census_membership", "dedicated_identity")
            )
            .is_none()
        );
        assert!(startup_census_refusal("unknown\n").is_none());
    }

    #[test]
    fn oversized_failure_still_retains_a_bounded_private_diagnostic() {
        let root = std::env::temp_dir().join(format!("native-diagnostic-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let error = dolgorae::machine::MachineError::new(
            "INTERNAL_ERROR",
            "fixture",
            false,
            json!({"private":"x".repeat(70_000)}),
        );
        record_failure(&root, "oversized", &error);
        let path = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.len() < 65_536);
        let record: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record["error_omitted"]["code"], "INTERNAL_ERROR");
        assert!(record["error_omitted"]["bytes"].as_u64().unwrap() > 65_536);
        assert_eq!(
            record["error_omitted"]["sha256"].as_str().unwrap().len(),
            64
        );
        assert!(!record.to_string().contains(&"x".repeat(128)));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_preparation_cleanup_requires_empty_history_and_absent_profile_root() {
        let root = std::env::temp_dir().join(format!("native-cleanup-{}", Uuid::now_v7()));
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        fs::create_dir_all(home.root()).unwrap();
        let history = home.root().join("profile-bindings.json");
        fs::write(&history, br#"{"schema_version":1,"bindings":{}}"#).unwrap();
        assert!(no_profile_prepared(&home).unwrap());
        // Neither branch may invoke Codex just to rediscover a failed preparation.
        assert!(stop_profiles(&home, &root.join("unused-operator")));
        fs::create_dir(home.root().join("profiles")).unwrap();
        assert!(!no_profile_prepared(&home).unwrap());
        fs::remove_dir(home.root().join("profiles")).unwrap();
        let key = "a".repeat(64);
        fs::write(
            &history,
            serde_json::to_vec(&json!({
                "schema_version":1,"bindings":{format!("fixture-a:{key}"):{
                    "selected_name":"fixture-a","definition_sha256":"b".repeat(64),
                    "server_key":key,"launch_snapshot_sha256":"c".repeat(64),
                }},
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(!no_profile_prepared(&home).unwrap());
        fs::write(
            &history,
            br#"{"schema_version":1,"bindings":{},"unknown":true}"#,
        )
        .unwrap();
        assert!(no_profile_prepared(&home).is_err());
        assert!(!stop_profiles(&home, &root.join("unused-operator")));
        fs::remove_file(&history).unwrap();
        assert!(no_profile_prepared(&home).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_rejection_retains_private_named_check_without_printing_details() {
        let root = std::env::temp_dir().join(format!("native-diagnostic-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let error = dolgorae::machine::MachineError::new(
            "COMPATIBILITY_REJECTED",
            "Codex compatibility check failed",
            false,
            json!({"profile":"fixture-a","check":"profile_configuration_classification",
                   "expected":"closed classification","actual":"private-canary"}),
        );
        let panic =
            std::panic::catch_unwind(|| fail(&root, "caller startup", error.clone())).unwrap_err();
        assert_eq!(
            panic.downcast_ref::<String>().unwrap(),
            "caller startup failed: COMPATIBILITY_REJECTED"
        );
        let path = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let diagnostic: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(diagnostic["stage"], "caller startup");
        assert_eq!(diagnostic["error"], serde_json::to_value(error).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
