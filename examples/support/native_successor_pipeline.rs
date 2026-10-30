//! Real native managed and external successor write/close evidence.
use crate::caller_scope;
use crate::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use crate::darwin::DarwinSystem;
use crate::delegation_service as delegation;
use crate::domain::{Access, ControllerKind, PolicyVerification, RunLifecycle};
use crate::engagement::EngagementStore;
use crate::global_profile::GlobalProfileStore;
use crate::global_runtime::{GlobalMembershipStore, MembershipDisposition, ResolvedGlobalProfile};
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource, SelectedNamedSource};
use crate::paths::DolgoraeHome;
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
    use crate::audit::AuditKind as K;
    let (snapshot, observed) = RunSnapshot::observe(root, run, 0).unwrap();
    assert_eq!(snapshot.projection.lifecycle, RunLifecycle::Closed);
    assert_eq!(observed.effective_policy.access, Access::Read);
    assert_eq!(
        observed.effective_policy.verification,
        PolicyVerification::Verified
    );
    assert!(observed.effective_policy.writer_generation.is_none());
    assert_eq!(
        snapshot.writer.state,
        crate::writer::WriterAuthorityState::None
    );
    assert!(crate::run::is_caller_workspace_generation(
        snapshot.manifest.schema_version
    ));
    assert_eq!(
        snapshot.manifest.execution_lane,
        crate::domain::ExecutionLane::Dedicated
    );
    assert_eq!(snapshot.manifest.workspace_id, workspace.workspace_id);
    let binding = snapshot.manifest.global_profile_binding.as_ref().unwrap();
    assert!(binding.caller_workspace_supported());
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
    let writer =
        crate::writer::WriterStore::new(root, &workspace.workspace_id, DarwinSystem.current_uid())
            .load()
            .unwrap();
    assert_eq!(writer.state, crate::writer::WriterAuthorityState::None);
    let release = writer
        .generation_release
        .as_ref()
        .expect("qualified committed release missing");
    assert_eq!(
        release.phase,
        crate::writer::GenerationReleasePhase::Committed
    );
    assert_eq!(release.source.run_id, run);
    assert_eq!(release.writer_generation, writer.writer_generation);
    assert_eq!(release.source.profile_server_key, binding.server_key);
    let reservation = release.successor.as_ref().unwrap();
    assert!(reservation.attempt_started);
    assert_eq!(
        reservation.run_generation,
        snapshot.projection.run_generation
    );
    assert!(reservation.run_generation > release.source.run_generation);
    assert_eq!(
        observed.effective_policy.server_epoch,
        Some(reservation.server_epoch)
    );
    let thread_id = snapshot.projection.thread_id.as_deref().unwrap();
    assert_eq!(release.source.thread_id.as_deref(), Some(thread_id));
    let initial_operation = snapshot
        .manifest
        .aggregate_binding
        .as_ref()
        .and_then(|binding| {
            (binding.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                && binding.member_kind == crate::run::AggregateMemberKind::Specialist)
                .then_some(binding.operation_id)
        })
        .unwrap_or(run)
        .to_string();
    let ledger = snapshot.observed_ledger(root).unwrap();
    let records = ledger.records();
    let initial = records
        .iter()
        .find(|r| {
            r.kind() == K::TurnIntent
                && payload(r)["schema"] == "dolgorae.caller-native-intent/v1"
                && payload(r)["method"] == "thread/start"
                && payload(r)["operation_id"] == initial_operation
        })
        .expect("original native Thread intent missing");
    let provisional = records
        .iter()
        .find(|r| {
            r.kind() == K::TurnIntent
                && payload(r)["operation_id"] == initial_operation
                && payload(r)["provisional_thread_id"] == thread_id
        })
        .expect("original provisional Thread missing");
    let initial_bound = records
        .iter()
        .find(|r| {
            r.kind() == K::ThreadBound
                && payload(r)["operation_id"] == initial_operation
                && payload(r)["thread_id"] == thread_id
        })
        .expect("original Thread binding missing");
    let acquired = records
        .iter()
        .rev()
        .find(|r| r.kind() == K::WriterAcquired)
        .unwrap();
    let acquisition = payload(acquired);
    assert_eq!(acquired.run_generation(), release.source.run_generation);
    assert_eq!(
        acquisition["writer_generation"].as_u64(),
        Some(release.writer_generation)
    );
    assert_eq!(
        acquisition["effective_policy"]["writer_generation"].as_u64(),
        Some(release.writer_generation)
    );
    assert_eq!(acquisition["effective_policy"]["access"], "write");
    assert_eq!(acquisition["effective_policy"]["verification"], "verified");
    assert!(release.source_revision >= acquired.sequence());
    let reader = records
        .iter()
        .find(|r| {
            if r.kind() != K::ThreadBound || r.run_generation() != reservation.run_generation {
                return false;
            }
            let value = payload(r);
            value["operation_id"] == release.transaction_id.to_string()
                && value["thread_id"] == thread_id
                && value["thread_generation"].as_u64()
                    == observed.effective_policy.thread_generation
                && value["effective_policy"]
                    == serde_json::to_value(&observed.effective_policy).unwrap()
        })
        .expect("exact released Reader binding missing");
    let seal = records.last().unwrap();
    assert_eq!(seal.kind(), K::LifecycleTransition);
    assert_eq!(payload(seal)["terminal_seal"], true);
    assert_eq!(payload(seal)["current"], "closed");
    let cleanup = &records[records.len() - 2];
    assert_eq!(cleanup.kind(), K::CleanupResult);
    assert_eq!(cleanup.run_generation(), reservation.run_generation);
    assert_eq!(seal.run_generation(), reservation.run_generation);
    assert!(initial.sequence() < provisional.sequence());
    assert!(provisional.sequence() < initial_bound.sequence());
    assert!(initial_bound.sequence() < acquired.sequence());
    assert!(release.source_revision < reader.sequence());
    assert!(reader.sequence() < cleanup.sequence());
    assert!(cleanup.sequence() < seal.sequence());
    json!({"writer_acquired_sequence":acquired.sequence(),"verified_reader_sequence":reader.sequence(),
        "cleanup_sequence":cleanup.sequence(),"seal_sequence":seal.sequence(),"sealed_policy":"read",
        "writer_released":true,"owned_generation_absence_verified":true})
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

fn consumer_task(
    root: &Path,
    marker: &str,
    formal: bool,
) -> crate::task_request::SpecialistTaskRequest {
    let mut request = task(marker);
    let dispatch = if formal {
        "formal-dispatch.txt"
    } else {
        "ordinary-dispatch.txt"
    };
    request.contexts.push(crate::task_request::TaskContext {
        id: "aquarium-assignment".into(),
        content: fs::read_to_string(root.join(dispatch)).unwrap(),
        provenance: "caller".into(),
    });
    if formal {
        let package: Value =
            serde_json::from_slice(&fs::read(root.join("package.json")).unwrap()).unwrap();
        request.contexts.push(crate::task_request::TaskContext {
            id: "delegated-package".into(),
            content: serde_json::to_string(&package["result"]["package"]).unwrap(),
            provenance: "caller".into(),
        });
    }
    private(
        &root.join(if formal {
            "formal-task.json"
        } else {
            "ordinary-task.json"
        }),
        &serde_json::to_vec(
            &json!({"task":request,"execution_intent":"canonical_workspace_write",
            "deadline_seconds":600}),
        )
        .unwrap(),
    );
    request
}

