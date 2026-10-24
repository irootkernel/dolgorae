//! Real native managed and external successor write/close evidence.
use crate::caller_scope;
use crate::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use crate::darwin::DarwinSystem;
use crate::delegation_service as delegation;
use crate::domain::{Access, ControllerKind, PolicyVerification, RunLifecycle};
use crate::engagement::EngagementStore;
use crate::global_profile::GlobalProfileStore;
use crate::global_runtime::{GlobalMembershipStore, MembershipDisposition, ResolvedGlobalProfile};
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource};
use crate::paths::DolgoraeHome;
use crate::profile::{ProfileOperation, execute_global};
use crate::provisioning::{ProvisioningAdmission, ProvisioningSources};
use crate::run::RunStore;
use crate::semantic::{
    CoreSemanticService, RunMutationInput, RunMutationOperation, SemanticCommand, SemanticResult,
    SemanticRunSettlementPort, SemanticService,
};
use crate::snapshot::RunSnapshot;
use crate::workspace::{
    NativeSubagents, RuntimeProfile, SystemWorkspacePlatform, WorkspaceMode, WorkspaceView,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

const PROFILE: &str = "integrated-successor";
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn checked<T>(root: &Path, stage: &str, result: Result<T, crate::machine::MachineError>) -> T {
    result.unwrap_or_else(|error| {
        private(
            &root.join("failure.json"),
            &serde_json::to_vec(&json!({"stage":stage,"error":error})).unwrap(),
        );
        panic!("native successor {stage}: {}", error.code)
    })
}
fn mutate(
    view: &WorkspaceView,
    carrier: &CredentialCarrier,
    run: Uuid,
    operation: RunMutationOperation,
) -> RunSnapshot {
    CoreSemanticService
        .mutate_caller_run(RunMutationInput {
            workspace: view.canonical_path.to_path_buf().unwrap(),
            expected_workspace_id: view.workspace_id.clone(),
            run_id: run,
            expected_state_revision: None,
            carrier: CredentialCarrier::duplicate_fd(carrier.raw_fd()).unwrap(),
            operation,
        })
        .unwrap()
        .snapshot
}
fn git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "user.email=native@example.invalid",
            "-c",
            "user.name=Native Acceptance",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn membership(home: &DolgoraeHome, manifest: &crate::run::RunManifest) -> MembershipDisposition {
    let binding = manifest.global_profile_binding.as_ref().unwrap();
    GlobalMembershipStore::new(home, &binding.selected_name, &binding.server_key)
        .unwrap()
        .load()
        .unwrap()
        .members[&format!("{}:{}", manifest.workspace_id, manifest.run_id)]
        .disposition
}
fn payload(record: &crate::audit::AuditRecord) -> Value {
    serde_json::from_slice(&crate::jcs::canonicalize(record.payload()).unwrap()).unwrap()
}
fn verify_closed(root: &Path, workspace: &WorkspaceView, run: Uuid) -> Value {
    let (snapshot, observed) = RunSnapshot::observe(root, run, 0).unwrap();
    assert_eq!(snapshot.projection.lifecycle, RunLifecycle::Closed);
    assert_eq!(observed.effective_policy.access, Access::Read);
    assert_eq!(
        observed.effective_policy.verification,
        PolicyVerification::Verified
    );
    assert_eq!(
        snapshot.writer.state,
        crate::writer::WriterAuthorityState::None
    );
    let runtime_root = crate::worker::runtime_root(root);
    assert!(
        !crate::worker::runtime_record_path(&runtime_root, run)
            .unwrap()
            .exists()
    );
    let absent = crate::worker::verified_absence_marker(&runtime_root, run)
        .unwrap()
        .expect("owned cleanup must prove generation absence before Profile retirement");
    assert_eq!(absent.run_id, run);
    assert_eq!(absent.run_generation, snapshot.projection.run_generation);
    let ledger = snapshot.observed_ledger(root).unwrap();
    let records = ledger.records();
    let initial_operation = snapshot
        .manifest
        .aggregate_binding
        .as_ref()
        .and_then(|binding| {
            (binding.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                && binding.member_kind == crate::run::AggregateMemberKind::Specialist)
                .then_some(binding.operation_id)
        })
        .unwrap_or(run);
    let initial_intent = records
        .iter()
        .find(|record| {
            record.kind() == crate::audit::AuditKind::TurnIntent
                && payload(record)["schema"] == "dolgorae.caller-native-intent/v1"
        })
        .unwrap();
    assert_eq!(
        payload(initial_intent)["operation_id"],
        initial_operation.to_string()
    );
    let initial_thread = records
        .iter()
        .find(|record| {
            record.kind() == crate::audit::AuditKind::TurnIntent
                && payload(record).get("provisional_thread_id").is_some()
        })
        .unwrap();
    assert_eq!(
        payload(initial_thread)["operation_id"],
        initial_operation.to_string()
    );
    let seal = records
        .iter()
        .find(|record| {
            record.kind() == crate::audit::AuditKind::LifecycleTransition
                && payload(record)["terminal_seal"] == true
                && payload(record)["current"] == "closed"
        })
        .unwrap();
    let reader = records
        .iter()
        .rev()
        .find(|record| {
            record.kind() == crate::audit::AuditKind::WriterReleased
                && payload(record)["effective_policy"]["access"] == "read"
                && payload(record)["effective_policy"]["verification"] == "verified"
        })
        .unwrap();
    let acquired = records
        .iter()
        .rev()
        .find(|record| record.kind() == crate::audit::AuditKind::WriterAcquired)
        .unwrap();
    assert!(acquired.sequence() < reader.sequence());
    assert!(reader.sequence() < seal.sequence());
    assert_eq!(snapshot.manifest.workspace_id, workspace.workspace_id);
    json!({"writer_acquired_sequence":acquired.sequence(),"verified_reader_sequence":reader.sequence(),"seal_sequence":seal.sequence(),"sealed_policy":"read","writer_released":true,"owned_generation_absence_verified":true})
}
fn task(marker: &str) -> crate::task_request::SpecialistTaskRequest {
    crate::task_request::SpecialistTaskRequest {
        purpose: "change".to_owned(),
        brief: marker.to_owned(),
        contexts: Vec::new(),
        criteria: Vec::new(),
        expected_output: "complete_result".to_owned(),
    }
}
fn context<'a>(session: Uuid, key: &'a str) -> delegation::ManagedCallContext<'a> {
    delegation::ManagedCallContext {
        session_id: session,
        source_run_id: session,
        source_turn_id: "native-host-turn",
        source_tool_call_id: "native-host-tool",
        idempotency_key: key,
    }
}

