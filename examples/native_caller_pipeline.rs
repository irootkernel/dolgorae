//! Explicitly opted-in, isolated caller service acceptance driver.
#![forbid(unsafe_code)]

use dolgorae::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use dolgorae::darwin::DarwinSystem;
use dolgorae::domain::{Access, ControllerKind, PolicyVerification, RunLifecycle};
use dolgorae::global_profile::GlobalProfileStore;
use dolgorae::paths::DolgoraeHome;
use dolgorae::profile::{ProfileOperation, execute_global};
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
use std::ffi::OsString;
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
    for name in ["fixture-a", "fixture-b"] {
        if GlobalProfileStore::new(home).resolve(name).is_err() {
            continue;
        }
        let args = [
            OsString::from(name),
            OsString::from("--operator-file"),
            operator.as_os_str().to_owned(),
        ];
        if let Err(error) = execute_global(ProfileOperation::ServerStop, &args) {
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
                    argv: vec![codex.to_str().unwrap().to_owned()],
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
    let credential = root.join("controller.json");
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
        let first = start(&view, &credential, "fixture-a", "first", false)
            .unwrap_or_else(|error| fail(root, "caller startup", error));
        let first_id = first.snapshot.manifest.run_id;
        let first_thread = first.snapshot.projection.thread_id.clone().unwrap();
        check_policy(&state_root, &first.snapshot, Access::Write, &first_thread);
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
            "native_account_environment":"platform_owned"})
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
        fs::write(&history, br#"{"schema_version":1,"bindings":{"binding":{"selected_name":"fixture-a","definition_sha256":"digest","server_key":"key","launch_snapshot_sha256":"snapshot"}}}"#).unwrap();
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