fn managed_consumer_setup(
    root: &Path,
    view: &WorkspaceView,
    primary: Uuid,
    binding: &crate::run::ControllerBinding,
    candidate: &crate::provisioning::ProvisioningCandidate,
    codex: &Path,
) -> crate::trusted_host::VerifiedOwner {
    let output = std::process::Command::new("/usr/bin/git")
        .args([
            "-C",
            view.canonical_path.to_path_buf().unwrap().to_str().unwrap(),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    private(
        &root.join("managed-consumer-setup.json"),
        &serde_json::to_vec(&json!({
            "candidate":candidate,"adapter_version":env!("CARGO_PKG_VERSION"),
            "base_commit":String::from_utf8(output.stdout).unwrap().trim(),"root_run":primary
        }))
        .unwrap(),
    );
    let inputs: Value =
        serde_json::from_slice(&fs::read(root.join("consumer-inputs.json")).unwrap()).unwrap();
    let prepared = std::process::Command::new(inputs["python"].as_str().unwrap())
        .arg("-c")
        .arg("import sys; from pathlib import Path; sys.dont_write_bytecode=True; sys.path.insert(0,sys.argv[1]); import run_native_integrated_acceptance as m; m.prepare_successor_consumers(Path(sys.argv[2]))")
        .arg(inputs["scripts"].as_str().unwrap()).arg(root).output().unwrap();
    private(&root.join("consumer-setup.stdout"), &prepared.stdout);
    private(&root.join("consumer-setup.stderr"), &prepared.stderr);
    assert!(prepared.status.success(), "selected consumer setup failed");
    let connection: crate::native_consumer::PodwayConnection =
        serde_json::from_slice(&fs::read(root.join("connection.json")).unwrap()).unwrap();
    // Protected host state must be disjoint from the caller workspace.
    let host_root = root.join("managed-native-host");
    fs::create_dir(&host_root).unwrap();
    fs::set_permissions(&host_root, fs::Permissions::from_mode(0o700)).unwrap();
    private(
        &host_root.join("controller"),
        &fs::read(root.join("controller")).unwrap(),
    );
    let journal = host_root.join("journal");
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    let draft = json!({"route":"codex-0.160.0","executable":{"path":codex,"version":"0.164.0","sha256":"d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf"},
        "owner":{"kind":"managed","session_ref":primary},"controller_generation":binding.identity.generation,
        "controller_file":host_root.join("controller"),"native_host_root":journal,
        "allowed_scopes":[{"kind":"orchestrated_session","reference":primary}],
        "tools":["dolgorae_specialists_native","dolgorae_specialist_consumer"],
        "caller_scope":{"binding_ref":"local-specialist","podway_workspace_ref":connection.workspace,"podway_session_ref":connection.session,"podway_attempt_ref":connection.attempt,"delegated_placement_ref":connection.placement,"max_rounds":3},
        "adapter_id":"dolgorae.managed/1","adapter_version":env!("CARGO_PKG_VERSION")});
    private(
        &host_root.join("draft.json"),
        &serde_json::to_vec(&draft).unwrap(),
    );
    let operator = CredentialCarrier::open_path(&root.join("operator")).unwrap();
    let controller = CredentialCarrier::open_path(&root.join("controller")).unwrap();
    let owner_path = host_root.join("owner.json");
    for operation in [
        "owner-config-create",
        "namespace-setup",
        "host-state-init",
        "preparation-init",
        "managed-connect",
    ] {
        let mut args: Vec<OsString> = [
            operation.to_owned(),
            "--owner-config".into(),
            owner_path.to_str().unwrap().into(),
            "--operator-fd".into(),
            operator.raw_fd().to_string(),
            "--controller-fd".into(),
            controller.raw_fd().to_string(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        if operation == "owner-config-create" {
            args.extend([
                OsString::from("--workspace"),
                view.canonical_path.to_path_buf().unwrap().into_os_string(),
                OsString::from("--request"),
                host_root.join("draft.json").into_os_string(),
            ]);
        }
        checked(root, operation, crate::trusted_host::run_admin(&args));
    }
    let owner =
        crate::trusted_host::VerifiedOwner::load(&DolgoraeHome::system().unwrap(), &owner_path)
            .unwrap();
    crate::native_consumer::initialize(
        &owner,
        crate::native_consumer::Mode::Delegated {
            connection: Box::new(connection),
        },
    )
    .unwrap();
    owner
}
// Only local fault controls and owning-service observation use this test context.
// The successful managed path obtains provenance from actual Primary Turns.
fn context<'a>(session: Uuid, key: &'a str) -> delegation::ManagedCallContext<'a> {
    delegation::ManagedCallContext {
        session_id: session,
        source_run_id: session,
        source_turn_id: "native-host-turn",
        source_tool_call_id: "native-host-tool",
        idempotency_key: key,
    }
}

fn primary_turn(
    root: &Path,
    view: &WorkspaceView,
    primary: Uuid,
    phase: &str,
    input: Value,
) -> Value {
    private(
        &root.join("primary-phase.json"),
        &serde_json::to_vec(&json!({
            "phase":phase,"root_run":primary,"input":input
        }))
        .unwrap(),
    );
    let message = format!(
        "native-primary-{phase}\nApply this selected formal contract to delivery only; no goal acceptance is authorized.\n{}",
        fs::read_to_string(
            root.join("aquarium-source/references/formal-agent-orchestration-v1.md")
        )
        .unwrap()
    );
    let args: Vec<OsString> = vec![
        primary.to_string(),
        "--workspace".into(),
        view.canonical_path
            .to_path_buf()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        "--controller-file".into(),
        root.join("controller").to_str().unwrap().into(),
        "--message".into(),
        message,
        "--idempotency-key".into(),
        format!("native-primary-{phase}"),
        "--timeout".into(),
        "60s".into(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let result = checked(
        root,
        phase,
        CoreSemanticService.execute(&SemanticCommand::Run {
            verb: crate::semantic::RunVerb::Send,
            args,
        }),
    );
    private(
        &root.join(format!("primary-{phase}-turn.json")),
        &serde_json::to_vec(&result).unwrap(),
    );
    serde_json::from_slice(&fs::read(root.join(format!("primary-{phase}-results.json"))).unwrap())
        .unwrap()
}

fn consumer_finish(root: &Path) {
    let inputs: Value =
        serde_json::from_slice(&fs::read(root.join("consumer-inputs.json")).unwrap()).unwrap();
    let output = std::process::Command::new(inputs["python"].as_str().unwrap()).arg("-c")
        .arg("import sys; from pathlib import Path; sys.dont_write_bytecode=True; sys.path.insert(0,sys.argv[1]); import run_native_integrated_acceptance as m; m.finish_successor_consumer(Path(sys.argv[2]))")
        .arg(inputs["scripts"].as_str().unwrap()).arg(root).output().unwrap();
    private(&root.join("consumer-finish.stdout"), &output.stdout);
    private(&root.join("consumer-finish.stderr"), &output.stderr);
    assert!(
        output.status.success(),
        "separate selected Action completion failed"
    );
}

/// Qualify the ordinary physical birth that optional peer-tool discovery must
/// not invalidate. The source context is prepared service data, not model proof.
fn successor_startup_component(
    root: &Path,
    scope: &delegation::DelegationScope<'_>,
    primary: Uuid,
    candidate: &crate::provisioning::ProvisioningCandidate,
    owner: &CredentialCarrier,
    home: &DolgoraeHome,
) -> Value {
    let state = scope.state_root;
    let session = crate::orchestration::OrchestrationStore::open_observer(state)
        .unwrap()
        .session(primary)
        .unwrap();
    assert!(session.specialist_policy.role("reviewer").is_ok());
    assert!(session.specialist_policy.role("general").is_err());
    let primary_runtime = RunSnapshot::load(state, primary, 0)
        .unwrap()
        .runtime_record
        .unwrap();
    let port = SemanticRunSettlementPort { carrier: owner };
    let admitted = checked(
        root,
        "successor startup",
        delegation::spawn_specialist(
            scope,
            &context(primary, "native-startup-component"),
            &port,
            delegation::SpawnSpecialist {
                provisioning_ref: &candidate.provisioning_ref,
                role_ref: "general",
                agent_configuration_digest: &candidate.agent_configuration_digest,
                expected_revision: None,
            },
        ),
    );
    let run = Uuid::parse_str(&admitted.member.member_ref).unwrap();
    assert_eq!(admitted.member.role_ref, "general");
    let (snapshot, observed) = RunSnapshot::observe(state, run, 0).unwrap();
    assert_eq!(snapshot.manifest.schema_version, 4);
    assert_eq!(snapshot.projection.lifecycle, RunLifecycle::Idle);
    assert!(snapshot.projection.active_turn_id.is_none());
    assert_eq!(observed.effective_policy.access, Access::Read);
    assert_eq!(
        observed.effective_policy.verification,
        PolicyVerification::Verified
    );
    let retained =
        crate::provisioning::retained_snapshot(state, &candidate.provisioning_ref).unwrap();
    assert_eq!(
        snapshot.manifest.agent_configuration,
        retained.admitted_configuration().unwrap().1
    );
    assert_eq!(
        snapshot.manifest.agent_configuration.native_subagent_policy,
        "enabled"
    );
    let binding = snapshot.manifest.aggregate_binding.as_ref().unwrap();
    assert_eq!(binding.aggregate_id, primary);
    assert_eq!(
        binding.member_kind,
        crate::run::AggregateMemberKind::Specialist
    );
    assert!(
        crate::worker::session_tool_contract(state, &snapshot.manifest)
            .unwrap()
            .is_none()
    );
    let runtime = snapshot.runtime_record.clone().unwrap();
    assert_eq!(
        crate::worker::classify_worker_identity(&runtime),
        crate::worker::ProcessIdentityVerdict::Match
    );
    let response =
        crate::worker::call_run_worker(state, run, DarwinSystem.current_uid(), None, |expected| {
            crate::worker::ControlRequestV1::RunStatus {
                expected,
                caller: None,
            }
        })
        .unwrap();
    assert!(
        matches!(response,crate::worker::ControlResponseV1::Status{identity,lifecycle,active_turn:None,..} if identity==runtime.identity && lifecycle=="idle")
    );
    let thread = snapshot.projection.thread_id.as_deref().unwrap();
    crate::worker::read_thread_registration(state, &snapshot.manifest, thread).unwrap();
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state)).unwrap();
    assert!(
        matches!(store.intent_lookup(&admitted.receipt.namespace).unwrap(),crate::engagement::IntentLookup::Accepted(ref receipt) if receipt.as_ref()==&admitted.receipt)
    );
    let member = crate::orchestration::successor_member(store.connection(), primary, run)
        .unwrap()
        .unwrap();
    assert_eq!(member.membership_state, "active");
    assert_eq!(member.actor_residency, "resident");
    drop(store);
    let records = snapshot.observed_ledger(state).unwrap();
    assert!(
        !records
            .records()
            .iter()
            .any(|record| matches!(record.kind(), crate::audit::AuditKind::TurnStarted))
    );
    drop(records);
    private(
        &root.join("startup-owned-runtime.json"),
        &serde_json::to_vec(&json!({"primary":primary_runtime,"successor":runtime})).unwrap(),
    );
    private(
        &root.join("startup-receipt.json"),
        &serde_json::to_vec(&admitted.receipt).unwrap(),
    );
    assert!(
        cleanup(root),
        "owning service release and exact Profile retirement failed"
    );
    crate::worker::prove_worker_generation_absent(&runtime).unwrap();
    crate::worker::prove_worker_generation_absent(&primary_runtime).unwrap();
    let absence = crate::worker::verified_absence_marker(&crate::worker::runtime_root(state), run)
        .unwrap()
        .unwrap();
    assert_eq!(absence.run_id, run);
    assert_eq!(absence.run_generation, runtime.identity.run_generation);
    assert_eq!(
        RunSnapshot::load(state, run, 0)
            .unwrap()
            .projection
            .lifecycle,
        RunLifecycle::Closed
    );
    assert!(home.root().starts_with(root));
    json!({"schema":"dolgorae.native-successor-startup-component/v1","passed":true,"actual_worker_ready":true,
        "actual_thread_bound":true,"effective_read_only_verified":true,"optional_collaboration_tool_absent":true,
        "prepared_service_context":true,"actual_primary_model_tool_call":false,"model_turns":0,
        "accepted_configuration_retained":true,"accepted_receipt_verified":true,"member_run":run,"thread_id":thread,
        "worker_generation":runtime.identity.run_generation,"owned_generation_absence_verified":true})
}

// Preserve first production errors before the service's bounded projection.
struct ObservedStart<'a> {
    inner: &'a dyn delegation::RunSettlementPort,
    root: &'a Path,
}
impl delegation::RunSettlementPort for ObservedStart<'_> {
    fn start_member_run(
        &self,
        r: delegation::AdmittedMemberStart<'_>,
    ) -> Result<delegation::MemberRunStart, crate::machine::MachineError> {
        let result = self.inner.start_member_run(r);
        if let Err(ref error) = result {
            private(
                &self.root.join("member-start-error.json"),
                &serde_json::to_vec(error).unwrap(),
            );
        }
        result
    }
    fn dispatch_member_task(
        &self,
        r: delegation::MemberTaskDispatch<'_>,
    ) -> Result<delegation::MemberDispatchOutcome, crate::machine::MachineError> {
        self.inner.dispatch_member_task(r)
    }
    fn dispatch_managed_task(
        &self,
        r: delegation::ManagedTaskDispatch<'_>,
    ) -> Result<delegation::MemberDispatchOutcome, crate::machine::MachineError> {
        self.inner.dispatch_managed_task(r)
    }
    fn settle_member_task(
        &self,
        r: delegation::MemberTaskSettlement<'_>,
    ) -> Result<delegation::MemberSettlement, crate::machine::MachineError> {
        self.inner.settle_member_task(r)
    }
    fn settle_member_release(
        &self,
        r: delegation::MemberReleaseRequest<'_>,
    ) -> Result<delegation::MemberReleaseOutcome, delegation::MemberReleaseFailure> {
        self.inner.settle_member_release(r)
    }
    fn member_writer_observation(
        &self,
        r: delegation::MemberCleanupObservation<'_>,
    ) -> Result<delegation::MemberWriterObservation, crate::machine::MachineError> {
        let result = self.inner.member_writer_observation(r);
        let path = self.root.join("member-writer-observation-error.json");
        if let Err(ref error) = result
            && !path.exists()
        {
            private(&path, &serde_json::to_vec(error).unwrap());
        }
        result
    }
    fn member_run_observation(
        &self,
        r: delegation::MemberCleanupObservation<'_>,
        budget: &mut crate::ledger::ObservationBudget,
    ) -> Result<crate::snapshot::MemberRunObservation, crate::machine::MachineError> {
        let result = self.inner.member_run_observation(r, budget);
        let path = self.root.join("member-run-observation-error.json");
        if let Err(ref error) = result
            && !path.exists()
        {
            private(&path, &serde_json::to_vec(error).unwrap());
        }
        result
    }
    fn member_interaction_observation(
        &self,
        r: delegation::MemberCleanupObservation<'_>,
        source: &crate::snapshot::MemberRunObservation,
        budget: &mut crate::ledger::ObservationBudget,
    ) -> Result<(bool, crate::domain::ProjectionStamp), crate::machine::MachineError> {
        let result = self.inner.member_interaction_observation(r, source, budget);
        let path = self.root.join("member-interaction-observation-error.json");
        if let Err(ref error) = result
            && !path.exists()
        {
            private(&path, &serde_json::to_vec(error).unwrap());
        }
        result
    }
}