pub fn campaign(root: &Path, codex: &Path) -> Value {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    assert_eq!(
        fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
        root.join("home")
    );
    assert!(
        fs::canonicalize(std::env::var_os("CODEX_HOME").unwrap())
            .unwrap()
            .starts_with(root)
    );
    git(&root.join("workspace"), &["init", "-q"]);
    let SemanticResult::Workspace(view) = CoreSemanticService
        .execute(&SemanticCommand::Initialize {
            path: Some(root.join("workspace")),
            mode: WorkspaceMode::Git,
        })
        .unwrap()
    else {
        panic!("Git workspace")
    };
    let workspace = view.canonical_path.to_path_buf().unwrap();
    private(&workspace.join("file.txt"), b"before\n");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "fixture baseline"]);
    let home = DolgoraeHome::system().unwrap();
    private(
        &home.root().join("home/AGENTS.md"),
        b"integrated-common-017\n",
    );
    private(
        &home.root().join("roles/general/AGENTS.md"),
        b"integrated-general-017\n",
    );
    private(
        &home.root().join("roles/reviewer/AGENTS.md"),
        b"integrated-reviewer-017\n",
    );
    GlobalProfileStore::new(&home)
        .add(
            PROFILE.to_owned(),
            RuntimeProfile {
                argv: vec![codex.to_str().unwrap().to_owned()],
                codex_home: root.join("profile").to_str().unwrap().to_owned(),
                environment: BTreeMap::from([
                    (
                        "PATH".to_owned(),
                        "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
                    ),
                    ("LANG".to_owned(), "C".to_owned()),
                    ("LC_ALL".to_owned(), "C".to_owned()),
                ]),
                native_subagents: NativeSubagents::Enabled,
            },
        )
        .unwrap();
    let binding = ResolvedGlobalProfile::resolve(&home, PROFILE)
        .unwrap()
        .prepare(&home)
        .unwrap();
    let operator = root.join("operator");
    OperatorStore::new(home.operator_root())
        .initialize(&operator)
        .unwrap();
    execute_global(ProfileOperation::ServerStart, &[OsString::from(PROFILE)]).unwrap();
    let mut policy: Value = serde_json::from_str(include_str!(
        "../../docs/protocol/examples/workspace-policy-input-v3.valid.json"
    ))
    .unwrap();
    policy["policy_name"] = json!("native-successor");
    policy["approval_policy"] = json!("fully_delegated");
    policy["roles"][0]["agent_configuration"]["selected_profile"] = json!(PROFILE);
    policy["roles"][0]["agent_configuration"]["model"] = json!("gpt-5.6-luna");
    policy["roles"][0]["agent_configuration"]["default_effort"] = json!("low");
    policy["roles"][0]["agent_configuration"]
        .as_object_mut()
        .unwrap()
        .remove("global_profile_binding_sha256");
    let policy_path = root.join("policy.json");
    private(&policy_path, &serde_json::to_vec(&policy).unwrap());
    crate::markdown_policy::MarkdownPolicyRegistry::discover(Some(&workspace))
        .unwrap()
        .add_file(&policy_path, "native-successor")
        .unwrap();
    let controller = root.join("controller");
    create_controller_credential(
        &controller,
        ControllerKind::InteractiveClient,
        "native-successor".to_owned(),
        None,
        Some("native-successor".to_owned()),
    )
    .unwrap();
    let owner = CredentialCarrier::open_path(&controller).unwrap();
    let mut start: Value = serde_json::from_str(include_str!(
        "../../docs/protocol/examples/workspace-execution-start-defaults.valid.json"
    ))
    .unwrap();
    start["workspace"] = json!(workspace);
    start["profile"] = json!(PROFILE);
    start["control_mode"] = json!("direct_interactive");
    start["access"] = json!("read-only");
    start["purpose"] = json!({"kind":"interactive","external_label":null});
    start["specialist_policy"] = json!("native-successor");
    start["idempotency_key"] = json!("native-root");
    let started = checked(
        root,
        "Primary start",
        CoreSemanticService.start_caller_run(
            serde_json::from_value(start).unwrap(),
            CredentialCarrier::duplicate_fd(owner.raw_fd()).unwrap(),
        ),
    );
    let primary = started.snapshot.manifest.run_id;
    let state_root = home.workspace_root(&view.workspace_id);
    let primary_binding = RunStore::new(SystemWorkspacePlatform, &state_root)
        .load_controller_binding(primary)
        .unwrap();
    let scope = delegation::DelegationScope {
        view: &view,
        state_root: &state_root,
        controller: &primary_binding,
    };
    crate::engagement::ensure_delegation_namespace(&state_root).unwrap();
    let source_store = InstructionSourceStore::new(&home);
    assert!(binding.caller_workspace_supported());
    assert_eq!(
        binding.launch_snapshot.process_static_configuration["features"]["shell_snapshot"],
        json!(false)
    );
    let sources = ProvisioningSources {
        instruct: &source_store,
        role_name: "general",
        role: SelectedInstructionSource::File(vec!["roles".to_owned(), "general".to_owned()]),
        dependencies: Vec::new(),
        selected_guidance: Vec::new(),
        profile_binding: binding,
        admission: ProvisioningAdmission {
            model: "gpt-5.6-luna".to_owned(),
            effort: "low".to_owned(),
            purpose_kind: "implementation".to_owned(),
            purpose_label: None,
            required_capabilities: Vec::new(),
            execution_lane: "dedicated".to_owned(),
            requested_access: "canonical_workspace_write".to_owned(),
            required_assurance: "best_effort_personal_alpha".to_owned(),
            native_subagent_policy: "disabled".to_owned(),
        },
    };
    let (candidate, _) = checked(
        root,
        "provisioning",
        delegation::install_provisioning(&scope, &sources, "native-install", None),
    );
    let port = SemanticRunSettlementPort { carrier: &owner };
    let spawned = checked(
        root,
        "managed spawn",
        delegation::spawn_specialist(
            &scope,
            &context(primary, "native-spawn"),
            &port,
            delegation::SpawnSpecialist {
                provisioning_ref: &candidate.provisioning_ref,
                role_ref: "general",
                agent_configuration_digest: &candidate.agent_configuration_digest,
                expected_revision: None,
            },
        ),
    );
    let member = Uuid::parse_str(&spawned.member.member_ref).unwrap();
    let broker = crate::orchestration::OrchestrationStore::open(&state_root)
        .unwrap()
        .member_credential(member)
        .unwrap();
    let member_carrier = broker.controller_carrier().unwrap();
    let manifest = RunStore::new(SystemWorkspacePlatform, &state_root)
        .load_manifest(member)
        .unwrap();
    assert_eq!(manifest.schema_version, 4);
    assert_eq!(
        manifest.aggregate_binding.as_ref().unwrap().aggregate_id,
        primary
    );
    let capture_after_acquire = std::rc::Rc::new(std::cell::Cell::new(false));
    crate::semantic::set_managed_dispatch_preparation({
        let state = state_root.clone();
        let workspace = workspace.clone();
        let checked = std::rc::Rc::clone(&capture_after_acquire);
        move || {
            let (snapshot, observed) = RunSnapshot::observe(&state, member, 0).unwrap();
            assert_eq!(
                snapshot.writer.state,
                crate::writer::WriterAuthorityState::Active
            );
            assert_eq!(observed.effective_policy.access, Access::Write);
            assert_eq!(
                observed.effective_policy.verification,
                PolicyVerification::Verified
            );
            let store =
                EngagementStore::open(&EngagementStore::workspace_database_path(&state)).unwrap();
            let task:String=store.connection().query_row("SELECT json_extract(accepted_subjects_json,'$.task_ref') FROM delegation_operations WHERE operation_kind='assign_specialist_task' AND idempotency_key='native-write'",[],|r|r.get(0)).unwrap();
            assert_eq!(
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM dispatch_indexes WHERE task_id=?1",
                        [task],
                        |r| r.get::<_, u64>(0)
                    )
                    .unwrap(),
                0
            );
            private(
                &workspace.join("file.txt"),
                b"captured after qualified acquire\n",
            );
            checked.set(true);
        }
    });
    let (assigned, receipt) = checked(
        root,
        "managed assign",
        delegation::assign_specialist_task(
            &scope,
            &context(primary, "native-write"),
            &caller_scope::fixture(32),
            &port,
            delegation::AssignSpecialistTask {
                member_ref: member,
                task: task("native-managed-writer"),
                execution_intent: "canonical_workspace_write",
                target: None,
                deadline_seconds: 600,
                expected_revision: None,
            },
        ),
    );
    assert!(capture_after_acquire.get());
    let task_id = Uuid::parse_str(&assigned.task_ref).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let collected = loop {
        let got = delegation::collect_specialist_task(
            &scope,
            &context(primary, "collect"),
            &port,
            delegation::CollectSpecialistTask {
                member_ref: member,
                task_ref: task_id,
                adapter_id: "native-acceptance",
                adapter_version: "1",
            },
        )
        .unwrap();
        if got.task.state == "completed" {
            break got;
        }
        assert!(Instant::now() < deadline, "native task did not settle");
        std::thread::sleep(Duration::from_millis(10));
    };
    let changes = collected.complete_result.as_ref().unwrap()["changes"]
        .as_array()
        .unwrap();
    assert!(changes.iter().any(|change| change["path"] == "file.txt"));
    let store =
        EngagementStore::open(&EngagementStore::workspace_database_path(&state_root)).unwrap();
    assert!(store.dispatch_index_object(task_id).unwrap().is_some());
    assert!(
        store
            .dispatch_index_baseline(task_id)
            .unwrap()
            .iter()
            .any(|(path, bytes)| path == "file.txt"
                && bytes == b"captured after qualified acquire\n")
    );
    // The terminal hook publishes without a Controller carrier. Pending owned
    // writer cleanup is performed by the next production dispatch or release.
    let failure_evidence =
        dispatch_failures(root, &view, &state_root, primary, member, &scope, &port);
    let reacquired = mutate(
        &view,
        &member_carrier,
        member,
        RunMutationOperation::AcquireWriter,
    );
    assert_eq!(reacquired.captured_effective_policy.access, Access::Write);
    assert_eq!(
        reacquired.captured_effective_policy.verification,
        PolicyVerification::Verified
    );
    let ordering = std::rc::Rc::new(std::cell::RefCell::new(None));
    crate::semantic::set_member_release_before_retirement({
        let root = state_root.clone();
        let home = home.clone();
        let view = view.clone();
        let manifest = manifest.clone();
        let ordering = std::rc::Rc::clone(&ordering);
        move || {
            let checked = verify_closed(&root, &view, member);
            assert_eq!(membership(&home, &manifest), MembershipDisposition::Active);
            *ordering.borrow_mut() = Some(checked);
        }
    });
    let revision = crate::engagement::scope_membership_revision(
        store.connection(),
        &view.workspace_id,
        "orchestrated_session",
        &primary.to_string(),
    )
    .unwrap();
    let released = checked(
        root,
        "managed release",
        delegation::release_specialist(
            &scope,
            &context(primary, "release"),
            &port,
            delegation::ReleaseSpecialist {
                member_ref: member,
                expected_revision: revision,
                allow_interrupt: false,
            },
        ),
    );
    assert_eq!(released.operation.progress, "terminal");
    assert_eq!(
        membership(&home, &manifest),
        MembershipDisposition::Released
    );
    assert!(ordering.borrow().is_some());
    assert!(
        crate::orchestration::BrokerCredential::load(
            &state_root.join("orchestration/broker-credentials"),
            DarwinSystem.current_uid(),
            member
        )
        .is_err()
    );
    let external_evidence = external_campaign(root, &view, &state_root, &home, &candidate);
    let closed = mutate(
        &view,
        &owner,
        primary,
        RunMutationOperation::Close { interrupt: false },
    );
    assert_eq!(closed.projection.lifecycle, RunLifecycle::Closed);
    let args = [
        OsString::from(PROFILE),
        OsString::from("--operator-file"),
        operator.as_os_str().to_owned(),
    ];
    execute_global(ProfileOperation::ServerStop, &args).unwrap();
    private(&root.join("cleanup-result"), b"complete\n");
    json!({"schema":"dolgorae.native-successor-pipeline/v1","passed":true,"managed_manifest":4,"shell_snapshot_disabled":true,"dispatch_failures":failure_evidence,"external":external_evidence,"production_managed_dispatch":true,"broker_writer_acquisition":true,"dispatch_index_before_submit":true,"published_changes":changes.len(),"normal_live_close":ordering.borrow().clone().unwrap(),"cleanup_before_profile_retirement":true,"profile_retired":true,"broker_credential_retired_after_terminal":true,"receipt_operation":receipt.namespace.operation_kind})
}

