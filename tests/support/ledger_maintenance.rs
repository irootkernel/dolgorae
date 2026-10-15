//! Workspace-scoped checkpoint maintenance against native isolated durable state.
#![cfg(target_os = "macos")]
use super::support::Fixture;
use dolgorae::controller::{CredentialCarrier, OperatorStore};
use dolgorae::fault::{FaultBarrier, FaultInjected, FaultInjector};
use dolgorae::machine::{FailureEnvelope, MachineError};
use dolgorae::maintenance::{MaintenancePlan, apply_checkpoint_with_faults};
use dolgorae::run::RunStore;
use dolgorae::workspace::SystemWorkspacePlatform;
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::process::{Command, Stdio};
use uuid::Uuid;

const BARRIERS: [FaultBarrier; 10] = [
    FaultBarrier::AfterMaintenanceOwnerSync,
    FaultBarrier::AfterMaintenanceSegmentSync,
    FaultBarrier::AfterMaintenanceCheckpointSync,
    FaultBarrier::AfterMaintenanceReceiptIndexSync,
    FaultBarrier::AfterMaintenanceManifestSync,
    FaultBarrier::AfterMaintenanceDirectorySync,
    FaultBarrier::AfterMaintenanceSelectionReplace,
    FaultBarrier::AfterMaintenanceSelectionSync,
    FaultBarrier::AfterMaintenanceWorkspaceSelectionSync,
    FaultBarrier::AfterMaintenanceCompletionSync,
];
struct Crash(FaultBarrier);
impl FaultInjector for Crash {
    fn check(&self, barrier: FaultBarrier) -> Result<(), FaultInjected> {
        if barrier == self.0 {
            Err(FaultInjected(barrier))
        } else {
            Ok(())
        }
    }
}
fn checked(envelope: &Value) {
    checked_schema(envelope, "dolgorae-machine-v5.schema.json");
}
fn checked_schema(envelope: &Value, schema: &str) {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut validator = Command::new(repository.join(".venv/bin/python"))
        .args(["-B", "-c", "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path(sys.argv[1])/'tests/e2e')); from schema_support import validator,assert_valid; assert_valid(json.load(sys.stdin),validator(pathlib.Path(sys.argv[1])/'docs/protocol',sys.argv[2]),'native maintenance')"])
        .arg(repository).arg(schema).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    validator
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(envelope).unwrap())
        .unwrap();
    let output = validator.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn checked_frozen_error(error: MachineError) {
    let mut envelope = FailureEnvelope::new("run.start", error);
    for version in [2, 4] {
        envelope.schema_version = version;
        checked_schema(
            &serde_json::to_value(&envelope).unwrap(),
            &format!("dolgorae-machine-v{version}.schema.json"),
        );
    }
}
fn publication_candidate(fixture: &Fixture) -> dolgorae::run::RunManifest {
    let store = RunStore::new(SystemWorkspacePlatform, &fixture.state_root);
    let run = fs::read_dir(fixture.state_root.join("runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find_map(|name| Uuid::parse_str(&name).ok())
        .unwrap();
    let mut candidate = store.load_manifest(run).unwrap();
    assert!(candidate.aggregate_binding.is_none());
    candidate.run_id = Uuid::now_v7();
    candidate
}
fn assert_admission_refused(fixture: &Fixture, namespace_valid: bool) {
    let database =
        dolgorae::engagement::EngagementStore::workspace_database_path(&fixture.state_root);
    let before_database = fs::read(&database).unwrap();
    let wal = database.with_extension("sqlite3-wal");
    let before_wal = fs::read(&wal).ok();
    let runs = fixture.state_root.join("runs");
    let before_runs = fs::read_dir(&runs).unwrap().count();
    let candidate = publication_candidate(fixture);
    let store = RunStore::new(SystemWorkspacePlatform, &fixture.state_root);
    let error = store
        .publish(&candidate)
        .expect_err("Run publication crossed retained maintenance admission");
    if namespace_valid {
        assert_eq!(error.code, "RUN_BUSY");
        assert!(error.retryable);
        assert_eq!(
            error.details,
            serde_json::json!({"run_id":candidate.run_id,"owner_kind":"startup"})
        );
    } else {
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
    }
    checked_frozen_error(error);
    assert!(!runs.join(candidate.run_id.to_string()).exists());
    let original = fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find_map(|name| Uuid::parse_str(&name).ok())
        .unwrap();
    let ledger_error = dolgorae::ledger::Ledger::open(runs.join(original.to_string()), original)
        .err()
        .expect("ledger writer crossed retained maintenance admission");
    assert_eq!(
        matches!(ledger_error, dolgorae::ledger::LedgerError::WriterBusy(_)),
        namespace_valid
    );
    for error in [
        dolgorae::engagement::EngagementStore::open(&database)
            .err()
            .expect("database writer crossed retained maintenance admission"),
        dolgorae::orchestration::OrchestrationStore::open_observer(&fixture.state_root)
            .err()
            .expect("database observer crossed retained maintenance admission"),
    ] {
        if namespace_valid {
            assert_eq!(error.code, "CAPABILITY_UNSUPPORTED");
            assert_eq!(
                error.details,
                serde_json::json!({"profile":null,"capability_name":"dolgorae.workspace-maintenance/v014","support":"recognized_unsupported"})
            );
        } else {
            assert_eq!(error.code, "RUNTIME_PATH_INVALID");
        }
        checked_frozen_error(error);
    }
    assert_eq!(fs::read(database).unwrap(), before_database);
    assert_eq!(fs::read(wal).ok(), before_wal);
    assert_eq!(fs::read_dir(runs).unwrap().count(), before_runs);
}
fn assert_admission_open(fixture: &Fixture) {
    let candidate = publication_candidate(fixture);
    let store = RunStore::new(SystemWorkspacePlatform, &fixture.state_root);
    let original = fs::read_dir(fixture.state_root.join("runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find_map(|name| Uuid::parse_str(&name).ok())
        .unwrap();
    assert!(
        dolgorae::ledger::Ledger::open(
            fixture.state_root.join("runs").join(original.to_string()),
            original
        )
        .is_ok()
    );
    assert!(
        dolgorae::orchestration::OrchestrationStore::open_observer(&fixture.state_root).is_ok()
    );
    store.publish(&candidate).unwrap();
    fs::remove_dir_all(
        fixture
            .state_root
            .join("runs")
            .join(candidate.run_id.to_string()),
    )
    .unwrap();
}
fn plan(f: &Fixture, generation: u64) -> (MaintenancePlan, std::path::PathBuf) {
    let output = f.command(&[
        "maintenance",
        "plan",
        "--workspace",
        f.workspace.to_str().unwrap(),
        "--operator-file",
        f.operator.to_str().unwrap(),
        "--selected-generation",
        &generation.to_string(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    checked(&envelope);
    let plan: MaintenancePlan = serde_json::from_value(envelope["data"]["plan"].clone()).unwrap();
    let path = f.root.join(format!("plan-{}.json", plan.plan_ref));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(&serde_json::to_vec(&plan).unwrap()).unwrap();
    file.sync_all().unwrap();
    (plan, path)
}
fn get(f: &Fixture, plan: &MaintenancePlan) -> Value {
    let output = f.command(&[
        "maintenance",
        "get",
        "--workspace",
        f.workspace.to_str().unwrap(),
        "--operator-file",
        f.operator.to_str().unwrap(),
        "--selected-generation",
        &plan.selected_generation.to_string(),
        "--plan-id",
        &plan.plan_ref.to_string(),
    ]);
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    checked(&envelope);
    assert!(output.status.success(), "{envelope}");
    envelope["data"]["maintenance"].clone()
}
fn apply(f: &Fixture, path: &std::path::Path) -> std::process::Output {
    f.command(&[
        "maintenance",
        "apply",
        "--workspace",
        f.workspace.to_str().unwrap(),
        "--operator-file",
        f.operator.to_str().unwrap(),
        "--plan-file",
        path.to_str().unwrap(),
        "--approve",
    ])
}
pub(super) fn maintenance_recovers_every_switch_boundary_and_preserves_closed_run_history() {
    let fixture = Fixture::new("run_start_model_list.json");
    let first = fixture.start_run(&[]);
    let second = fixture.start_run(&[]);
    for run in [&first, &second] {
        fixture.cli(&[
            "run",
            "--controller-file",
            fixture.controller.to_str().unwrap(),
            "send",
            run,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--message",
            "checkpoint receipt",
            "--idempotency-key",
            "checkpoint-turn",
        ]);
        // This scripted Profile endpoint serves one retained connection at a time.
        if run == &first {
            fixture.cli(&[
                "run",
                "--controller-file",
                fixture.controller.to_str().unwrap(),
                "close",
                run,
                "--workspace",
                fixture.workspace.to_str().unwrap(),
            ]);
        }
    }
    let (live_plan, live_path) = plan(&fixture, 1);
    let refused = apply(&fixture, &live_path);
    let refused: Value = serde_json::from_slice(&refused.stdout).unwrap();
    checked(&refused);
    assert_eq!(refused["error"]["code"], "RUN_BUSY");
    assert_eq!(get(&fixture, &live_plan)["state"], "planned");
    for run in [&first, &second] {
        fixture.cli(&[
            "run",
            "--controller-file",
            fixture.controller.to_str().unwrap(),
            "close",
            run,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
        ]);
    }
    drop(dolgorae::orchestration::OrchestrationStore::open(&fixture.state_root).unwrap());
    let histories = [&first, &second].map(|run| {
        fs::read(
            fixture
                .state_root
                .join("runs")
                .join(run)
                .join("audit.jsonl"),
        )
        .unwrap()
    });
    let mut retained = Vec::new();
    let mut first_workspace_selector = Vec::new();
    for (index, barrier) in BARRIERS.into_iter().enumerate() {
        let (original, path) = plan(&fixture, index as u64 + 1);
        {
            let carrier = CredentialCarrier::open_path(&fixture.operator).unwrap();
            let operator = OperatorStore::new(fixture.home.join(".dolgorae/operator"))
                .authorize(&carrier)
                .unwrap();
            let failure = apply_checkpoint_with_faults(
                &fixture.state_root,
                &fixture.workspace_id,
                &original,
                true,
                &operator,
                &Crash(barrier),
            )
            .unwrap_err();
            assert_eq!(failure.code, "RECOVERY_REQUIRED");
            assert_eq!(
                failure.details["operation_ref"],
                original.plan_ref.to_string()
            );
            assert_eq!(failure.details["required_action"], "resume_maintenance");
        }
        assert_eq!(
            get(&fixture, &original)["state"],
            if barrier == FaultBarrier::AfterMaintenanceCompletionSync {
                "completed"
            } else {
                "recovery_needed"
            }
        );
        if barrier == FaultBarrier::AfterMaintenanceCompletionSync {
            assert_admission_open(&fixture);
        } else {
            assert_admission_refused(&fixture, true);
        }
        if barrier == FaultBarrier::AfterMaintenanceSelectionReplace {
            let control = fixture.state_root.join("maintenance");
            let progress = control.join(format!("{}.json", original.plan_ref));
            let owner = control.join(format!("{}.owner.json", original.plan_ref));
            let assert_namespace_recovery = || {
                assert_admission_refused(&fixture, false);
                for refusal in [
                    apply(&fixture, &path),
                    fixture.command(&[
                        "maintenance",
                        "get",
                        "--workspace",
                        fixture.workspace.to_str().unwrap(),
                        "--operator-file",
                        fixture.operator.to_str().unwrap(),
                        "--selected-generation",
                        &original.selected_generation.to_string(),
                        "--plan-id",
                        &original.plan_ref.to_string(),
                    ]),
                ] {
                    let envelope: Value = serde_json::from_slice(&refusal.stdout).unwrap();
                    checked(&envelope);
                    assert_eq!(
                        envelope["error"]["classification"], "RecoveryNeeded",
                        "{envelope}"
                    );
                    assert_eq!(envelope["error"]["effect"], "unresolved");
                    assert_eq!(envelope["error"]["required_action"], "restore_state");
                    assert_eq!(
                        envelope["error"]["operation_ref"],
                        original.plan_ref.to_string()
                    );
                }
            };
            for damaged in [&progress, &owner] {
                fs::set_permissions(damaged, fs::Permissions::from_mode(0o644)).unwrap();
                assert_namespace_recovery();
                fs::set_permissions(damaged, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let original_progress = fs::read(&progress).unwrap();
            let original_owner = fs::read(&owner).unwrap();
            fs::remove_file(&owner).unwrap();
            assert_admission_refused(&fixture, false);
            fs::write(&owner, &original_owner).unwrap();
            fs::set_permissions(&owner, fs::Permissions::from_mode(0o600)).unwrap();
            fs::set_permissions(&control, fs::Permissions::from_mode(0o755)).unwrap();
            assert_admission_refused(&fixture, false);
            fs::set_permissions(&control, fs::Permissions::from_mode(0o700)).unwrap();
            for (field, invalid) in [
                ("state", Value::from("unsupported")),
                (
                    "schema",
                    Value::from("dolgorae.native-maintenance-operation/invalid"),
                ),
            ] {
                let mut damaged: Value = serde_json::from_slice(&original_progress).unwrap();
                damaged[field] = invalid;
                let damaged_bytes = serde_json::to_vec(&damaged).unwrap();
                fs::write(&progress, &damaged_bytes).unwrap();
                assert_namespace_recovery();
                assert_eq!(fs::read(&progress).unwrap(), damaged_bytes);
                assert_eq!(fs::read(&owner).unwrap(), original_owner);
                fs::write(&progress, &original_progress).unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let invalid_input = apply(&fixture, &path);
            let envelope: Value = serde_json::from_slice(&invalid_input.stdout).unwrap();
            checked(&envelope);
            assert_eq!(envelope["error"]["classification"], "Rejected");
            assert_eq!(envelope["error"]["effect"], "none");
            assert_eq!(envelope["error"]["operation_ref"], Value::Null);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let output = apply(&fixture, &path);
        let completed: Value = serde_json::from_slice(&output.stdout).unwrap();
        checked(&completed);
        assert!(output.status.success(), "{completed}");
        assert_eq!(
            completed["data"]["maintenance"]["operation_ref"],
            original.plan_ref.to_string()
        );
        assert_eq!(
            completed["data"]["maintenance"]["new_generation"],
            index as u64 + 2
        );
        assert_admission_open(&fixture);
        let repeated = apply(&fixture, &path);
        assert!(repeated.status.success());
        for (run, original_bytes) in [&first, &second].into_iter().zip(&histories) {
            let root = fixture.state_root.join("runs").join(run);
            let state: dolgorae::ledger::RunStateProjection =
                serde_json::from_slice(&fs::read(root.join("state.json")).unwrap()).unwrap();
            let ledger = dolgorae::ledger::ObservedLedger::open_complete(
                &root,
                Uuid::parse_str(run).unwrap(),
                state.ledger_head.sequence,
            )
            .unwrap();
            let mut exported = Vec::new();
            ledger.write_canonical_history(&mut exported).unwrap();
            assert_eq!(&exported, original_bytes);
            assert_eq!(fs::read(root.join("audit.jsonl")).unwrap(), *original_bytes);
            assert!(
                root.join(format!(
                    "ledger-generations/{}/manifest.json",
                    original.plan_ref
                ))
                .is_file()
            );
        }
        let workspace_selection = fixture.state_root.join("maintenance/selected.json");
        if index == 0 {
            first_workspace_selector = fs::read(&workspace_selection).unwrap();
        }
        if index == 1 {
            let current = fs::read(&workspace_selection).unwrap();
            let valid: Value = serde_json::from_slice(&current).unwrap();
            let mut unowned = valid.clone();
            unowned["operation_ref"] = Value::String(Uuid::now_v7().to_string());
            let mut wrong_generation = valid.clone();
            wrong_generation["generation"] = serde_json::json!(99);
            let mut wrong_basis = valid.clone();
            wrong_basis["runs"][0]["head"]["hash"] = Value::String("0".repeat(64));
            for (case, replacement) in [
                first_workspace_selector.clone(),
                serde_json::to_vec(&unowned).unwrap(),
                serde_json::to_vec(&wrong_generation).unwrap(),
                serde_json::to_vec(&wrong_basis).unwrap(),
            ]
            .into_iter()
            .enumerate()
            {
                fs::write(&workspace_selection, replacement).unwrap();
                let refused = fixture.command(&[
                    "maintenance",
                    "plan",
                    "--workspace",
                    fixture.workspace.to_str().unwrap(),
                    "--operator-file",
                    fixture.operator.to_str().unwrap(),
                    "--selected-generation",
                    "2",
                ]);
                let envelope: Value = serde_json::from_slice(&refused.stdout).unwrap();
                checked(&envelope);
                assert_eq!(
                    envelope["error"]["code"],
                    if case == 1 {
                        "LOOKUP_NAMESPACE_UNAVAILABLE"
                    } else {
                        "RECOVERY_REQUIRED"
                    },
                    "{envelope}"
                );
            }
            fs::write(&workspace_selection, current).unwrap();
        }
        retained.push(original);
    }
    for run in [&first, &second] {
        let root = fixture.state_root.join("runs").join(run);
        checked_schema(
            &serde_json::from_slice::<Value>(
                &fs::read(root.join("ledger-selection.json")).unwrap(),
            )
            .unwrap(),
            "dolgorae-native-ledger-maintenance-v1.schema.json",
        );
        for directory in fs::read_dir(root.join("ledger-generations")).unwrap() {
            let directory = directory.unwrap().path();
            for name in [
                "manifest.json",
                "checkpoint.json",
                "receipts.json",
                "selection.json",
            ] {
                checked_schema(
                    &serde_json::from_slice::<Value>(&fs::read(directory.join(name)).unwrap())
                        .unwrap(),
                    "dolgorae-native-ledger-maintenance-v1.schema.json",
                );
            }
        }
    }
    for entry in fs::read_dir(fixture.state_root.join("maintenance")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            checked_schema(
                &serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
                "dolgorae-native-ledger-maintenance-v1.schema.json",
            );
        }
    }
    for original in &retained {
        assert_eq!(get(&fixture, original)["state"], "completed");
    }
    // Missing or damaged namespace is not proof that Apply did not happen.
    let original = retained.last().unwrap();
    let operation = fixture
        .state_root
        .join("maintenance")
        .join(format!("{}.json", original.plan_ref));
    let owner = operation.with_file_name(format!("{}.owner.json", original.plan_ref));
    let held_owner = owner.with_extension("held");
    fs::rename(&owner, &held_owner).unwrap();
    let missing_owner = fixture.command(&[
        "maintenance",
        "get",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--operator-file",
        fixture.operator.to_str().unwrap(),
        "--selected-generation",
        &original.selected_generation.to_string(),
        "--plan-id",
        &original.plan_ref.to_string(),
    ]);
    fs::rename(&held_owner, &owner).unwrap();
    let missing_owner: Value = serde_json::from_slice(&missing_owner.stdout).unwrap();
    checked(&missing_owner);
    assert_eq!(
        missing_owner["error"]["code"],
        "LOOKUP_NAMESPACE_UNAVAILABLE"
    );
    let bytes = fs::read(&operation).unwrap();
    fs::write(&operation, b"{invalid}").unwrap();
    let output = fixture.command(&[
        "maintenance",
        "get",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--operator-file",
        fixture.operator.to_str().unwrap(),
        "--selected-generation",
        &original.selected_generation.to_string(),
        "--plan-id",
        &original.plan_ref.to_string(),
    ]);
    fs::write(&operation, bytes).unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    checked(&envelope);
    assert_eq!(envelope["error"]["code"], "LOOKUP_NAMESPACE_CORRUPT");
    assert_ne!(envelope["error"]["code"], "MAINTENANCE_NOT_APPLIED");
    // Close before checkpoint leaves an empty append; its historical final seal remains usable.
    let closed_root = fixture.state_root.join("runs").join(&first);
    let selection: Value =
        serde_json::from_slice(&fs::read(closed_root.join("ledger-selection.json")).unwrap())
            .unwrap();
    let manifest: Value = serde_json::from_slice(
        &fs::read(closed_root.join(selection["manifest"]["path"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(closed_root.join(manifest["append_path"].as_str().unwrap()))
            .unwrap()
            .len(),
        0
    );
    let checkpoint: Value = serde_json::from_slice(
        &fs::read(closed_root.join(manifest["checkpoint"]["path"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    let final_history = closed_root.join(
        checkpoint["segments"].as_array().unwrap().last().unwrap()["content"]["path"]
            .as_str()
            .unwrap(),
    );
    let mut bytes = fs::read(&final_history).unwrap();
    bytes[0] ^= 1;
    fs::write(&final_history, bytes).unwrap();
    assert!(
        dolgorae::conformance::ConformantLedger::open(
            &closed_root,
            Uuid::parse_str(&first).unwrap()
        )
        .is_err()
    );
    dolgorae::conformance::delete_run_confirmed(
        &fixture.state_root,
        Uuid::parse_str(&first).unwrap(),
        true,
    )
    .unwrap();
    assert!(!closed_root.exists());

    // Preserve SPEC-010's restricted terminal delete after generation rollover.
    let idle = fixture.start_run(&[]);
    let (terminal_plan, terminal_path) = plan(&fixture, 11);
    let switched = apply(&fixture, &terminal_path);
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stdout)
    );
    fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "close",
        &idle,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    let run_root = fixture.state_root.join("runs").join(&idle);
    let history = run_root.join(format!(
        "ledger-generations/{}/history.jsonl",
        terminal_plan.plan_ref
    ));
    let mut bytes = fs::read(&history).unwrap();
    bytes[0] ^= 1;
    fs::write(&history, bytes).unwrap();
    assert!(
        dolgorae::conformance::ConformantLedger::open(&run_root, Uuid::parse_str(&idle).unwrap())
            .is_err()
    );
    assert!(
        dolgorae::conformance::delete_run_confirmed(
            &fixture.state_root,
            Uuid::parse_str(&idle).unwrap(),
            false
        )
        .is_err()
    );
    dolgorae::conformance::delete_run_confirmed(
        &fixture.state_root,
        Uuid::parse_str(&idle).unwrap(),
        true,
    )
    .unwrap();
    assert!(!run_root.exists());
}
