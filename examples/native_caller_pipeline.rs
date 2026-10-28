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

fn record_failure(root: &Path, stage: &str, error: &dolgorae::machine::MachineError) {
    let bytes = serde_json::to_vec(&json!({
        "schema": "dolgorae.native-caller-pipeline-diagnostic/v1",
        "stage": stage,
        "error": error,
    }))
    .unwrap();
    if bytes.len() <= 65_536 {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(format!("failure-{}.json", Uuid::now_v7())))
            .and_then(|mut file| file.write_all(&bytes))
            .expect("private failure diagnostic could not be retained");
    }
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
    CoreSemanticService
        .mutate_caller_run(RunMutationInput {
            workspace: view.canonical_path.to_path_buf().unwrap(),
            expected_workspace_id: view.workspace_id.clone(),
            run_id,
            expected_state_revision: None,
            carrier: CredentialCarrier::open_path(credential).unwrap(),
            operation,
        })
        .unwrap_or_else(|error| fail(credential.parent().unwrap(), stage, error))
        .snapshot
}

fn start(
    view: &WorkspaceView,
    credential: &Path,
    profile: &str,
    key: &str,
    reader: bool,
) -> Result<dolgorae::semantic::RunStartReceipt, dolgorae::machine::MachineError> {
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
    CoreSemanticService.start_caller_run(
        serde_json::from_value(input).unwrap(),
        CredentialCarrier::open_path(credential).unwrap(),
    )
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

fn observed_shell_snapshot(socket: &Path, home: &str, thread: Option<&str>) -> Value {
    let mut rpc =
        dolgorae::app_server::JsonRpcConnection::connect(socket, Duration::from_secs(5)).unwrap();
    let initialized = rpc.request("initialize", json!({"clientInfo":{"name":"dolgorae-m6-observer","version":"1"},"capabilities":{"experimentalApi":false}})).unwrap();
    assert_eq!(initialized["codexHome"], home);
    rpc.notify("initialized", json!({})).unwrap();
    let mut cursor = Value::Null;
    for _ in 0..16 {
        let page = rpc
            .request(
                "experimentalFeature/list",
                json!({"cursor":cursor,"limit":100,"threadId":thread}),
            )
            .unwrap();
        for feature in page["data"].as_array().unwrap() {
            if feature["name"] == "shell_snapshot" {
                assert_eq!(
                    feature["enabled"], false,
                    "native effective feature must be disabled"
                );
                return feature.clone();
            }
        }
        cursor = page["nextCursor"].clone();
        assert!(
            !cursor.is_null(),
            "native shell_snapshot feature observation missing"
        );
    }
    panic!("native feature pagination exceeded bound");
}

fn default_feature_observation(root: &Path, snapshot: &RunSnapshot) -> Value {
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
    let shared_feature = observed_shell_snapshot(
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
    let dedicated_feature = observed_shell_snapshot(
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
        "run_launch_digest":snapshot.manifest.profile.launch_contract_sha256,"effective_shell_snapshot":dedicated_feature["enabled"],"shared_feature_observation":shared_feature,"dedicated_feature_observation":dedicated_feature,"dedicated_argv":dedicated.argv,"quiescent_startup":true});
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
    let SemanticResult::Workspace(view) = service
        .execute(&SemanticCommand::Initialize {
            path: Some(root.join("home/workspace")),
            mode: WorkspaceMode::NonGit,
        })
        .unwrap()
    else {
        panic!("workspace initialization result");
    };
    let home = DolgoraeHome::system().unwrap();
    for name in ["fixture-a", "fixture-b"] {
        GlobalProfileStore::new(&home)
            .add(
                name.to_owned(),
                RuntimeProfile {
                    argv: {
                        let mut argv = vec![codex.to_str().unwrap().to_owned()];
                        if let Ok(variant) =
                            fs::read_to_string(root.join("default-feature-variant"))
                        {
                            match variant.as_str() {
                                "argv-enable" => {
                                    argv.extend(["--enable".into(), "shell_snapshot".into()])
                                }
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
                },
            )
            .unwrap();
    }
    let credential = if std::env::var("DOLGORAE_NATIVE_READONLY_POOL").as_deref() == Ok("1") {
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
    create_controller_credential(
        &credential,
        ControllerKind::Automation,
        "native-caller-fixture".to_owned(),
        None,
        None,
    )
    .unwrap();
    let operator = root.join("operator.json");
    OperatorStore::new(home.operator_root())
        .initialize(&operator)
        .unwrap();
    let state_root = home.workspace_root(&view.workspace_id);
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let result = std::panic::catch_unwind(|| {
        if std::env::var("DOLGORAE_NATIVE_READONLY_POOL").as_deref() == Ok("1") {
            return pool_campaign(root, &home, &view, &credential);
        }
        let first = start(&view, &credential, "fixture-a", "first", false)
            .unwrap_or_else(|error| fail(root, "caller startup", error));
        let first_id = first.snapshot.manifest.run_id;
        let first_thread = first.snapshot.projection.thread_id.clone().unwrap();
        check_policy(&state_root, &first.snapshot, Access::Write, &first_thread);
        let default_features = root
            .join("default-feature-variant")
            .exists()
            .then(|| default_feature_observation(root, &first.snapshot));
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
        let busy = service
            .mutate_caller_run(RunMutationInput {
                workspace: view.canonical_path.to_path_buf().unwrap(),
                expected_workspace_id: view.workspace_id.clone(),
                run_id: first_id,
                expected_state_revision: None,
                carrier: CredentialCarrier::open_path(&credential).unwrap(),
                operation: RunMutationOperation::AcquireWriter,
            })
            .err()
            .expect("one Writer across Profiles");
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
        let close = service.mutate_caller_run(RunMutationInput {
            workspace: view.canonical_path.to_path_buf().unwrap(),
            expected_workspace_id: view.workspace_id.clone(),
            run_id: id,
            expected_state_revision: None,
            carrier: CredentialCarrier::open_path(&credential).unwrap(),
            operation: RunMutationOperation::Close { interrupt: false },
        });
        match close {
            Ok(receipt) => clean &= receipt.snapshot.projection.lifecycle == RunLifecycle::Closed,
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
    let pool_limit = fs::read_to_string(root.join("pool-limit"))
        .unwrap()
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
    instruct.initialize_templates().unwrap();
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
    let mut socket = dolgorae::gateway_socket::GatewaySocket::bind(home, &socket_path).unwrap();
    let backend = std::sync::Arc::new(dolgorae::gateway_service::CoreGatewayBackend::new(
        home.clone(),
        socket.record().server_instance_id,
    ));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    struct GatewayGuard {
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for GatewayGuard {
        fn drop(&mut self) {
            let _ = self.stop.take().unwrap().send(());
            self.thread.take().unwrap().join().unwrap();
        }
    }
    let _gateway = GatewayGuard {
        stop: Some(stop),
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
            let denial = CoreSemanticService
                .mutate_caller_run(RunMutationInput {
                    workspace: view.canonical_path.to_path_buf().unwrap(),
                    expected_workspace_id: view.workspace_id.clone(),
                    run_id: id,
                    expected_state_revision: None,
                    carrier: CredentialCarrier::open_path(credential).unwrap(),
                    operation: RunMutationOperation::AcquireWriter,
                })
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
    let _result = CoreSemanticService
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