fn external_campaign(
    root: &Path,
    view: &WorkspaceView,
    state: &Path,
    home: &DolgoraeHome,
    candidate: &crate::provisioning::ProvisioningCandidate,
) -> Value {
    let credential = root.join("external-controller");
    create_controller_credential(
        &credential,
        ControllerKind::Automation,
        "native-external".to_owned(),
        None,
        None,
    )
    .unwrap();
    let carrier = CredentialCarrier::open_path(&credential).unwrap();
    let binding = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
    let scope = delegation::DelegationScope {
        view,
        state_root: state,
        controller: &binding,
    };
    let port = SemanticRunSettlementPort { carrier: &carrier };
    let (engagement, _) =
        delegation::open_external_engagement(&scope, "native-open", None).unwrap();
    let engagement_id = Uuid::parse_str(&engagement.engagement_ref).unwrap();
    let hired = checked(
        root,
        "external hire",
        delegation::hire_external_specialist(
            &scope,
            &port,
            delegation::HireExternalSpecialist {
                engagement_id,
                provisioning_ref: &candidate.provisioning_ref,
                role_ref: "general",
                agent_configuration_digest: &candidate.agent_configuration_digest,
                idempotency_key: "native-hire",
                expected_revision: None,
            },
        ),
    );
    let member = Uuid::parse_str(&hired.member.member_ref).unwrap();
    let manifest = RunStore::new(SystemWorkspacePlatform, state)
        .load_manifest(member)
        .unwrap();
    assert_eq!(manifest.schema_version, 4);
    let (assigned, _) = checked(
        root,
        "external assign",
        delegation::assign_external_specialist_task(
            &scope,
            &caller_scope::fixture(32),
            &port,
            delegation::AssignExternalSpecialistTask {
                engagement_id,
                member_ref: member,
                task: task("native-external-writer"),
                execution_intent: "canonical_workspace_write",
                target: None,
                deadline_seconds: 600,
                idempotency_key: "native-external-write",
                expected_revision: None,
            },
        ),
    );
    let task_id = Uuid::parse_str(&assigned.task_ref).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let collected = loop {
        let got = delegation::collect_external_specialist_task(
            &scope,
            &port,
            delegation::CollectExternalSpecialistTask {
                engagement_id,
                member_ref: member,
                task_ref: task_id,
                adapter_id: "native-acceptance",
                adapter_version: "1",
            },
        )
        .unwrap();
        if got.task.state == "completed" {
            break got;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        collected.complete_result.as_ref().unwrap()["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "file.txt")
    );
    let acquired = mutate(view, &carrier, member, RunMutationOperation::AcquireWriter);
    assert_eq!(
        acquired.captured_effective_policy.verification,
        PolicyVerification::Verified
    );
    assert_eq!(acquired.captured_effective_policy.access, Access::Write);
    let ordering = std::rc::Rc::new(std::cell::RefCell::new(None));
    crate::semantic::set_member_release_before_retirement({
        let state = state.to_path_buf();
        let home = home.clone();
        let view = view.clone();
        let manifest = manifest.clone();
        let ordering = std::rc::Rc::clone(&ordering);
        move || {
            let closed = verify_closed(&state, &view, member);
            assert_eq!(membership(&home, &manifest), MembershipDisposition::Active);
            *ordering.borrow_mut() = Some(closed);
        }
    });
    let store = EngagementStore::open(&EngagementStore::workspace_database_path(state)).unwrap();
    let revision = crate::engagement::scope_membership_revision(
        store.connection(),
        &view.workspace_id,
        "external_engagement",
        &engagement_id.to_string(),
    )
    .unwrap();
    let released = checked(
        root,
        "external release",
        delegation::release_external_specialist(
            &scope,
            &port,
            delegation::ReleaseExternalSpecialist {
                engagement_id,
                member_ref: member,
                expected_revision: revision,
                allow_interrupt: false,
            },
        ),
    );
    assert_eq!(released.operation.progress, "terminal");
    assert_eq!(membership(home, &manifest), MembershipDisposition::Released);
    json!({"manifest":4,"normal_live_close":ordering.borrow().clone().unwrap(),"cleanup_before_profile_retirement":true,"profile_retired":true})
}

// All injected faults below are owned fixture inputs. Acquire, capture, rollback,
// dispatch classification and Reader release remain the production operations.
fn dispatch_failures(
    root: &Path,
    view: &WorkspaceView,
    state: &Path,
    session: Uuid,
    member: Uuid,
    scope: &delegation::DelegationScope<'_>,
    port: &SemanticRunSettlementPort<'_>,
) -> Vec<Value> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let workspace = view.canonical_path.to_path_buf().unwrap();
    let mut evidence = Vec::new();
    for mode in [
        "recording_rollback",
        "file_capacity",
        "baseline_capacity",
        "preparation_release_ok",
        "preparation_release_failed",
    ] {
        let before = Rc::new(RefCell::new(None));
        let credential_fault = Rc::new(RefCell::new(None));
        let turns = || {
            let (snapshot, _) = RunSnapshot::observe(state, member, 0).unwrap();
            snapshot
                .observed_ledger(state)
                .unwrap()
                .records()
                .iter()
                .filter(|r| r.kind() == crate::audit::AuditKind::TurnStarted)
                .count()
        };
        let before_turns = turns();
        let before_submits = crate::semantic::managed_submit_calls();
        crate::semantic::set_managed_dispatch_preparation({
            let state = state.to_path_buf();
            let view = view.clone();
            let workspace = workspace.clone();
            let before = Rc::clone(&before);
            let credential_fault = Rc::clone(&credential_fault);
            move || {
                let (snapshot, observed) = RunSnapshot::observe(&state, member, 0).unwrap();
                assert_eq!(
                    snapshot.writer.state,
                    crate::writer::WriterAuthorityState::Active
                );
                assert_eq!(observed.effective_policy.access, Access::Write);
                assert_eq!(
                    observed.effective_policy.verification,
                    PolicyVerification::Verified
                );
                let mut store =
                    EngagementStore::open(&EngagementStore::workspace_database_path(&state))
                        .unwrap();
                let (task,receipt):(String,String)=store.connection().query_row("SELECT json_extract(accepted_subjects_json,'$.task_ref'),receipt_json FROM delegation_operations WHERE operation_kind='assign_specialist_task' AND idempotency_key=?1",[mode],|row| Ok((row.get(0)?,row.get(1)?))).unwrap();
                assert_eq!(
                    store
                        .connection()
                        .query_row(
                            "SELECT COUNT(*) FROM dispatch_indexes WHERE task_id=?1",
                            [&task],
                            |r| r.get::<_, u64>(0)
                        )
                        .unwrap(),
                    0
                );
                match mode {
                    "file_capacity" => private(
                        &workspace.join("capacity.bin"),
                        &vec![b'x'; 8 * 1024 * 1024 + 1],
                    ),
                    "baseline_capacity" => {
                        // Valid historical baselines fill the real aggregate budget.
                        // Content is deduplicated, but each retained index counts.
                        private(
                            &workspace.join("baseline.bin"),
                            &vec![b'b'; 8 * 1024 * 1024],
                        );
                        for _ in 0..32 {
                            let id = Uuid::now_v7();
                            let capture =
                                crate::successor_capture::capture_dispatch_index(&workspace, id)
                                    .unwrap();
                            let retained:u64=store.connection().query_row("SELECT COALESCE(SUM(baseline_bytes),0) FROM dispatch_indexes WHERE run_ref=?1",[member.to_string()],|r|r.get(0)).unwrap();
                            if retained + capture.baseline_bytes > 268435456 {
                                break;
                            }
                            store
                                .transact(|tx| {
                                    crate::engagement::record_dispatch_index_with_content(
                                        tx,
                                        crate::engagement::DispatchIndexRecord {
                                            task_id: id,
                                            workspace_ref: &view.workspace_id,
                                            run_ref: member,
                                            head_commit: capture.head.as_deref(),
                                            index_sha256: &capture.sha256,
                                            index_byte_length: capture.canonical.len() as u64,
                                            file_count: capture.file_count,
                                            baseline_bytes: capture.baseline_bytes,
                                        },
                                        &capture.canonical,
                                        &capture.baseline,
                                        1,
                                    )
                                })
                                .unwrap();
                        }
                    }
                    "recording_rollback" => {
                        private(&workspace.join("rollback.txt"), b"new rollback content\n");
                        store.connection().execute_batch("CREATE TRIGGER native_recording_failure BEFORE INSERT ON dispatch_indexes BEGIN SELECT RAISE(ABORT,'injected recording failure'); END;").unwrap();
                    }
                    "preparation_release_ok" | "preparation_release_failed" => {
                        std::os::unix::fs::symlink(
                            workspace.join("file.txt"),
                            workspace.join("unsafe-link"),
                        )
                        .unwrap();
                        if mode == "preparation_release_failed" {
                            let path = state
                                .join("orchestration/broker-credentials")
                                .join(format!("{member}.json"));
                            let original = fs::read(&path).unwrap();
                            let mut wire: Value = serde_json::from_slice(&original).unwrap();
                            let mut capability =
                                wire["capability"].as_str().unwrap().as_bytes().to_vec();
                            capability[0] = if capability[0] == b'A' { b'B' } else { b'A' };
                            wire["capability"] = json!(String::from_utf8(capability).unwrap());
                            private(&path, &serde_json::to_vec(&wire).unwrap());
                            *credential_fault.borrow_mut() = Some((path, original));
                        }
                    }
                    _ => unreachable!(),
                }
                let contents: u64 = store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM result_content", [], |r| r.get(0))
                    .unwrap();
                let refs: u64 = store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM result_content_refs", [], |r| r.get(0))
                    .unwrap();
                let guard = crate::writer::WriterStore::new(
                    &state,
                    &view.workspace_id,
                    DarwinSystem.current_uid(),
                )
                .load()
                .unwrap();
                *before.borrow_mut() = Some((task, receipt, contents, refs, guard));
            }
        });
        let error = delegation::assign_specialist_task(
            scope,
            &context(session, mode),
            &caller_scope::fixture(32),
            port,
            delegation::AssignSpecialistTask {
                member_ref: member,
                task: task(mode),
                execution_intent: "canonical_workspace_write",
                target: None,
                deadline_seconds: 600,
                expected_revision: None,
            },
        )
        .unwrap_err();
        if mode != "preparation_release_failed" {
            assert_eq!(
                error.code,
                match mode {
                    "recording_rollback" => "INTERNAL_ERROR",
                    "file_capacity" => "REVIEW_TARGET_LIMIT_EXCEEDED",
                    "baseline_capacity" => "STORAGE_CAPACITY_EXCEEDED",
                    "preparation_release_ok" => "REVIEW_TARGET_UNSAFE_FILE",
                    _ => unreachable!(),
                }
            );
        }
        let (task_id, receipt, contents, refs, original_guard) = before
            .borrow_mut()
            .take()
            .expect("qualified acquire reached preparation");
        assert_eq!(turns(), before_turns, "Submit must not start a Turn");
        assert_eq!(
            crate::semantic::managed_submit_calls(),
            before_submits,
            "Submit must not be called"
        );
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(state)).unwrap();
        let current:String=store.connection().query_row("SELECT receipt_json FROM delegation_operations WHERE operation_kind='assign_specialist_task' AND idempotency_key=?1",[mode],|r|r.get(0)).unwrap();
        assert_eq!(current, receipt);
        for (table, expected) in [("result_content", contents), ("result_content_refs", refs)] {
            let actual: u64 = store
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(actual, expected, "{mode}: {table}");
        }
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_indexes WHERE task_id=?1",
                    [&task_id],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            0
        );
        let accounting: String = store
            .connection()
            .query_row(
                "SELECT accounting_state FROM caller_scope_assignments WHERE assignment_id=?1",
                [&task_id],
                |r| r.get(0),
            )
            .unwrap();
        let writer =
            crate::writer::WriterStore::new(state, &view.workspace_id, DarwinSystem.current_uid())
                .load()
                .unwrap();
        let unresolved = mode == "preparation_release_failed";
        assert_eq!(
            accounting,
            if unresolved {
                "counted_unresolved"
            } else {
                "proved_nondispatch"
            }
        );
        if unresolved {
            assert_eq!(
                writer, original_guard,
                "release refusal must preserve the exact acquired guard"
            );
        } else {
            assert_eq!(writer.state, crate::writer::WriterAuthorityState::None);
        }
        evidence.push(json!({"case":mode,"error":error.code,"qualified_acquire":true,"submit_calls":0,"index_and_content_rolled_back":true,"receipt_unchanged":true,"accounting":accounting,"guard_preserved":unresolved}));
        match mode {
            "file_capacity" => fs::remove_file(workspace.join("capacity.bin")).unwrap(),
            "baseline_capacity" => fs::remove_file(workspace.join("baseline.bin")).unwrap(),
            "recording_rollback" => {
                store
                    .connection()
                    .execute_batch("DROP TRIGGER native_recording_failure")
                    .unwrap();
                fs::remove_file(workspace.join("rollback.txt")).unwrap();
            }
            _ => fs::remove_file(workspace.join("unsafe-link")).unwrap(),
        }
        if let Some((path, original)) = credential_fault.borrow_mut().take() {
            private(&path, &original);
            // Teardown after the uncertain-state assertions: this fixture knows
            // no Submit occurred and restores the exact original carrier bytes.
            store
                .transact(|tx| {
                    crate::orchestration::terminalize_successor_task(
                        tx,
                        session,
                        Uuid::parse_str(&task_id).unwrap(),
                        "TURN_FAILED",
                        1,
                    )?;
                    crate::caller_scope::proved_nondispatch(tx, Uuid::parse_str(&task_id).unwrap())
                })
                .unwrap();
            let broker = crate::orchestration::OrchestrationStore::open(state)
                .unwrap()
                .member_credential(member)
                .unwrap();
            mutate(
                view,
                &broker.controller_carrier().unwrap(),
                member,
                RunMutationOperation::ReleaseWriter,
            );
        }
    }
    private(&root.join("dispatch-failures-checked"), b"complete\n");
    private(
        &root.join("dispatch-failures.json"),
        &serde_json::to_vec(&evidence).unwrap(),
    );
    evidence
}

