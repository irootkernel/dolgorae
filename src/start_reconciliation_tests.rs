use crate::operation::{Namespace, StartOperationClosure};
use crate::run::{CallerStartReservation, StartReservation, StartReservationStore};
use crate::workspace::{
    FileIdentity, GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode,
    WorkspaceRecord,
};
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    workspace: String,
    controller: crate::domain::ControllerIdentity,
}

impl Fixture {
    fn new() -> Self {
        let root =
            Path::new("/tmp").join(format!("dolgorae-start-reconciliation-{}", Uuid::now_v7()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let source = root.join("source");
        let state = root.join("state");
        for path in [
            &source,
            &state,
            &state.join("runs"),
            &state.join("runtime"),
            &state.join("runtime/locks"),
        ] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        let workspace = crate::workspace::workspace_id(&source);
        let identity = |path: &Path| {
            let metadata = fs::metadata(path).unwrap();
            FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        };
        let record = WorkspaceRecord {
            schema_version: 1,
            workspace_id: workspace.clone(),
            canonical_path: LosslessPath::from_path(&source),
            mode: WorkspaceMode::NonGit,
            initial_git_baseline: GitBaseline::empty(),
            state_root_identity: identity(&state),
            lock_root_identity: identity(&state.join("runtime/locks")),
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(state.join("workspace.json"))
            .unwrap();
        file.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        let sample: CallerStartReservation = serde_json::from_str(include_str!(
            "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
        ))
        .unwrap();
        Self {
            root,
            state,
            workspace,
            controller: sample.accepted_intent.controller.identity,
        }
    }

    fn store(&self) -> StartReservationStore<SystemWorkspacePlatform> {
        StartReservationStore::new(SystemWorkspacePlatform, &self.state)
    }

    fn namespace(&self, key: &str) -> Namespace {
        Namespace::delegation(&self.controller, &self.workspace, None, "start_run", key).unwrap()
    }

    fn slot(&self, key: &str) -> PathBuf {
        self.state
            .join("idempotency/run-start")
            .join(format!("{}.json", crate::jcs::sha256_hex(key.as_bytes())))
    }

    fn witness(&self, key: &str) -> PathBuf {
        self.state
            .join("idempotency/run-start-closures")
            .join(format!("{}.json", crate::jcs::sha256_hex(key.as_bytes())))
    }

    fn lookup(&self, key: &str) -> Result<Value, crate::machine::MachineError> {
        crate::semantic::lookup_start_operation_at(
            &self.state,
            &self.workspace,
            &self.controller,
            key,
        )
    }

    fn reconcile(&self, key: &str) -> Result<Value, crate::machine::MachineError> {
        crate::semantic::reconcile_start_operation_at(
            &self.state,
            &self.workspace,
            &self.controller,
            key,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn start_reconciliation_observes_a_clean_uninitialized_orchestration_extension() {
    let f = Fixture::new();
    let path = crate::engagement::EngagementStore::workspace_database_path(&f.state);
    drop(crate::engagement::EngagementStore::open(&path).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    let schema = || {
        connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='orchestrated_sessions'",
        [], |row| row.get::<_, i64>(0),
    ).unwrap()
    };
    assert_eq!(schema(), 0);
    let closed = f.reconcile("before-first-start").unwrap();
    assert_eq!(closed["state"], "closed_without_acceptance");
    assert_eq!(schema(), 0, "reconciliation must not set up the extension");
    let extension: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM metadata WHERE key='orchestration_schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(extension, 0);
    assert_eq!(f.lookup("before-first-start").unwrap(), closed);
    connection.execute_batch(
        "CREATE TABLE caller_policy_session_acceptances(workspace_id TEXT,idempotency_key TEXT);",
    ).unwrap();
    assert_eq!(
        f.reconcile("partial-extension").unwrap_err().code,
        "RECOVERY_REQUIRED"
    );
    assert!(!f.slot("partial-extension").exists());
}

#[test]
fn start_reconciliation_refuses_retained_aggregate_admission_without_a_run() {
    let f = Fixture::new();
    drop(crate::orchestration::OrchestrationStore::open(&f.state).unwrap());
    let connection =
        rusqlite::Connection::open(f.state.join("orchestration/orchestration.sqlite3")).unwrap();
    connection.execute(
        "INSERT INTO caller_policy_session_acceptances(
            session_id,root_run_id,workspace_id,idempotency_key,request_sha256,receipt_json,receipt_sha256
         ) VALUES(?1,?1,?2,'retained-admission',?3,'{}',?3)",
        rusqlite::params![Uuid::now_v7().to_string(), f.workspace, "a".repeat(64)],
    ).unwrap();
    // Even a damaged receipt is retained evidence, never proof of no acceptance.
    assert_eq!(
        f.reconcile("retained-admission").unwrap_err().code,
        "RECOVERY_REQUIRED"
    );
    assert!(!f.slot("retained-admission").exists());
    assert!(!f.witness("retained-admission").exists());
    assert_eq!(
        f.reconcile("independent").unwrap()["state"],
        "closed_without_acceptance"
    );
}

#[test]
fn start_reconciliation_checked_closure_example_binds_its_receipt() {
    let closure: StartOperationClosure = serde_json::from_str(include_str!(
        "../docs/protocol/examples/start-operation-closure-v1-record.valid.json"
    ))
    .unwrap();
    closure.validate().unwrap();
    assert_eq!(
        closure.lookup(),
        serde_json::from_str::<Value>(include_str!(
            "../docs/protocol/examples/start-operation-closure-v1-lookup.valid.json"
        ))
        .unwrap()
    );
}

#[test]
fn start_reconciliation_closes_only_the_original_key_and_retains_its_receipt() {
    let f = Fixture::new();
    assert_eq!(f.lookup("original").unwrap()["state"], "unresolved");
    assert!(
        !f.state.join("idempotency").exists(),
        "lookup must not create a fence"
    );
    let closed = f.reconcile("original").unwrap();
    assert_eq!(closed["state"], "closed_without_acceptance");
    assert_eq!(closed["original_key_permanently_closed"], true);
    assert!(closed.get("input_digest").is_none());
    let slot = fs::read(f.slot("original")).unwrap();
    let witness = fs::read(f.witness("original")).unwrap();
    assert_eq!(f.reconcile("original").unwrap(), closed);
    assert_eq!(f.lookup("original").unwrap(), closed);
    assert_eq!(fs::read(f.slot("original")).unwrap(), slot);
    assert_eq!(fs::read(f.witness("original")).unwrap(), witness);
    assert_eq!(f.lookup("unrelated").unwrap()["state"], "unresolved");
    let second = f.reconcile("second-original").unwrap();
    assert_ne!(
        second["closure_receipt_digest"],
        closed["closure_receipt_digest"]
    );
    assert!(f.store().accepted_caller_capacity().unwrap().is_empty());
    // Every old allocation reader fails closed on the new slot shape.
    assert!(serde_json::from_slice::<StartReservation>(&slot).is_err());
    assert!(serde_json::from_slice::<CallerStartReservation>(&slot).is_err());
    assert!(
        serde_json::from_slice::<
            CallerStartReservation<crate::delegation_identity::InstructionSnapshot>,
        >(&slot)
        .is_err()
    );
    assert_eq!(
        f.store().load("original").unwrap_err().code,
        "IDEMPOTENCY_CONFLICT"
    );
    let attempted = StartReservation {
        schema_version: 1,
        operation: "start_run".into(),
        idempotency_key: "original".into(),
        normalized_identity_sha256: "a".repeat(64),
        run_id: Uuid::now_v7(),
    };
    assert_eq!(
        f.store().reserve(&attempted).unwrap_err().code,
        "IDEMPOTENCY_CONFLICT"
    );
    assert_eq!(fs::read(f.slot("original")).unwrap(), slot);
    assert_eq!(fs::read_dir(f.state.join("runs")).unwrap().count(), 0);
}

#[test]
fn start_reconciliation_recovers_a_fence_without_fabricating_completion_on_lookup() {
    let f = Fixture::new();
    let closure = StartOperationClosure::new(f.namespace("interrupted")).unwrap();
    let store = f.store();
    let lock = store.lock_allocation("run-start", "interrupted").unwrap();
    store.publish_start_closure(&closure, &lock).unwrap();
    drop(lock);
    let original = fs::read(f.slot("interrupted")).unwrap();
    assert_eq!(f.lookup("interrupted").unwrap()["state"], "unresolved");
    assert!(!f.witness("interrupted").exists());
    assert_eq!(
        store.load("interrupted").unwrap_err().code,
        "IDEMPOTENCY_CONFLICT"
    );
    assert_eq!(f.reconcile("interrupted").unwrap(), closure.lookup());
    assert_eq!(fs::read(f.slot("interrupted")).unwrap(), original);
    assert_eq!(f.lookup("interrupted").unwrap(), closure.lookup());
}

#[test]
fn start_reconciliation_rejects_foreign_scope_corruption_and_missing_fence() {
    let f = Fixture::new();
    f.reconcile("closed").unwrap();
    let mut foreign = f.controller.clone();
    foreign.generation += 1;
    assert!(
        crate::semantic::reconcile_start_operation_at(&f.state, &f.workspace, &foreign, "closed")
            .is_err()
    );
    foreign = f.controller.clone();
    foreign.controller_id = Uuid::now_v7();
    assert!(
        crate::semantic::lookup_start_operation_at(&f.state, &f.workspace, &foreign, "closed")
            .is_err()
    );
    assert!(
        crate::semantic::reconcile_start_operation_at(
            &f.state,
            &"f".repeat(64),
            &f.controller,
            "closed"
        )
        .is_err()
    );
    let witness = fs::read(f.witness("closed")).unwrap();
    let mut corrupt: Value = serde_json::from_slice(&witness).unwrap();
    corrupt["receipt_digest"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    fs::write(f.witness("closed"), serde_json::to_vec(&corrupt).unwrap()).unwrap();
    assert!(f.lookup("closed").is_err());
    assert!(f.reconcile("closed").is_err());
    fs::write(f.witness("closed"), &witness).unwrap();
    fs::remove_file(f.slot("closed")).unwrap();
    assert!(f.lookup("closed").is_err());
    assert!(f.reconcile("closed").is_err());
    assert!(f.store().load("closed").is_err());
    assert!(!f.slot("closed").exists());
}

#[test]
fn start_reconciliation_refuses_incomplete_allocation_run_and_workspace_evidence() {
    for scenario in [
        "malformed",
        "misnamed",
        "unexpected-entry",
        "orphan-run",
        "replaced-namespace",
    ] {
        let f = Fixture::new();
        let store = f.store();
        let lock = store.lock_allocation("run-start", "owner").unwrap();
        drop(lock);
        match scenario {
            "malformed" => {
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(f.slot("broken"))
                    .unwrap()
                    .write_all(b"{")
                    .unwrap();
            }
            "misnamed" => {
                let record = StartReservation {
                    schema_version: 1,
                    operation: "start_run".into(),
                    idempotency_key: "different-key".into(),
                    normalized_identity_sha256: "a".repeat(64),
                    run_id: Uuid::now_v7(),
                };
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(f.slot("displaced"))
                    .unwrap()
                    .write_all(&serde_json::to_vec(&record).unwrap())
                    .unwrap();
            }
            "unexpected-entry" => {
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(f.state.join("idempotency/run-start/unrecognized-history"))
                    .unwrap();
            }
            "orphan-run" => {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(f.state.join("runs").join(Uuid::now_v7().to_string()))
                    .unwrap();
            }
            "replaced-namespace" => {
                let path = f.state.join("workspace.json");
                let mut record: WorkspaceRecord =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                record.state_root_identity.inode += 1;
                fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(f.reconcile("unknown").is_err(), "{scenario}");
        assert!(!f.slot("unknown").exists(), "{scenario}");
        assert_eq!(f.lookup("unknown").unwrap()["state"], "unresolved");
    }
}

#[test]
fn start_reconciliation_preserves_an_accepted_public_start_before_run_publication() {
    let f = Fixture::new();
    let old: CallerStartReservation = serde_json::from_str(include_str!(
        "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
    ))
    .unwrap();
    let mut old = old.accepted_intent;
    old.request.idempotency_key = "accepted-original".into();
    old.request.workspace = Some(f.root.join("source").to_string_lossy().into());
    old.request.expected_workspace_id = Some(f.workspace.clone());
    let capture = crate::delegation_identity::InstructionSnapshot::compose(
        old.instructions.common,
        old.instructions.role,
        vec![],
        vec![],
    )
    .unwrap();
    let mut capabilities = old.request.required_capabilities.clone();
    capabilities.sort();
    let configuration = crate::run::AgentConfigurationSnapshot::from_admitted(
        crate::run::AdmittedConfigurationInput {
            admitted: crate::delegation_identity::AdmittedConfiguration {
                schema: crate::delegation_identity::CONFIGURATION_SCHEMA.into(),
                profile_binding_schema: crate::run::ADMITTED_PROFILE_BINDING_SCHEMA.into(),
                profile_binding_digest: format!(
                    "sha256:{}",
                    old.global_profile_binding.digest().unwrap()
                ),
                model: old.readiness.model.clone(),
                effort: old.readiness.default_effort.clone(),
                purpose_kind: old.request.purpose.kind.as_str().into(),
                purpose_label: old.request.purpose.external_label.clone(),
                required_capabilities: capabilities,
                execution_lane: "dedicated".into(),
                requested_access: match old.request.access {
                    crate::domain::CallerWorkspaceAccess::ReadOnly => "read_only",
                    crate::domain::CallerWorkspaceAccess::Writable => "canonical_workspace_write",
                }
                .into(),
                required_assurance: "best_effort_personal_alpha".into(),
                native_subagent_policy: "enabled".into(),
                instruction_identity_digest: capture.instruction_identity_digest.clone(),
            },
            instructions: capture.clone(),
            profile: &old.request.profile,
            profile_binding: &old.global_profile_binding,
            role_reference: Some("reviewer"),
        },
    )
    .unwrap();
    let accepted = CallerStartReservation::new(crate::run::CallerStartIntent {
        run_id: old.run_id,
        created_at: old.created_at,
        workspace_id: f.workspace.clone(),
        canonical_workspace: LosslessPath::from_path(&f.root.join("source")),
        workspace_mode: WorkspaceMode::NonGit,
        start_baseline: GitBaseline::empty(),
        request: old.request,
        controller: old.controller,
        global_profile_binding: old.global_profile_binding,
        instructions: capture,
        admitted_configuration: Some(configuration),
        readiness: old.readiness,
        orchestration: old.orchestration,
    })
    .unwrap();
    f.store().reserve_caller_generation(&accepted).unwrap();
    let before = fs::read(f.slot("accepted-original")).unwrap();
    let lookup = f.reconcile("accepted-original").unwrap();
    assert_eq!(lookup["state"], "unresolved");
    assert_eq!(
        lookup["known_subjects"]["run_ref"],
        accepted.run_id.to_string()
    );
    assert_eq!(fs::read(f.slot("accepted-original")).unwrap(), before);
    assert!(!f.witness("accepted-original").exists());

    let id = accepted.run_id;
    let root = crate::run::run_root(&f.state, id);
    crate::workspace::create_directory(&root, 0o700).unwrap();
    let receipt = crate::run::public_start_operation_receipt(&accepted.accepted_intent).unwrap();
    let deleted = crate::run_deletion::RunDeletion::capture(
        &f.state,
        &f.workspace,
        id,
        crate::domain::RunLifecycle::StartFailed,
        accepted.accepted_intent.controller,
        Some(receipt.clone()),
    )
    .unwrap();
    deleted.publish(&f.state).unwrap();
    deleted.finish(&f.state).unwrap();
    let lookup = f.lookup("accepted-original").unwrap();
    assert_eq!(lookup["state"], "accepted");
    assert_eq!(lookup["current_outcome"], "failed");
    assert_eq!(lookup["receipt"], serde_json::to_value(receipt).unwrap());
    assert_eq!(f.reconcile("accepted-original").unwrap(), lookup);
    assert_eq!(
        f.reconcile("unrelated-after-delete").unwrap()["state"],
        "closed_without_acceptance"
    );
    assert_eq!(fs::read(f.slot("accepted-original")).unwrap(), before);
}

#[test]
fn start_reconciliation_waits_for_the_original_start_and_cannot_replace_acceptance() {
    let f = Fixture::new();
    let store = f.store();
    let lock = store.lock_allocation("run-start", "in-flight").unwrap();
    let state = f.state.clone();
    let workspace = f.workspace.clone();
    let controller = f.controller.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = crate::semantic::reconcile_start_operation_at(
            &state,
            &workspace,
            &controller,
            "in-flight",
        );
        done_tx
            .send(
                result
                    .map(|value| value["state"].clone())
                    .map_err(|error| error.code),
            )
            .unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        done_rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    let accepted = StartReservation {
        schema_version: 1,
        operation: "start_run".into(),
        idempotency_key: "in-flight".into(),
        normalized_identity_sha256: "a".repeat(64),
        run_id: Uuid::now_v7(),
    };
    store.reserve(&accepted).unwrap();
    let original = fs::read(f.slot("in-flight")).unwrap();
    drop(lock);
    assert_eq!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap_err(),
        "IDEMPOTENCY_CONFLICT"
    );
    thread.join().unwrap();
    assert_eq!(fs::read(f.slot("in-flight")).unwrap(), original);
    assert!(!f.witness("in-flight").exists());
}