pub fn scheduler_component(root: &Path, codex: &Path) -> Value {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    let correction_only = root.join("scheduler-correction-phase").exists();
    if correction_only {
        assert_eq!(
            fs::read(root.join("scheduler-correction-phase")).unwrap(),
            b"dolgorae.scheduler-correction-phase/v1\n"
        );
    }
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
    private(
        &workspace.join(".git/info/exclude"),
        b"/.podway/\n/.dolgorae/\n",
    );
    let home = DolgoraeHome::system().unwrap();
    private(
        &home.root().join("home/AGENTS.md"),
        b"integrated-common-017\n",
    );
    private(
        &home.root().join("roles/general/AGENTS.md"),
        b"integrated-general-017\nExecute only the accepted bounded task.\n",
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
        .prepare_qualified(&home)
        .unwrap();
    let operator = root.join("operator");
    OperatorStore::new(home.operator_root())
        .initialize(&operator)
        .unwrap();
    assert!(binding.caller_workspace_supported());
    binding.record_qualified(&home).unwrap();
    private(
        &root.join("qualified-binding.json"),
        &serde_json::to_vec(&binding).unwrap(),
    );
    crate::profile::ensure_qualified_global_server(&binding).unwrap();
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
    start["model"] = json!("gpt-5.6-luna");
    start["effort"] = json!("low");
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
        trusted_invocation: None,
    };
    crate::engagement::ensure_delegation_namespace(&state_root).unwrap();
    let source_store = InstructionSourceStore::new(&home);
    assert!(binding.caller_workspace_supported());
    assert_eq!(
        binding.launch_snapshot.process_static_configuration["features"]["shell_snapshot"],
        json!(false)
    );
    // The disabled successor launch has its own captured account-home lifetime;
    // it cannot replace the enabled Primary server bound to the first Profile.
    GlobalProfileStore::new(&home)
        .add(
            "scheduler-members".into(),
            RuntimeProfile {
                argv: vec![codex.to_str().unwrap().into()],
                codex_home: root.join("profile-members").to_str().unwrap().into(),
                environment: BTreeMap::from([
                    ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
                    ("LANG".into(), "C".into()),
                    ("LC_ALL".into(), "C".into()),
                ]),
                native_subagents: NativeSubagents::Enabled,
            },
        )
        .unwrap();
    let member_binding = ResolvedGlobalProfile::resolve(&home, "scheduler-members")
        .unwrap()
        .prepare_specialist(&home, false)
        .unwrap();
    member_binding.record_qualified(&home).unwrap();
    private(
        &root.join("qualified-member-binding.json"),
        &serde_json::to_vec(&member_binding).unwrap(),
    );
    let mut sources = ProvisioningSources {
        instruct: &source_store,
        role_name: "general",
        role: SelectedInstructionSource::File(vec!["roles".to_owned(), "general".to_owned()]),
        dependencies: vec![SelectedNamedSource {
            name: "formal-agent-orchestration-v1".into(),
            source: SelectedInstructionSource::Inline(
                "Bounded scheduler component qualification.".into(),
            ),
        }],
        selected_guidance: Vec::new(),
        profile_binding: member_binding,
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

    // Component preparation uses the already authenticated service seam. It
    // does not claim Primary model-tool provenance or live policy enablement.
    let port = SemanticRunSettlementPort { carrier: &owner };
    let start_port = ObservedStart { inner: &port, root };
    let spawned = checked(
        root,
        "scheduler managed spawn",
        delegation::spawn_specialist(
            &scope,
            &context(primary, "scheduler-spawn"),
            &start_port,
            delegation::SpawnSpecialist {
                provisioning_ref: &candidate.provisioning_ref,
                role_ref: "general",
                agent_configuration_digest: &candidate.agent_configuration_digest,
                expected_revision: None,
            },
        ),
    );
    let managed = Uuid::parse_str(&spawned.member.member_ref).unwrap();
    // Physical canonical-member birth owns a Writer. Prepare an idle Reader
    // through its actual Broker before admitting another canonical member;
    // queued dispatch must later perform its own fenced Writer acquisition.
    let managed_owner = crate::orchestration::OrchestrationStore::open_observer(&state_root)
        .unwrap()
        .member_credential(managed)
        .unwrap()
        .controller_carrier()
        .unwrap();
    mutate(
        &view,
        &managed_owner,
        managed,
        RunMutationOperation::ReleaseWriter,
    );
    let (_, managed_reader) = RunSnapshot::observe(&state_root, managed, 0).unwrap();
    assert_eq!(managed_reader.effective_policy.access, Access::Read);
    assert_eq!(
        managed_reader.effective_policy.verification,
        PolicyVerification::Verified
    );
    let managed_assignment = (!correction_only).then(|| {
        checked(
            root,
            "scheduler managed queue",
            delegation::queue_specialist_task(
                &scope,
                &context(primary, "scheduler-managed-write"),
                &caller_scope::fixture(32),
                &port,
                delegation::AssignSpecialistTask {
                    member_ref: managed,
                    task: task("scheduler-managed-write"),
                    execution_intent: "canonical_workspace_write",
                    target: None,
                    deadline_seconds: 600,
                    expected_revision: None,
                },
                1,
            ),
        )
    });
    let external_path = root.join("external-controller");
    create_controller_credential(
        &external_path,
        ControllerKind::Automation,
        "scheduler-external".into(),
        None,
        None,
    )
    .unwrap();
    let external_carrier = CredentialCarrier::open_path(&external_path).unwrap();
    let external_binding = crate::controller::binding_from_carrier(&external_carrier, 1).unwrap();
    let external_scope = delegation::DelegationScope {
        view: &view,
        state_root: &state_root,
        controller: &external_binding,
        trusted_invocation: None,
    };
    let external_port = SemanticRunSettlementPort {
        carrier: &external_carrier,
    };
    let external_observer = ObservedStart {
        inner: &external_port,
        root,
    };
    let (opened, _) =
        delegation::open_external_engagement(&external_scope, "scheduler-open", None).unwrap();
    let engagement = Uuid::parse_str(&opened.engagement_ref).unwrap();
    let hired = delegation::hire_external_specialist(
        &external_scope,
        &external_port,
        delegation::HireExternalSpecialist {
            engagement_id: engagement,
            provisioning_ref: &candidate.provisioning_ref,
            role_ref: "general",
            agent_configuration_digest: &candidate.agent_configuration_digest,
            idempotency_key: "scheduler-hire-write",
            expected_revision: None,
        },
    )
    .unwrap();
    let external = Uuid::parse_str(&hired.member.member_ref).unwrap();
    crate::semantic::release_external_writer(
        &view,
        &state_root,
        external,
        engagement,
        &external_carrier,
    )
    .unwrap();
    let (_, external_reader) = RunSnapshot::observe(&state_root, external, 0).unwrap();
    assert_eq!(external_reader.effective_policy.access, Access::Read);
    assert_eq!(
        external_reader.effective_policy.verification,
        PolicyVerification::Verified
    );
    if correction_only {
        // The existing external hire leaves its actual logical marker unstarted.
        // Prepare one failed activation before the original positive campaign;
        // these controlled clocks are fixture data, never a product retry/reset.
        let (fault_task, fault_receipt) = delegation::queue_external_specialist_task(
            &external_scope,
            &caller_scope::fixture(32),
            &external_port,
            delegation::AssignExternalSpecialistTask {
                engagement_id: engagement,
                member_ref: external,
                task: task("scheduler-activation-no-submit"),
                execution_intent: "canonical_workspace_write",
                target: None,
                deadline_seconds: 600,
                idempotency_key: "scheduler-activation-no-submit",
                expected_revision: None,
            },
            1,
        )
        .unwrap();
        let activation_proof = checked(
            root,
            "scheduler activation compensation",
            crate::semantic::native_scheduler_activation_fixture(
                &view,
                &state_root,
                crate::scheduler::Actor {
                    owner: crate::assignment_queue::Owner::External(engagement),
                    run: external,
                },
                &external_carrier,
            ),
        );
        assert_eq!(activation_proof["task"], fault_task.task_ref);
        let retained_receipt =
            EngagementStore::open(&EngagementStore::workspace_database_path(&state_root))
                .unwrap()
                .intent_lookup(&fault_receipt.namespace)
                .unwrap();
        assert!(
            matches!(retained_receipt, crate::engagement::IntentLookup::Accepted(receipt) if *receipt == fault_receipt)
        );
        private(
            &root.join("scheduler-activation-compensation.json"),
            &serde_json::to_vec(&activation_proof).unwrap(),
        );
        let store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&state_root)).unwrap();
        let revision = crate::engagement::scope_membership_revision(
            store.connection(),
            &view.workspace_id,
            "external_engagement",
            &engagement.to_string(),
        )
        .unwrap();
        let released = checked(
            root,
            "scheduler failed member release",
            delegation::release_external_specialist(
                &external_scope,
                &external_port,
                delegation::ReleaseExternalSpecialist {
                    engagement_id: engagement,
                    member_ref: external,
                    expected_revision: revision,
                    allow_interrupt: false,
                },
            ),
        );
        assert_eq!(released.operation.progress, "terminal");
        assert!(cleanup(root), "scheduler correction owned cleanup failed");
        return json!({"schema":"dolgorae.native-scheduler-component/v1", "phase":"correction", "passed":true,
            "activation_compensation":activation_proof,"owned_cleanup_verified":true,
            "prepared_accepted_queue_fixture":true,"live_policy_enablement_proven":false});
    }
    let (managed_task, managed_receipt) = managed_assignment.expect("normal queued work");
    let (external_task, external_receipt) = delegation::queue_external_specialist_task(
        &external_scope,
        &caller_scope::fixture(32),
        &external_port,
        delegation::AssignExternalSpecialistTask {
            engagement_id: engagement,
            member_ref: external,
            task: task("scheduler-external-write"),
            execution_intent: "canonical_workspace_write",
            target: None,
            deadline_seconds: 600,
            idempotency_key: "scheduler-external-write",
            expected_revision: None,
        },
        1,
    )
    .unwrap();
    sources.admission.execution_lane = "shared_readonly".into();
    sources.admission.requested_access = "read_only".into();
    let (reader_candidate, _) = delegation::install_provisioning(
        &external_scope,
        &sources,
        "scheduler-reader-install",
        None,
    )
    .unwrap();
    let mut readers = Vec::new();
    let mut passivation_reader = None;
    for (index, brief) in ["scheduler-pool-first", "scheduler-pool-sibling"]
        .iter()
        .enumerate()
    {
        let key = format!("scheduler-reader-{index}");
        let hired = delegation::hire_external_specialist(
            &external_scope,
            &external_port,
            delegation::HireExternalSpecialist {
                engagement_id: engagement,
                provisioning_ref: &reader_candidate.provisioning_ref,
                role_ref: "general",
                agent_configuration_digest: &reader_candidate.agent_configuration_digest,
                idempotency_key: &key,
                expected_revision: None,
            },
        )
        .unwrap();
        let member = Uuid::parse_str(&hired.member.member_ref).unwrap();
        if index == 0 {
            // Prepare a legal resident actor with no required result. The R
            // collection facade cannot acknowledge an earlier task's result.
            let ready = RunSnapshot::load(&state_root, member, 0).unwrap();
            assert_eq!(
                ready.manifest.execution_lane,
                crate::domain::ExecutionLane::SharedReadonly
            );
            assert_eq!(ready.captured_effective_policy.access, Access::Read);
            assert_eq!(
                ready.captured_effective_policy.verification,
                PolicyVerification::Verified
            );
            assert!(ready.projection.thread_id.is_some());
            let mut store =
                EngagementStore::open(&EngagementStore::workspace_database_path(&state_root))
                    .unwrap();
            let (captured, _, state) = store
                .external_member_configuration(engagement, member)
                .unwrap();
            assert_eq!(state, "active");
            assert_eq!(ready.manifest.agent_configuration, captured);
            assert_eq!(
                captured
                    .admitted_configuration
                    .as_ref()
                    .unwrap()
                    .digest()
                    .unwrap(),
                reader_candidate.agent_configuration_digest
            );
            let runtime = ready.runtime_record.as_ref().unwrap();
            assert_eq!(
                crate::worker::classify_worker_identity(runtime),
                crate::worker::ProcessIdentityVerdict::Match
            );
            let status = crate::worker::call_run_worker(
                &state_root,
                member,
                DarwinSystem.current_uid(),
                None,
                |expected| crate::worker::ControlRequestV1::RunStatus {
                    expected,
                    caller: None,
                },
            )
            .unwrap();
            assert!(
                matches!(status,crate::worker::ControlResponseV1::Status{identity,lifecycle,active_turn:None,..} if identity==runtime.identity && lifecycle=="idle")
            );
            assert!(
                store
                    .external_active_tasks(engagement)
                    .unwrap()
                    .iter()
                    .all(|task| task.specialist_run_id != member)
            );
            store
                .mark_external_member_resident(engagement, member)
                .unwrap();
            passivation_reader = Some(member);
            continue;
        }
        let (queued, _) = delegation::queue_external_specialist_task(
            &external_scope,
            &caller_scope::fixture(32),
            &external_port,
            delegation::AssignExternalSpecialistTask {
                engagement_id: engagement,
                member_ref: member,
                task: task(brief),
                execution_intent: "read_only",
                target: None,
                deadline_seconds: 600,
                idempotency_key: &format!("scheduler-mail-{index}"),
                expected_revision: None,
            },
            1,
        )
        .unwrap();
        readers.push((member, Uuid::parse_str(&queued.task_ref).unwrap()));
    }
    let managed_task = Uuid::parse_str(&managed_task.task_ref).unwrap();
    let external_task = Uuid::parse_str(&external_task.task_ref).unwrap();
    // Recover an expired pre-effect lease from this real idle Run's verified
    // ledger. Conversely, a crossed/unknown dispatch boundary remains fenced
    // even when the same authoritative Run is idle and the lease is expired.
    {
        let actor = crate::scheduler::Actor {
            owner: crate::assignment_queue::Owner::Managed(primary),
            run: managed,
        };
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&state_root)).unwrap();
        let permit = store
            .transact(|tx| {
                crate::scheduler::claim(tx, actor.owner, managed, crate::scheduler::now_ms()?)
            })
            .unwrap()
            .unwrap();
        assert_eq!(permit.work, managed_task);
        let snapshot = RunSnapshot::load(&state_root, managed, 0).unwrap();
        store
            .transact(|tx| {
                tx.execute(
                    "UPDATE scheduler_claims SET lease_until_ms=0 WHERE claim_id=?1",
                    [permit.claim.to_string()],
                )
                .unwrap();
                crate::scheduler::reconcile_claim(
                    tx,
                    actor,
                    &snapshot,
                    crate::scheduler::now_ms()?,
                    &state_root,
                )
            })
            .unwrap();
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM scheduler_claims WHERE target_run_id=?1",
                    [managed.to_string()],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            0
        );
        let permit = store
            .transact(|tx| {
                crate::scheduler::claim(tx, actor.owner, managed, crate::scheduler::now_ms()?)
            })
            .unwrap()
            .unwrap();
        store
            .transact(|tx| {
                crate::scheduler::prepare_dispatch(tx, &permit, crate::scheduler::now_ms()?)?;
                crate::scheduler::finish_dispatch(
                    tx,
                    &permit,
                    &delegation::MemberDispatchOutcome::Uncertain {
                        code: "RECOVERY_REQUIRED".into(),
                        message: "test-owned pre-send fault".into(),
                    },
                    crate::scheduler::now_ms()?,
                )?;
                tx.execute(
                    "UPDATE scheduler_claims SET lease_until_ms=0 WHERE claim_id=?1",
                    [permit.claim.to_string()],
                )
                .unwrap();
                crate::scheduler::reconcile_claim(
                    tx,
                    actor,
                    &snapshot,
                    crate::scheduler::now_ms()?,
                    &state_root,
                )
            })
            .unwrap();
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT phase FROM scheduler_claims WHERE claim_id=?1",
                    [permit.claim.to_string()],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "unknown"
        );
        // The fixture itself knows it never called the Worker. Settle this one
        // local fault with an explicit proved refusal, never by lease expiry.
        store
            .transact(|tx| {
                crate::scheduler::finish_dispatch(
                    tx,
                    &permit,
                    &delegation::MemberDispatchOutcome::Refused {
                        code: "RUN_BUSY".into(),
                        message: "test-owned no-send fault".into(),
                    },
                    crate::scheduler::now_ms()?,
                )
            })
            .unwrap();
    }
    // No gateway exists yet: these three durable admissions have lost their
    // hints. The runtime startup scan must discover the original queue rows.
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let gateway =
        crate::gateway_socket::GatewaySocket::bind(&home, &root.join("gateway.sock")).unwrap();
    let runtime = {
        let _entered = async_runtime.enter();
        crate::scheduler::ControlPlaneRuntime::start(&home, gateway.record().server_instance_id)
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(150);
    let managed_context = context(primary, "scheduler-collect");
    // Complete R observation may refuse a concurrent generation publication.
    // This trusted fixture waits for its original work and Writer transitions
    // to settle before asking for the complete result/disposition proof. It
    // never catches or retries an audit error from that proof.
    loop {
        let store =
            EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state_root))
                .unwrap();
        let mut states = vec![
            crate::orchestration::successor_task(store.connection(), primary, managed_task)
                .unwrap()
                .unwrap()
                .state,
            store
                .external_task(engagement, external_task)
                .unwrap()
                .state,
        ];
        for (_, task) in &readers {
            states.push(store.external_task(engagement, *task).unwrap().state);
        }
        assert!(
            states.iter().all(|state| !matches!(
                state.as_str(),
                "failed" | "cancelled" | "expired" | "interrupted_unknown"
            )),
            "accepted scheduler work failed: {states:?}"
        );
        let complete = states
            .iter()
            .all(|state| state == "completed_not_delivered");
        let writer = crate::writer::WriterStore::new(
            &state_root,
            &view.workspace_id,
            DarwinSystem.current_uid(),
        )
        .load()
        .unwrap();
        let vacant = writer.state == crate::writer::WriterAuthorityState::None
            && writer.holder.is_none()
            && writer.transaction_id.is_none()
            && writer.handoff.is_none()
            && writer.recovery_action.is_none()
            && writer
                .generation_release
                .as_ref()
                .is_none_or(|release| !release.pending());
        if complete && vacant {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scheduler work or Writer did not settle: {states:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    {
        let m = delegation::collect_specialist_task(
            &scope,
            &managed_context,
            &start_port,
            delegation::CollectSpecialistTask {
                member_ref: managed,
                task_ref: managed_task,
                adapter_id: "scheduler-component",
                adapter_version: "1",
            },
        )
        .unwrap();
        let e = delegation::collect_external_specialist_task(
            &external_scope,
            &external_observer,
            delegation::CollectExternalSpecialistTask {
                engagement_id: engagement,
                member_ref: external,
                task_ref: external_task,
                adapter_id: "scheduler-component",
                adapter_version: "1",
            },
        )
        .unwrap();
        let mut complete = m.task.state == "completed" && e.task.state == "completed";
        for (member, task) in &readers {
            let r = delegation::collect_external_specialist_task(
                &external_scope,
                &external_observer,
                delegation::CollectExternalSpecialistTask {
                    engagement_id: engagement,
                    member_ref: *member,
                    task_ref: *task,
                    adapter_id: "scheduler-component",
                    adapter_version: "1",
                },
            )
            .unwrap();
            complete &= r.task.state == "completed";
        }
        assert!(
            complete,
            "settled original tasks did not produce complete results"
        );
    }
    for (task, receipt) in [
        (managed_task, &managed_receipt),
        (external_task, &external_receipt),
    ] {
        let store =
            EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state_root))
                .unwrap();
        assert!(
            matches!(store.intent_lookup(&receipt.namespace).unwrap(),crate::engagement::IntentLookup::Accepted(ref observed) if observed.as_ref()==receipt)
        );
        let body = fs::read_to_string(workspace.join(if task == managed_task {
            "scheduler-managed.txt"
        } else {
            "scheduler-external.txt"
        }))
        .unwrap();
        assert_eq!(body, "written\n");
    }
    let reader = passivation_reader.unwrap();
    let sibling = readers[0].0;
    let before = RunSnapshot::load(&state_root, reader, 0).unwrap();
    let sibling_before = RunSnapshot::load(&state_root, sibling, 0).unwrap();
    assert_eq!(
        before.manifest.execution_lane,
        crate::domain::ExecutionLane::SharedReadonly
    );
    let server_before = crate::profile::observe_history_server(
        home.root(),
        before.manifest.global_profile_binding.as_ref().unwrap(),
    )
    .unwrap();
    let old_runtime = before.runtime_record.clone().unwrap();
    private(
        &root.join("scheduler-captured-reader.json"),
        &serde_json::to_vec(&before.manifest).unwrap(),
    );
    // Advance only this test-owned idle cache; the production 600-second policy
    // remains frozen and the actual Worker/process/thread guards still run.
    let mut store =
        EngagementStore::open(&EngagementStore::workspace_database_path(&state_root)).unwrap();
    store.transact(|tx|{crate::scheduler::observe_actor(tx,crate::scheduler::Actor{owner:crate::assignment_queue::Owner::External(engagement),run:reader},before.stamp.run_state_revision,true,crate::scheduler::now_ms()? - 601_000)?;tx.execute("UPDATE scheduler_actors SET observed_revision=?2,idle_since_ms=?3 WHERE target_run_id=?1",rusqlite::params![reader.to_string(),before.stamp.run_state_revision,crate::scheduler::now_ms()?-601_000]).unwrap();Ok(())}).unwrap();
    drop(store);
    crate::scheduler::notify(&state_root, reader);
    let until = Instant::now() + Duration::from_secs(40);
    loop {
        let store =
            EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state_root))
                .unwrap();
        if store
            .external_member_actor_residency(engagement, reader)
            .unwrap()
            == "passivated"
        {
            break;
        }
        assert!(Instant::now() < until, "scheduler reader did not passivate");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(crate::worker::prove_worker_generation_absent(&old_runtime).is_ok());
    assert_eq!(
        RunSnapshot::load(&state_root, sibling, 0)
            .unwrap()
            .runtime_record,
        sibling_before.runtime_record
    );
    let shared = crate::profile::observe_history_server(
        home.root(),
        before.manifest.global_profile_binding.as_ref().unwrap(),
    )
    .unwrap();
    crate::profile::revalidate_history_server(
        home.root(),
        before.manifest.global_profile_binding.as_ref().unwrap(),
        &shared,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&server_before).unwrap(),
        serde_json::to_value(&shared).unwrap()
    );
    private(
        &home.root().join("roles/general/AGENTS.md"),
        b"Edited authoring source must not replace accepted instructions.\n",
    );
    let (resumed_task, _) = delegation::queue_external_specialist_task(
        &external_scope,
        &caller_scope::fixture(32),
        &external_port,
        delegation::AssignExternalSpecialistTask {
            engagement_id: engagement,
            member_ref: reader,
            task: task("scheduler-pool-resume"),
            execution_intent: "read_only",
            target: None,
            deadline_seconds: 600,
            idempotency_key: "scheduler-resume",
            expected_revision: None,
        },
        1,
    )
    .unwrap();
    let resumed_task = Uuid::parse_str(&resumed_task.task_ref).unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let store =
            EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state_root))
                .unwrap();
        let state = store.external_task(engagement, resumed_task).unwrap().state;
        assert!(
            !matches!(
                state.as_str(),
                "failed" | "cancelled" | "expired" | "interrupted_unknown"
            ),
            "resumed scheduler work failed: {state}"
        );
        let writer = crate::writer::WriterStore::new(
            &state_root,
            &view.workspace_id,
            DarwinSystem.current_uid(),
        )
        .load()
        .unwrap();
        let vacant = writer.state == crate::writer::WriterAuthorityState::None
            && writer.holder.is_none()
            && writer.transaction_id.is_none()
            && writer.handoff.is_none()
            && writer.recovery_action.is_none()
            && writer
                .generation_release
                .as_ref()
                .is_none_or(|release| !release.pending());
        if state == "completed_not_delivered" && vacant {
            break;
        }
        assert!(
            Instant::now() < until,
            "scheduler reader did not resume accepted mail"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let resumed = delegation::collect_external_specialist_task(
        &external_scope,
        &external_observer,
        delegation::CollectExternalSpecialistTask {
            engagement_id: engagement,
            member_ref: reader,
            task_ref: resumed_task,
            adapter_id: "scheduler-component",
            adapter_version: "1",
        },
    )
    .unwrap();
    assert_eq!(resumed.task.state, "completed");
    let after = RunSnapshot::load(&state_root, reader, 0).unwrap();
    assert_eq!(after.manifest, before.manifest);
    assert_eq!(after.projection.thread_id, before.projection.thread_id);
    assert_eq!(
        after.projection.run_generation,
        before.projection.run_generation + 1
    );
    assert_eq!(
        RunSnapshot::load(&state_root, sibling, 0)
            .unwrap()
            .runtime_record,
        sibling_before.runtime_record
    );
    async_runtime.block_on(runtime.shutdown());
    drop(gateway);
    async_runtime.shutdown_timeout(Duration::from_secs(1));
    // The existing fixture's owner-aware cleanup is the only teardown route.
    assert!(cleanup(root), "scheduler owned cleanup failed");
    json!({"schema":"dolgorae.native-scheduler-component/v1","phase":"normal","passed":true,"prepared_accepted_queue_fixture":true,"live_policy_enablement_proven":false,
       "startup_lost_wakes_reconciled":true,"managed_queued_write":true,"external_queued_write":true,"retained_receipts_verified":true,
       "shared_reader_passivated":true,"original_thread_resumed":true,"one_reactivation_generation":true,"captured_configuration_retained_after_role_edit":true,
       "result_free_ready_residency_prepared":true,
       "shared_server_and_sibling_preserved":true,"owned_cleanup_verified":true,"reader_before_generation":before.projection.run_generation,"reader_after_generation":after.projection.run_generation})
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
    private(
        &workspace.join(".git/info/exclude"),
        b"/.podway/\n/.dolgorae/\n",
    );
    let home = DolgoraeHome::system().unwrap();
    let startup_only =
        std::env::var("DOLGORAE_NATIVE_SUCCESSOR_STARTUP_COMPONENT").as_deref() == Ok("1");
    private(
        &home.root().join("home/AGENTS.md"),
        b"integrated-common-017\n",
    );
    private(
        &home.root().join("roles/general/AGENTS.md"),
        &[
            b"integrated-general-017\n".as_slice(),
            &if startup_only {
                b"Bounded native successor startup fixture.\n".to_vec()
            } else {
                fs::read(root.join("aquarium-source/references/executor-guidance.md")).unwrap()
            },
        ]
        .concat(),
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
        .prepare_qualified(&home)
        .unwrap();
    let operator = root.join("operator");
    OperatorStore::new(home.operator_root())
        .initialize(&operator)
        .unwrap();
    assert!(binding.caller_workspace_supported());
    binding.record_qualified(&home).unwrap();
    private(
        &root.join("qualified-binding.json"),
        &serde_json::to_vec(&binding).unwrap(),
    );
    crate::profile::ensure_qualified_global_server(&binding).unwrap();
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
    start["model"] = json!("gpt-5.6-luna");
    start["effort"] = json!("low");
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
        trusted_invocation: None,
    };
    crate::engagement::ensure_delegation_namespace(&state_root).unwrap();
    let source_store = InstructionSourceStore::new(&home);
    assert!(binding.caller_workspace_supported());
    assert_eq!(
        binding.launch_snapshot.process_static_configuration["features"]["shell_snapshot"],
        json!(false)
    );
    let mut sources = ProvisioningSources {
        instruct: &source_store,
        role_name: "general",
        role: SelectedInstructionSource::File(vec!["roles".to_owned(), "general".to_owned()]),
        dependencies: vec![SelectedNamedSource {
            name: "formal-agent-orchestration-v1".into(),
            source: SelectedInstructionSource::Inline(if startup_only {
                "Bounded accepted successor startup; no model Turn.".into()
            } else {
                fs::read_to_string(
                    root.join("aquarium-source/references/formal-agent-orchestration-v1.md"),
                )
                .unwrap()
            }),
        }],
        selected_guidance: Vec::new(),
        profile_binding: binding,
        admission: ProvisioningAdmission {
            model: "gpt-5.6-luna".to_owned(),
            effort: "low".to_owned(),
            purpose_kind: "implementation".to_owned(),
            purpose_label: None,
            required_capabilities: Vec::new(),
            execution_lane: "dedicated".to_owned(),
            requested_access: if startup_only {
                "read_only"
            } else {
                "canonical_workspace_write"
            }
            .to_owned(),
            required_assurance: "best_effort_personal_alpha".to_owned(),
            native_subagent_policy: if startup_only { "enabled" } else { "disabled" }.to_owned(),
        },
    };
    let (candidate, _) = checked(
        root,
        "provisioning",
        delegation::install_provisioning(&scope, &sources, "native-install", None),
    );
    if startup_only {
        return successor_startup_component(root, &scope, primary, &candidate, &owner, &home);
    }
    let consumer_owner =
        managed_consumer_setup(root, &view, primary, &primary_binding, &candidate, codex);
    let port = SemanticRunSettlementPort { carrier: &owner };
    let spawned = primary_turn(
        root,
        &view,
        primary,
        "spawn",
        json!({"candidate":candidate}),
    );
    let member = Uuid::parse_str(
        spawned[0]["header"]["facts"]["member"]["member_ref"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
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
    // The actual Root Worker owns this dispatch. The five existing local fault
    // controls below still inject after acquisition and before index capture.
    private(
        &workspace.join("file.txt"),
        b"captured after qualified acquire\n",
    );
    let task = consumer_task(root, "native-managed-writer", true);
    let namespace = crate::operation::Namespace::delegation(
        &primary_binding.identity,
        &view.workspace_id,
        Some(("orchestrated_session", &primary.to_string())),
        "assign_specialist_task",
        "native-write",
    )
    .unwrap();
    let accepted = crate::source_binding::prepare_assignment(
        &consumer_owner,
        crate::source_binding::AssignmentSelection {
            namespace: (&namespace).into(),
            member_ref: member,
            task: task.clone(),
            execution_intent: "canonical_workspace_write".into(),
            deadline_seconds: 600,
            target: None,
        },
    )
    .unwrap();
    let assigned = primary_turn(
        root,
        &view,
        primary,
        "assignment",
        json!({"member_ref":member,
        "candidate":candidate,"accepted_task_digest":accepted.digest().unwrap(),
        "controller_ref":format!("controller:{}",primary_binding.identity.controller_id),
        "request":{"schema":"dolgorae.external-specialist-request/v5",
        "operation":"assign_specialist_task","scope_kind":"orchestrated_session","scope_ref":primary,
        "member_ref":member,"assignment":{"task_body":task,"accepted_task_identity":accepted,"deadline_seconds":600},
        "idempotency_key":"native-write","expected_revision":null}}),
    );
    let receipt = assigned[0]["header"]["facts"]["receipt"].clone();
    let task_id = Uuid::parse_str(
        assigned[0]["header"]["facts"]["task"]["task_ref"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
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
    assert_eq!(changes.len(), 1);
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
    if RunSnapshot::observe(&state_root, member, 0)
        .unwrap()
        .0
        .writer
        .state
        == crate::writer::WriterAuthorityState::Active
    {
        mutate(
            &view,
            &member_carrier,
            member,
            RunMutationOperation::ReleaseWriter,
        );
    }
    let complete: crate::delegation_identity::CompleteResult =
        serde_json::from_value(collected.complete_result.clone().unwrap()).unwrap();
    let originals = json!({"complete_result":serde_json::to_string(&serde_json::to_value(&complete).unwrap()).unwrap(),
        "before":"captured after qualified acquire\n","after":"1 after\n"});
    let consumed = primary_turn(
        root,
        &view,
        primary,
        "consumer",
        json!({
        "member_ref":member,"task_ref":task_id,"originals":originals,
        "candidate":candidate,"assignment_receipt":receipt,"result_digest":complete.digest().unwrap(),
        "execution":{"binding_ref":consumer_owner.configuration.caller_scope.binding_ref,
        "adapter_id":"dolgorae.managed/1","adapter_version":env!("CARGO_PKG_VERSION"),
        "primary_host":"managed","scope_kind":"orchestrated_session","scope_ref":primary,
        "supervisor_ref":format!("controller:{}",primary_binding.identity.controller_id),"worker_ref":member,"task_ref":task_id,
        "role_ref":"general","role_snapshot_digest":complete.source.role_snapshot_digest,
        "agent_configuration_digest":complete.source.agent_configuration_digest,"accepted_task_digest":complete.source.accepted_task_digest}}),
    );
    assert!(
        consumed
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["header"]["action"] == "accept"
                && v["header"]["data"]["effect"] == "committed")
    );
    consumer_finish(root);
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
    sources.dependencies.clear();
    let (ordinary_candidate, _) = checked(
        root,
        "ordinary provisioning",
        delegation::install_provisioning(&scope, &sources, "native-ordinary-install", None),
    );
    let external_evidence = external_campaign(root, &view, &state_root, &home, &ordinary_candidate);
    let closed = mutate(
        &view,
        &owner,
        primary,
        RunMutationOperation::Close { interrupt: false },
    );
    assert_eq!(closed.projection.lifecycle, RunLifecycle::Closed);
    crate::profile::retire_qualified_global_server(
        started
            .snapshot
            .manifest
            .global_profile_binding
            .as_ref()
            .unwrap(),
        &CredentialCarrier::open_path(&operator).unwrap(),
    )
    .unwrap();
    private(&root.join("cleanup-result"), b"complete\n");
    json!({"schema":"dolgorae.native-successor-pipeline/v1","passed":true,"managed_manifest":4,"shell_snapshot_disabled":true,"dispatch_failures":failure_evidence,"external":external_evidence,"production_managed_dispatch":true,"broker_writer_acquisition":true,"dispatch_index_before_submit":true,"published_changes":changes.len(),"normal_live_close":ordering.borrow().clone().unwrap(),"cleanup_before_profile_retirement":true,"profile_retired":true,"broker_credential_retired_after_terminal":true,"receipt_operation":receipt["namespace"]["operation_kind"],"actual_primary_consumer":true})
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
        trusted_invocation: None,
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
                task: consumer_task(root, "native-external-writer", false),
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
    // Ordinary delivery is consumed as v3, without a formal acceptance receipt.
    let complete: crate::delegation_identity::CompleteResult =
        serde_json::from_value(collected.complete_result.clone().unwrap()).unwrap();
    let result_digest = complete.digest().unwrap();
    let original = crate::delegation_service::original_delegation_response(state, &complete, false)
        .unwrap()
        .unwrap();
    let handoff: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(handoff["schema"], "aquarium.executor-handoff/v3");
    assert_eq!(handoff["assignment"], "ordinary-assignment.json");
    assert_eq!(handoff["changed_artifacts"][0]["path"], "file.txt");
    assert_eq!(
        handoff["changed_artifacts"][0]["sha256"],
        crate::jcs::sha256_hex(
            &fs::read(view.canonical_path.to_path_buf().unwrap().join("file.txt")).unwrap()
        )
    );
    assert_eq!(handoff["results"][0]["exit"], 0);
    assert!(!handoff["gaps"].as_array().unwrap().is_empty());
    private(
        &root.join("ordinary-consumed.json"),
        &serde_json::to_vec(&json!({"schema":handoff["schema"],
            "accepted_task_digest":complete.source.accepted_task_digest,
            "result_digest":result_digest,"changed_artifacts":handoff["changed_artifacts"],
            "gaps":handoff["gaps"],"formal_receipt_required":false}))
        .unwrap(),
    );
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

// Owned preparation faults retain qualified Write proof and rollback before
// Submit, followed by Reader release or exact guard preservation.
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
    for mode in ["preparation_release_ok", "preparation_release_failed"] {
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
                let store =
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
                    let mut capability = wire["capability"].as_str().unwrap().as_bytes().to_vec();
                    capability[0] = if capability[0] == b'A' { b'B' } else { b'A' };
                    wire["capability"] = json!(String::from_utf8(capability).unwrap());
                    private(&path, &serde_json::to_vec(&wire).unwrap());
                    *credential_fault.borrow_mut() = Some((path, original));
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
        if mode == "preparation_release_ok" {
            assert_eq!(error.code, "REVIEW_TARGET_UNSAFE_FILE");
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
        let store =
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
        fs::remove_file(workspace.join("unsafe-link")).unwrap();
        if let Some((path, original)) = credential_fault.borrow_mut().take() {
            private(&path, &original);
            // Teardown after the uncertain-state assertions: this fixture knows
            // no Submit occurred and restores the exact original carrier bytes.
            delegation::cancel_specialist_task(
                scope,
                delegation::SettlementOwner::Managed {
                    session_id: session,
                },
                Some(&context(session, "fixture-fault-cancel")),
                port,
                member,
                Uuid::parse_str(&task_id).unwrap(),
                store
                    .scope_membership_revision(
                        &view.workspace_id,
                        "orchestrated_session",
                        &session.to_string(),
                    )
                    .unwrap(),
                "fixture fault cleanup",
            )
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

fn fixture_pending_tasks(
    state: &Path,
    kind: &str,
    scope: Uuid,
    member: Uuid,
) -> Result<Vec<Uuid>, crate::machine::MachineError> {
    let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state))?;
    let query = if kind == "orchestrated_session" {
        "SELECT t.task_id FROM brokered_tasks t JOIN brokered_task_acceptances a USING(task_id)
         WHERE session_id=?1 AND target_run_id=?2 AND state IN ('accepted','queued','claimed','dispatching','running')"
    } else {
        "SELECT task_id FROM tasks WHERE engagement_id=?1 AND specialist_run_id=?2
         AND request_json IS NOT NULL AND state IN ('accepted','queued','claimed','dispatching','running')"
    };
    let mut stmt = store.connection().prepare(query).map_err(|_| {
        crate::machine::MachineError::invalid_argument("cleanup", "task observation failed")
    })?;
    let rows = stmt
        .query_map(
            rusqlite::params![scope.to_string(), member.to_string()],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| {
            crate::machine::MachineError::invalid_argument("cleanup", "task observation failed")
        })?;
    rows.map(|row| {
        row.ok()
            .and_then(|s| Uuid::parse_str(&s).ok())
            .ok_or_else(|| {
                crate::machine::MachineError::invalid_argument("cleanup", "invalid task identity")
            })
    })
    .collect()
}

pub fn cleanup(root: &Path) -> bool {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    let mut observations = Vec::new();
    let mut record = |stage: &str, run_id: Option<Uuid>, outcome: Value| {
        observations.push(json!({"stage":stage,"run_id":run_id,"outcome":outcome}));
        private(
            &root.join("cleanup-observations.json"),
            &serde_json::to_vec(&observations).unwrap(),
        );
    };
    let error_fields = |error: &crate::machine::MachineError| {
        json!({"code":error.code,
            "message":error.message.chars().take(256).collect::<String>(),
            "reason":error.details.get("reason").and_then(Value::as_str)
                .map(|reason| reason.chars().take(256).collect::<String>())})
    };
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
                // A closed Run can still have an unsettled successor release.
                // Its owning service, not legacy Close, retires that membership.
                let released: Result<
                    Option<delegation::MemberReleaseView>,
                    crate::machine::MachineError,
                > = (|| {
                    let Some(aggregate) = manifest.aggregate_binding.as_ref().filter(|binding| {
                        binding.member_kind == crate::run::AggregateMemberKind::Specialist
                    }) else {
                        return Ok(None);
                    };
                    let store = EngagementStore::open_read_only(
                        &EngagementStore::workspace_database_path(&state),
                    )?;
                    let (kind, controller, credential) = match aggregate.aggregate_kind {
                        crate::domain::AggregateKind::OrchestratedSession => {
                            let facts = crate::orchestration::successor_member(
                                store.connection(),
                                aggregate.aggregate_id,
                                id,
                            )?;
                            let Some(facts) = facts else { return Ok(None) };
                            if crate::engagement::successor_spawn_member(
                                store.connection(),
                                &facts.spawn_operation_id.to_string(),
                            )?
                            .as_deref()
                                != Some(id.to_string().as_str())
                            {
                                return Ok(None);
                            }
                            (
                                "orchestrated_session",
                                runs.load_controller_binding(aggregate.aggregate_id)?,
                                "controller",
                            )
                        }
                        crate::domain::AggregateKind::ExternalSpecialistEngagement => {
                            if !crate::engagement::successor_hire_member(
                                store.connection(),
                                &id.to_string(),
                            )? {
                                return Ok(None);
                            }
                            let (_, controller) =
                                store.external_owner_binding(aggregate.aggregate_id)?;
                            ("external_engagement", controller, "external-controller")
                        }
                    };
                    let carrier = CredentialCarrier::open_path(&root.join(credential))?
                        .with_expected_generation(controller.identity.generation);
                    let view = WorkspaceView {
                        workspace_id: manifest.workspace_id.clone(),
                        canonical_path: manifest.canonical_workspace.clone(),
                        mode: manifest.workspace_mode,
                        created: false,
                    };
                    let revision = crate::engagement::scope_membership_revision(
                        store.connection(),
                        &view.workspace_id,
                        kind,
                        &aggregate.aggregate_id.to_string(),
                    )?;
                    drop(store);
                    let scope = delegation::DelegationScope {
                        view: &view,
                        state_root: &state,
                        controller: &controller,
                        trusted_invocation: None,
                    };
                    let port = SemanticRunSettlementPort { carrier: &carrier };
                    let context = delegation::ManagedCallContext {
                        session_id: aggregate.aggregate_id,
                        source_run_id: aggregate.aggregate_id,
                        source_turn_id: "fixture-cleanup",
                        source_tool_call_id: "fixture-cleanup-cancel",
                        idempotency_key: "fixture-cleanup-cancel",
                    };
                    for task in fixture_pending_tasks(&state, kind, aggregate.aggregate_id, id)? {
                        delegation::cancel_specialist_task(
                            &scope,
                            if kind == "orchestrated_session" {
                                delegation::SettlementOwner::Managed {
                                    session_id: aggregate.aggregate_id,
                                }
                            } else {
                                delegation::SettlementOwner::External {
                                    engagement_id: aggregate.aggregate_id,
                                }
                            },
                            (kind == "orchestrated_session").then_some(&context),
                            &port,
                            id,
                            task,
                            revision,
                            "fixture cleanup",
                        )?;
                    }
                    let result = if kind == "orchestrated_session" {
                        // Administrative fixture context, never positive native-call evidence.
                        let context = delegation::ManagedCallContext {
                            session_id: aggregate.aggregate_id,
                            source_run_id: aggregate.aggregate_id,
                            source_turn_id: "fixture-cleanup",
                            source_tool_call_id: "fixture-cleanup-release",
                            idempotency_key: "fixture-cleanup-release",
                        };
                        delegation::release_specialist(
                            &scope,
                            &context,
                            &port,
                            delegation::ReleaseSpecialist {
                                member_ref: id,
                                expected_revision: revision,
                                allow_interrupt: true,
                            },
                        )?
                    } else {
                        delegation::release_external_specialist(
                            &scope,
                            &port,
                            delegation::ReleaseExternalSpecialist {
                                engagement_id: aggregate.aggregate_id,
                                member_ref: id,
                                expected_revision: revision,
                                allow_interrupt: true,
                            },
                        )?
                    };
                    Ok(Some(result))
                })();
                match released {
                    Ok(Some(released)) => {
                        record(
                            "successor_release",
                            Some(id),
                            json!({"ok":released.state == "released",
                            "state":released.state,"progress":released.operation.progress,
                            "operation_ref":released.operation.operation_ref}),
                        );
                        continue;
                    }
                    Err(error) => {
                        record(
                            "successor_release",
                            Some(id),
                            json!({"ok":false,"error":error_fields(&error)}),
                        );
                        continue;
                    }
                    Ok(None) => {}
                }
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
                record(
                    "credential_open",
                    Some(id),
                    json!({"ok":credential.is_ok(),"error":credential.as_ref().err().map(&error_fields)}),
                );
                if let Ok(carrier) = credential {
                    let view = WorkspaceView {
                        workspace_id: manifest.workspace_id.clone(),
                        canonical_path: manifest.canonical_workspace.clone(),
                        mode: manifest.workspace_mode,
                        created: false,
                    };
                    let closed = CoreSemanticService.mutate_caller_run(RunMutationInput {
                        workspace: view.canonical_path.to_path_buf().unwrap(),
                        expected_workspace_id: view.workspace_id,
                        run_id: id,
                        expected_state_revision: None,
                        carrier,
                        operation: RunMutationOperation::Close { interrupt: true },
                    });
                    record(
                        "close",
                        Some(id),
                        json!({"ok":closed.is_ok(),"error":closed.as_ref().err().map(&error_fields)}),
                    );
                }
            }
        }
    }
    let bytes = fs::read(root.join("qualified-binding.json"));
    record(
        "binding_read",
        None,
        json!({"ok":bytes.is_ok(),"error":bytes.as_ref().err().map(|error|
            json!({"code":"IO_ERROR","reason":format!("{:?}",error.kind())}))}),
    );
    let Ok(bytes) = bytes else {
        return true;
    };
    let binding = serde_json::from_slice::<crate::global_runtime::GlobalProfileBinding>(&bytes);
    record(
        "binding_decode",
        None,
        json!({"ok":binding.is_ok(),"error":binding.as_ref().err().map(|error|
            json!({"code":"INVALID_JSON","reason":format!("{:?}",error.classify())}))}),
    );
    let Ok(binding) = binding else {
        return false;
    };
    let operator = CredentialCarrier::open_path(&root.join("operator"));
    record(
        "operator_open",
        None,
        json!({"ok":operator.is_ok(),"error":operator.as_ref().err().map(&error_fields)}),
    );
    let Ok(operator) = operator else {
        return false;
    };
    let retired = crate::profile::retire_qualified_global_server(&binding, &operator);
    record(
        "profile_retire",
        None,
        json!({"ok":retired.is_ok(),"error":retired.as_ref().err().map(&error_fields)}),
    );
    let mut all_retired = retired.is_ok();
    let member_binding = root.join("qualified-member-binding.json");
    if member_binding.exists() {
        let additional = fs::read(&member_binding).ok().and_then(|bytes| {
            serde_json::from_slice::<crate::global_runtime::GlobalProfileBinding>(&bytes).ok()
        });
        if let Some(additional) = additional {
            let retired = crate::profile::retire_qualified_global_server(&additional, &operator);
            record(
                "member_profile_retire",
                None,
                json!({"ok":retired.is_ok(),"error":retired.as_ref().err().map(&error_fields)}),
            );
            all_retired &= retired.is_ok();
        } else {
            all_retired = false;
        }
    }
    all_retired
}