pub fn cleanup(root: &Path) -> bool {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    let home = DolgoraeHome::system().unwrap();
    if let Ok(workspaces) = fs::read_dir(home.root().join("workspaces")) {
        for state in workspaces.flatten().map(|entry| entry.path()) {
            let runs = RunStore::new(SystemWorkspacePlatform, &state);
            let Ok(entries) = fs::read_dir(state.join("runs")) else {
                continue;
            };
            let mut ids = entries
                .flatten()
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .and_then(|name| Uuid::parse_str(name).ok())
                })
                .collect::<Vec<_>>();
            ids.sort_by_key(|id| {
                runs.load_manifest(*id)
                    .ok()
                    .and_then(|m| {
                        m.aggregate_binding
                            .map(|b| b.member_kind == crate::run::AggregateMemberKind::Primary)
                    })
                    .unwrap_or(false)
            });
            for id in ids {
                let Ok(manifest) = runs.load_manifest(id) else {
                    continue;
                };
                if runs
                    .load_state_projection(id)
                    .is_ok_and(|state| state.lifecycle == RunLifecycle::Closed)
                {
                    continue;
                }
                let credential = if manifest.aggregate_binding.as_ref().is_some_and(|binding| {
                    binding.member_kind == crate::run::AggregateMemberKind::Specialist
                        && binding.aggregate_kind
                            == crate::domain::AggregateKind::OrchestratedSession
                }) {
                    crate::orchestration::BrokerCredential::load(
                        &state.join("orchestration/broker-credentials"),
                        DarwinSystem.current_uid(),
                        id,
                    )
                    .and_then(|credential| credential.controller_carrier())
                } else {
                    CredentialCarrier::open_path(&root.join(
                        if manifest.aggregate_binding.as_ref().is_some_and(|binding| {
                            binding.aggregate_kind
                                == crate::domain::AggregateKind::ExternalSpecialistEngagement
                        }) {
                            "external-controller"
                        } else {
                            "controller"
                        },
                    ))
                };
                if let Ok(carrier) = credential {
                    let view = WorkspaceView {
                        workspace_id: manifest.workspace_id.clone(),
                        canonical_path: manifest.canonical_workspace.clone(),
                        mode: manifest.workspace_mode,
                        created: false,
                    };
                    let _ = CoreSemanticService.mutate_caller_run(RunMutationInput {
                        workspace: view.canonical_path.to_path_buf().unwrap(),
                        expected_workspace_id: view.workspace_id,
                        run_id: id,
                        expected_state_revision: None,
                        carrier,
                        operation: RunMutationOperation::Close { interrupt: true },
                    });
                }
            }
        }
    }
    if GlobalProfileStore::new(&home).resolve(PROFILE).is_err() {
        return true;
    }
    let status = match execute_global(ProfileOperation::ServerStatus, &[OsString::from(PROFILE)]) {
        Ok(status) => status,
        Err(_) => return false,
    };
    let Some(key) = status["server_key"].as_str() else {
        return false;
    };
    execute_global(
        ProfileOperation::ServerStop,
        &[
            OsString::from(PROFILE),
            OsString::from("--operator-file"),
            root.join("operator").as_os_str().to_owned(),
            OsString::from("--interrupt"),
            OsString::from("--confirm-server-key"),
            OsString::from(key),
        ],
    )
    .is_ok()
}
