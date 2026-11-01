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
use rusqlite::OptionalExtension as _;
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

/// Prepared policy is test data; only the two Specialists are physical native
/// actors here. The production live-policy admission gate remains untouched.
pub fn collaboration_component(root: &Path, codex: &Path) -> Value {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    assert_eq!(
        fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
        root.join("home")
    );
    let case: Value =
        serde_json::from_slice(&fs::read(root.join("component-policy-case.json")).unwrap())
            .unwrap();
    let writer_case = case["variation"].as_str() == Some("caller_policy_writer");
    git(&root.join("workspace"), &["init", "-q"]);
    let SemanticResult::Workspace(view) = CoreSemanticService
        .execute(&SemanticCommand::WorkspaceInspect {
            workspace: Some(root.join("workspace")),
        })
        .unwrap()
    else {
        panic!("workspace")
    };
    let workspace = view.canonical_path.to_path_buf().unwrap();
    private(
        &workspace.join("file.txt"),
        b"read-only consultation evidence\n",
    );
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "fixture baseline"]);
    let home = DolgoraeHome::system().unwrap();
    private(
        &home.root().join("home/AGENTS.md"),
        b"Bounded accountless component qualification.\n",
    );
    private(
        &home.root().join("roles/general/AGENTS.md"),
        if writer_case {
            b"Execute only the accepted Writer task. Write only caller-policy-writer-A.txt or caller-policy-writer-B.txt in this workspace.\n"
        } else {
            b"Execute only the accepted read-only task. Never run a shell for a consultation.\n"
        },
    );
    component_cli(
        root,
        &[
            "profile",
            "add",
            PROFILE,
            "--codex-home",
            root.join("profile").to_str().unwrap(),
            "--native-subagents",
            "enabled",
            "--env",
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "--env",
            "LANG=C",
            "--env",
            "LC_ALL=C",
            "--",
            codex.to_str().unwrap(),
        ],
        None,
    );
    component_cli(
        root,
        &[
            "operator",
            "credential",
            "initialize",
            "--output",
            root.join("operator").to_str().unwrap(),
        ],
        None,
    );
    // Observe an unrecorded candidate only. The current main owns publication
    // and the actual server generation under its own composing build.
    let binding = ResolvedGlobalProfile::resolve(&home, PROFILE)
        .unwrap()
        .prepare_qualified(&home)
        .unwrap();
    private(
        &root.join("qualified-binding.json"),
        &serde_json::to_vec(&binding).unwrap(),
    );
    let ready_profile = component_cli(
        root,
        &[
            "profile",
            "server",
            "start",
            PROFILE,
            "--confirm-server-key",
            &binding.server_key,
        ],
        None,
    );
    assert_eq!(ready_profile["state"]["server_key"], binding.server_key);
    private(&root.join("main-profile-start.json"),&serde_json::to_vec(&json!({"server_key":ready_profile["state"]["server_key"],"server_epoch":ready_profile["state"]["server_epoch"],"pid":ready_profile["state"]["pid"],"drainer_pid":ready_profile["state"]["drainer_pid"],"main_owned":true})).unwrap());
    let mut policy: Value = serde_json::from_str(include_str!(
        "../../docs/protocol/examples/workspace-policy-input-v3.valid.json"
    ))
    .unwrap();
    policy["policy_name"] = json!("native-collaboration");
    let approval = case["approval_policy"].as_str().unwrap();
    assert!(matches!(
        approval,
        "fully_delegated" | "user_approval_required"
    ));
    policy["approval_policy"] = json!(approval);
    policy["max_active_specialists"] = json!(2);
    let role = &mut policy["roles"][0];
    role["role_ref"] = json!("general");
    role["role_source"] = json!({"name":"general"});
    role["max_active_instances"] = json!(2);
    role["reuse_policy"] = json!("never");
    role["collaboration_source"] = json!(true);
    role["collaboration_target"] = json!(true);
    role["agent_configuration"]["selected_profile"] = json!(PROFILE);
    role["agent_configuration"]["model"] = json!("gpt-5.6-luna");
    role["agent_configuration"]["default_effort"] = json!("low");
    role["agent_configuration"]["purpose"] = json!("implementation");
    role["agent_configuration"]["native_subagent_policy"] = json!("enabled");
    role["agent_configuration"]
        .as_object_mut()
        .unwrap()
        .remove("global_profile_binding_sha256");
    if writer_case {
        assert_eq!(approval, "fully_delegated");
        policy["max_active_specialists"] = json!(1);
        policy["roles"][0]["max_active_instances"] = json!(1);
        policy["roles"][0]["allowed_access"] = json!(["canonical_workspace_write"]);
        policy["roles"][0]["collaboration_source"] = json!(false);
        policy["roles"][0]["collaboration_target"] = json!(false);
    }
    private(
        &root.join("policy.json"),
        &serde_json::to_vec(&policy).unwrap(),
    );
    component_cli(
        root,
        &[
            "specialist",
            "policy",
            "add",
            "native-collaboration",
            "--workspace",
            workspace.to_str().unwrap(),
            "--file",
            root.join("policy.json").to_str().unwrap(),
        ],
        None,
    );
    let ready: Value =
        serde_json::from_slice(&fs::read(root.join("foreground-ready.json")).unwrap()).unwrap();
    assert_eq!(ready["ok"], true);
    assert_eq!(ready["command"], "serve");
    component_cli(
        root,
        &[
            "controller",
            "credential",
            "create",
            "--kind",
            "interactive-client",
            "--instance-id",
            "native-collaboration-primary",
            "--orchestration-policy",
            "native-collaboration",
            "--output",
            root.join("controller").to_str().unwrap(),
        ],
        None,
    );
    let owner = CredentialCarrier::open_path(&root.join("controller")).unwrap();
    let started = component_cli(
        root,
        &[
            "run",
            "start",
            "--workspace",
            workspace.to_str().unwrap(),
            "--controller-file",
            root.join("controller").to_str().unwrap(),
            "--profile",
            PROFILE,
            "--control-mode",
            "direct-interactive",
            "--access",
            "read-only",
            "--purpose",
            "interactive",
            "--idempotency-key",
            "collaboration-primary",
        ],
        None,
    );
    let primary = Uuid::parse_str(started["run_id"].as_str().unwrap()).unwrap();
    let state_root = home.workspace_root(&view.workspace_id);
    let initial = RunSnapshot::load(&state_root, primary, 0).unwrap();
    assert_eq!(initial.manifest.schema_version, 3);
    assert_eq!(
        initial.manifest.control_mode,
        crate::domain::ControlMode::DirectInteractive
    );
    assert!(
        initial
            .manifest
            .agent_configuration
            .role_reference
            .is_none()
    );
    assert_eq!(
        initial.manifest.global_profile_binding.as_ref(),
        Some(&binding)
    );
    assert!(initial.projection.thread_id.is_some());
    if writer_case {
        return caller_policy_writer_case(root, &view, primary, &owner, &initial, &binding);
    }
    let submit_primary = |label: &str| {
        component_cli(
            root,
            &[
                "run",
                "submit",
                &primary.to_string(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--controller-fd",
                "0",
                "--message",
                label,
                "--idempotency-key",
                label,
            ],
            Some(&owner),
        )
    };
    let until = Instant::now() + Duration::from_secs(60);
    let clock = crate::providers::MonotonicClock::now(&DarwinSystem);
    let deadline = clock + until.saturating_duration_since(Instant::now());
    private(
        &root.join("primary-phase-deadline.json"),
        &serde_json::to_vec(
            &json!({"deadline_monotonic_ns":u64::try_from(deadline.as_nanos()).unwrap()}),
        )
        .unwrap(),
    );
    submit_primary("TASK027_PRIMARY_INITIAL");
    let (source, target, wait_witness) = loop {
        let broker = crate::orchestration::OrchestrationStore::open_observer(&state_root).unwrap();
        if approval == "user_approval_required" {
            for pending in broker.approval_interactions(primary).unwrap() {
                if pending.decision.is_none() {
                    component_approve(
                        root,
                        &view,
                        primary,
                        &owner,
                        pending.approval_request_id,
                        until,
                    );
                }
            }
        }
        let c = broker.collaboration_connection().unwrap();
        let exists: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='collaboration_waits')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let witness = if exists {
            c.query_row("SELECT w.source_run_id,d.target_run_id,w.source_thread_id,w.source_turn_id,w.call_id,w.source_generation,d.exchange_id FROM collaboration_waits w JOIN collaboration_wait_edges d USING(wait_id) WHERE w.session_id=?1 AND w.active=1",[primary.to_string()],|r|Ok(json!({"source":r.get::<_,String>(0)?,"target":r.get::<_,String>(1)?,"thread":r.get::<_,String>(2)?,"turn":r.get::<_,String>(3)?,"call":r.get::<_,String>(4)?,"generation":r.get::<_,u64>(5)?,"exchange":r.get::<_,String>(6)?}))).optional().unwrap()
        } else {
            None
        };
        if let Some(witness) = witness {
            let source = Uuid::parse_str(witness["source"].as_str().unwrap()).unwrap();
            let target = Uuid::parse_str(witness["target"].as_str().unwrap()).unwrap();
            let source_snapshot = RunSnapshot::load(&state_root, source, 0).unwrap();
            let target_snapshot = RunSnapshot::load(&state_root, target, 0).unwrap();
            assert_eq!(source_snapshot.manifest.schema_version, 3);
            assert_eq!(target_snapshot.manifest.schema_version, 3);
            assert_ne!(
                source_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .identity
                    .pid,
                target_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .identity
                    .pid
            );
            assert_ne!(
                source_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .dedicated_server_identity
                    .as_ref()
                    .unwrap()
                    .pid,
                target_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .dedicated_server_identity
                    .as_ref()
                    .unwrap()
                    .pid
            );
            let main_sha = crate::jcs::sha256_hex(&fs::read(root.join("current-main")).unwrap());
            assert_eq!(
                source_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .binary_sha256,
                main_sha
            );
            assert_eq!(
                target_snapshot
                    .runtime_record
                    .as_ref()
                    .unwrap()
                    .binary_sha256,
                main_sha
            );
            assert!(target_snapshot.projection.active_turn_id.is_some());
            assert!(
                crate::collaboration::dependency_unblock(
                    c,
                    Uuid::parse_str(witness["exchange"].as_str().unwrap()).unwrap()
                )
                .unwrap()
            );
            private(
                &root.join("source-await-registered.json"),
                &serde_json::to_vec(&witness).unwrap(),
            );
            break (source, target, witness);
        }
        assert!(
            Instant::now() < until,
            "actual Primary source wait not observed"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let wait_tasks = |expected: usize| {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let broker =
                crate::orchestration::OrchestrationStore::open_observer(&state_root).unwrap();
            let c = broker.collaboration_connection().unwrap();
            let (total,terminal):(usize,usize)=c.query_row("SELECT count(*),sum(state IN ('completed_not_delivered','delivered')) FROM brokered_tasks WHERE session_id=?1",[primary.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            if total == expected && terminal == expected {
                break;
            }
            assert!(
                Instant::now() < until,
                "original Caller policy tasks did not settle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait_tasks(3);
    let before = RunSnapshot::load(&state_root, source, 0).unwrap();
    let target_before = RunSnapshot::load(&state_root, target, 0).unwrap();
    let old = before.runtime_record.clone().unwrap();
    let broker = crate::orchestration::OrchestrationStore::open_observer(&state_root).unwrap();
    let source_owner = broker
        .member_credential(source)
        .unwrap()
        .controller_carrier()
        .unwrap();
    let exchange = Uuid::parse_str(wait_witness["exchange"].as_str().unwrap()).unwrap();
    let c = broker.collaboration_connection().unwrap();
    let artifact: String = c
        .query_row(
            "SELECT artifact_id FROM collaboration_results WHERE exchange_id=?1",
            [exchange.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT delivery_state FROM collaboration_exchanges WHERE exchange_id=?1",
            [exchange.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "acknowledged"
    );
    let chunk = crate::artifact::chunk(
        &state_root,
        &before,
        &artifact,
        (0, 65536),
        Some(&source_owner),
    )
    .unwrap();
    let expected_body = format!(
        "TASK027 large result prefix: 한글.\n{}TASK027 large result tail: 끝.\n",
        "한글-0123456789abcdef\n".repeat(4096)
    )
    .into_bytes();
    assert!(expected_body.len() > 65_536);
    let tail = crate::artifact::chunk(
        &state_root,
        &before,
        &artifact,
        (chunk.data.len() as u64, 65536),
        Some(&source_owner),
    )
    .unwrap();
    let mut complete = chunk.data.clone();
    complete.extend_from_slice(&tail.data);
    assert_eq!(complete, expected_body);
    assert_eq!(chunk.data.len(), 65_536);
    assert_eq!(chunk.total_byte_length, expected_body.len() as u64);
    assert_eq!(tail.total_byte_length, chunk.total_byte_length);
    assert_eq!(tail.sha256, chunk.sha256);
    assert_eq!(
        chunk.sha256.trim_start_matches("sha256:"),
        crate::jcs::sha256_hex(&complete)
    );
    let target_owner = broker
        .member_credential(target)
        .unwrap()
        .controller_carrier()
        .unwrap();
    assert!(
        crate::artifact::chunk(
            &state_root,
            &before,
            &artifact,
            (0, 65536),
            Some(&target_owner)
        )
        .is_err()
    );
    assert!(crate::artifact::chunk(&state_root, &before, &artifact, (0, 65536), None).is_err());
    drop(broker);
    component_cli(
        root,
        &[
            "run",
            "pause",
            &source.to_string(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--controller-fd",
            "0",
        ],
        Some(&source_owner),
    );
    let paused = RunSnapshot::load(&state_root, source, 0).unwrap();
    assert_eq!(paused.projection.lifecycle, RunLifecycle::Paused);
    assert_eq!(
        paused.projection.run_generation,
        old.identity.run_generation
    );
    assert!(paused.runtime_record.is_none());
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if crate::worker::prove_worker_generation_absent(&old).is_ok() {
            break;
        }
        assert!(
            Instant::now() < until,
            "old source Kernel generation absence unproved"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    component_cli(
        root,
        &[
            "run",
            "resume",
            &source.to_string(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--controller-fd",
            "0",
        ],
        Some(&source_owner),
    );
    let resumed = RunSnapshot::load(&state_root, source, 0).unwrap();
    assert!(resumed.projection.run_generation > before.projection.run_generation);
    assert_eq!(resumed.projection.thread_id, before.projection.thread_id);
    submit_primary("TASK027_PRIMARY_RECOVERY");
    wait_tasks(4);
    let after = RunSnapshot::load(&state_root, source, 0).unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    let checked_state = loop {
        let path = state_root.join("orchestration/state.json");
        let result = (|| -> Option<Value> {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
                return None;
            }
            let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
            let broker =
                crate::orchestration::OrchestrationStore::open_observer(&state_root).ok()?;
            let c = broker.collaboration_connection().ok()?;
            let o: u64 = c
                .query_row(
                    "SELECT coalesce(max(sequence),0) FROM orchestration_events",
                    [],
                    |r| r.get(0),
                )
                .ok()?;
            let e: u64 = c
                .query_row("SELECT coalesce(max(sequence),0) FROM events", [], |r| {
                    r.get(0)
                })
                .ok()?;
            if value["workspace_id"] != view.workspace_id
                || value["head_sequence"] != o + e
                || value["specialist_tasks"].as_array()?.len() != 4
            {
                return None;
            }
            if value["collaboration_exchanges"][0]["result_artifact_ref"] != artifact
                || value["collaboration_exchanges"][0]["delivery_state"] != "acknowledged"
            {
                return None;
            }
            Some(value)
        })();
        if let Some(value) = result {
            break value;
        }
        assert!(
            Instant::now() < until,
            "foreground did not publish current checked state"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    private(&root.join("foreground-checked-state-identity.json"),&serde_json::to_vec(&json!({"schema_version":checked_state["schema_version"],"head_sequence":checked_state["head_sequence"],"head_hash":checked_state["head_hash"],"workspace_id":view.workspace_id})).unwrap());
    component_cli(
        root,
        &[
            "run",
            "close",
            &primary.to_string(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--controller-fd",
            "0",
        ],
        Some(&owner),
    );
    let mut cleanup_rows = Vec::new();
    for run in [source, target] {
        let snapshot = RunSnapshot::load(&state_root, run, 0).unwrap();
        assert_eq!(snapshot.projection.lifecycle, RunLifecycle::Closed);
        assert_eq!(
            membership(&home, &snapshot.manifest),
            MembershipDisposition::Released
        );
        cleanup_rows.push(json!({"run_id":run,"stage":"member_release","ok":true}));
    }
    component_cli(
        root,
        &[
            "profile",
            "server",
            "stop",
            PROFILE,
            "--confirm-server-key",
            &binding.server_key,
            "--operator-file",
            root.join("operator").to_str().unwrap(),
        ],
        None,
    );
    cleanup_rows.push(json!({"stage":"profile_retire","ok":true,"main_owned":true}));
    private(
        &root.join("cleanup-observations.json"),
        &serde_json::to_vec(&cleanup_rows).unwrap(),
    );
    json!({"schema":"dolgorae.native-collaboration-component/v3","passed":true,"approval_policy":approval,"normal_primary":primary,"primary_thread":initial.projection.thread_id,"source_before":old,"source_after":after.runtime_record,"target":target_before.runtime_record,"active_wait":wait_witness,"source_owned_artifact_read":true,"source_owned_artifact_byte_length":expected_body.len(),"source_owned_artifact_sha256":crate::jcs::sha256_hex(&expected_body),"source_owned_artifact_read_pages":2,"foreign_artifact_read_refused":true,"credentialless_artifact_read_refused":true,"original_thread_resumed":true,"old_source_worker_absence_verified":true,"foreground_checked_state":true,"owned_cleanup_verified":true,"model_consumption_proven":false})
}

fn caller_policy_writer_case(
    root: &Path,
    view: &WorkspaceView,
    primary: Uuid,
    owner: &CredentialCarrier,
    initial: &RunSnapshot,
    binding: &crate::global_runtime::GlobalProfileBinding,
) -> Value {
    let home = DolgoraeHome::system().unwrap();
    let state = home.workspace_root(&view.workspace_id);
    let workspace = view.canonical_path.to_path_buf().unwrap();
    let wait_file = |name: &str, until: Instant| loop {
        let path = root.join(name);
        if path.exists() {
            break serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap();
        }
        assert!(
            Instant::now() < until,
            "Writer phase barrier did not arrive"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let wait_idle = |until: Instant| loop {
        let snapshot = RunSnapshot::load(&state, primary, 0).unwrap();
        if snapshot.projection.lifecycle == RunLifecycle::Idle
            && snapshot.projection.active_turn_id.is_none()
        {
            break snapshot;
        }
        assert!(Instant::now() < until, "original Root did not become idle");
        std::thread::sleep(Duration::from_millis(20));
    };
    let begin = |phase: &str| {
        let until = Instant::now() + Duration::from_secs(60);
        let deadline = crate::providers::MonotonicClock::now(&DarwinSystem)
            + until.saturating_duration_since(Instant::now());
        private(
            &root.join("primary-phase-deadline.json"),
            &serde_json::to_vec(
                &json!({"deadline_monotonic_ns":u64::try_from(deadline.as_nanos()).unwrap()}),
            )
            .unwrap(),
        );
        let label = format!("TASK027_WRITER_PRIMARY_{phase}");
        component_cli(
            root,
            &[
                "run",
                "submit",
                &primary.to_string(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--controller-fd",
                "0",
                "--message",
                &label,
                "--idempotency-key",
                &label,
            ],
            Some(owner),
        );
        until
    };
    let release_source = |phase: &str| {
        let pending = root.join(format!("writer-{phase}-release.pending"));
        private(
            &pending,
            &serde_json::to_vec(&json!({"phase":phase,"task_terminal":true})).unwrap(),
        );
        fs::rename(pending, root.join(format!("writer-{phase}-release.json"))).unwrap();
    };
    let until = begin("A");
    let ready = wait_file("writer-A-ready.json", until);
    assert_eq!(ready["phase"], "A");
    let member = Uuid::parse_str(ready["member"].as_str().unwrap()).unwrap();
    let broker = crate::orchestration::OrchestrationStore::open_observer(&state).unwrap();
    let members = broker.members(primary).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].run_id, member);
    assert_eq!(members[0].requested_access, "canonical_workspace_write");
    assert_eq!(
        members[0].spawn_operation_id.to_string(),
        ready["operation"].as_str().unwrap()
    );
    let child_owner = broker
        .member_credential(member)
        .unwrap()
        .controller_carrier()
        .unwrap();
    drop(broker);
    let born = RunSnapshot::load(&state, member, 0).unwrap();
    assert_eq!(born.manifest.schema_version, 3);
    assert_eq!(
        born.manifest.purpose.kind,
        crate::domain::PurposeKind::Implementation
    );
    assert_eq!(
        born.manifest.requested_access,
        Some(crate::domain::CallerWorkspaceAccess::Writable)
    );
    assert!(born.projection.latest_turn_id.is_none());
    assert_eq!(
        born.writer.state,
        crate::writer::WriterAuthorityState::Active
    );
    assert_eq!(born.writer.holder.as_ref().unwrap().run_id, member);
    assert_eq!(born.captured_effective_policy.access, Access::Write);
    assert_eq!(
        born.captured_effective_policy.verification,
        PolicyVerification::Verified
    );
    let main_sha = crate::jcs::sha256_hex(&fs::read(root.join("current-main")).unwrap());
    assert_eq!(
        born.runtime_record.as_ref().unwrap().binary_sha256,
        main_sha
    );
    let mut proofs = Vec::new();
    let mut source_turns = Vec::new();
    let mut target_turns = Vec::new();
    let mut released = born.clone();
    let mut completed_a: Option<(Uuid, String, RunSnapshot)> = None;
    let mut release_observation = Value::Null;
    for phase in ["A", "B"] {
        let phase_until = if phase == "A" {
            until
        } else {
            let (task, capture, expected) = completed_a.as_ref().unwrap();
            let observe = || {
                let current = RunSnapshot::load(&state, member, 0).unwrap();
                assert!(
                    current.manifest == expected.manifest,
                    "member manifest changed"
                );
                assert_eq!(current.controller, expected.controller);
                assert_eq!(
                    current.projection.run_generation,
                    expected.projection.run_generation
                );
                assert_eq!(current.projection.thread_id, expected.projection.thread_id);
                assert_eq!(
                    current.projection.latest_turn_id,
                    expected.projection.latest_turn_id
                );
                assert_eq!(current.projection.lifecycle, RunLifecycle::Idle);
                assert!(
                    current.projection.active_turn_id.is_none()
                        && current.projection.pending_requests.is_empty()
                );
                assert_eq!(current.runtime_record, expected.runtime_record);
                assert!(
                    !current
                        .runtime_record
                        .as_ref()
                        .unwrap()
                        .control_recovery_required
                );
                assert_eq!(
                    current.captured_effective_policy,
                    expected.captured_effective_policy
                );
                assert_eq!(current.captured_effective_policy.access, Access::Write);
                assert_eq!(
                    current.captured_effective_policy.verification,
                    PolicyVerification::Verified
                );
                assert_eq!(
                    current.writer.state,
                    crate::writer::WriterAuthorityState::Active
                );
                assert_eq!(
                    current.writer.writer_generation,
                    expected.writer.writer_generation
                );
                let mut current_holder = current.writer.holder.clone().unwrap();
                let mut original_holder = expected.writer.holder.clone().unwrap();
                current_holder.last_event_cursor = None;
                original_holder.last_event_cursor = None;
                assert_eq!(current_holder, original_holder);
                assert!(
                    current.writer.transaction_id.is_none()
                        && current.writer.handoff.is_none()
                        && current.writer.cancelled_handoff.is_none()
                        && current.writer.recovery_action.is_none()
                        && current.writer.generation_release.is_none()
                );
                let owner =
                    crate::orchestration::OrchestrationStore::open_observer(&state).unwrap();
                let c = owner.collaboration_connection().unwrap();
                let retained: (String, Option<String>, String) = c
                    .query_row(
                        "SELECT t.state,t.target_turn_id,p.capture_json FROM brokered_tasks t
                     JOIN policy_task_dispatch_captures p USING(task_id)
                     WHERE t.task_id=?1 AND t.session_id=?2 AND t.target_run_id=?3",
                        rusqlite::params![
                            task.to_string(),
                            primary.to_string(),
                            member.to_string()
                        ],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .unwrap();
                assert_eq!(retained.0, "completed_not_delivered");
                assert_eq!(retained.1, expected.projection.latest_turn_id);
                assert!(&retained.2 == capture, "original TaskA capture changed");
                let captures: u64 = c.query_row(
                    "SELECT count(*) FROM policy_task_dispatch_captures p JOIN brokered_tasks t USING(task_id)
                     WHERE t.target_run_id=?1 AND (p.writer_capture_json IS NOT NULL
                       OR p.writer_capture_sha256 IS NOT NULL OR p.writer_released!=0)",
                    [member.to_string()], |r|r.get(0)).unwrap();
                assert_eq!(captures, 0);
                current
            };
            let mut busy_count = 0u64;
            let mut first_busy: Option<crate::machine::FailureEnvelope> = None;
            loop {
                assert!(
                    Instant::now() < until,
                    "original phaseA release budget expired"
                );
                let before = observe();
                let mut command = std::process::Command::new(root.join("current-main"));
                command.args([
                    "run",
                    "release-write",
                    &member.to_string(),
                    "--workspace",
                    workspace.to_str().unwrap(),
                    "--controller-fd",
                    "0",
                ]);
                command.stdin(std::process::Stdio::from(
                    DarwinSystem
                        .duplicate_fd_cloexec(child_owner.raw_fd())
                        .unwrap(),
                ));
                let output = command.output().unwrap();
                if output.status.success() {
                    let success: crate::machine::SuccessEnvelope =
                        serde_json::from_slice(&output.stdout).unwrap();
                    assert!(
                        success.ok
                            && success.schema_version == 4
                            && success.command == "run.release-write"
                    );
                    release_observation = json!({"busy_refusals":busy_count,"first_busy":first_busy,"release_succeeded":true});
                    private(
                        &root.join("writer-release-observations.json"),
                        &serde_json::to_vec(&release_observation).unwrap(),
                    );
                    assert!(
                        Instant::now() < until,
                        "release exceeded original phaseA budget"
                    );
                    break;
                }
                private(&root.join("writer-release-refusal.stdout"), &output.stdout);
                private(&root.join("writer-release-refusal.stderr"), &output.stderr);
                let failure: crate::machine::FailureEnvelope =
                    serde_json::from_slice(&output.stdout).unwrap();
                assert!(
                    !failure.ok
                        && failure.schema_version == 2
                        && failure.command == "run.release_write"
                );
                assert!(
                    failure.error.code == "RUN_BUSY"
                        && failure.error.retryable
                        && failure.error.details["owner_kind"] == "startup"
                        && failure.error.details["run_id"] == member.to_string(),
                    "release was not typed pre-effect startup Busy"
                );
                let after = observe();
                assert!(
                    after.writer == before.writer,
                    "Writer changed during startup Busy"
                );
                assert_eq!(after.controller, before.controller);
                assert!(
                    after.runtime_record == before.runtime_record,
                    "runtime changed during startup Busy"
                );
                assert_eq!(
                    after.captured_effective_policy,
                    before.captured_effective_policy
                );
                busy_count += 1;
                if first_busy.is_none() {
                    first_busy = Some(failure.clone());
                }
                private(&root.join("writer-release-observations.json"), &serde_json::to_vec(
                    &json!({"busy_refusals":busy_count,"first_busy":first_busy,"last_busy":failure,
                        "pre_effect_identity_unchanged":true,"release_succeeded":false})).unwrap());
                assert!(
                    Instant::now() < until,
                    "original phaseA release budget expired"
                );
                std::thread::sleep(
                    Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
                );
            }
            released = RunSnapshot::load(&state, member, 0).unwrap();
            assert_eq!(
                released.writer.state,
                crate::writer::WriterAuthorityState::None
            );
            assert!(released.writer.holder.is_none() && released.writer.transaction_id.is_none());
            assert_eq!(released.captured_effective_policy.access, Access::Read);
            assert_eq!(
                released.captured_effective_policy.verification,
                PolicyVerification::Verified
            );
            assert_eq!(released.projection.thread_id, born.projection.thread_id);
            assert!(
                Instant::now() < until,
                "Reader barrier exceeded original phaseA budget"
            );
            let next = begin("B");
            assert_eq!(wait_file("writer-B-ready.json", next), json!({"phase":"B"}));
            next
        };
        component_writer_public_session(root, view, primary, owner, phase_until);
        let source = RunSnapshot::load(&state, primary, 0).unwrap();
        assert_eq!(source.manifest.controller, initial.manifest.controller);
        assert_eq!(source.projection.thread_id, initial.projection.thread_id);
        assert_eq!(source.projection.lifecycle, RunLifecycle::Running);
        let source_turn = source
            .projection
            .active_turn_id
            .clone()
            .expect("actual active Source Turn");
        assert!(!source_turns.contains(&source_turn));
        source_turns.push(source_turn.clone());
        let context = crate::orchestration::PrimaryCallContext {
            session_id: primary,
            source_run_id: primary,
            source_turn_id: source_turn.clone(),
            source_tool_call_id: format!("fixture-host-policy-writer-{phase}"),
            idempotency_key: format!("fixture-host-policy-writer-{phase}"),
        };
        let mut store = crate::orchestration::OrchestrationStore::open(&state).unwrap();
        let mut effects = crate::semantic::ProductionOrchestrationEffects::new(&state);
        let admitted = store
            .assign_task(
                &context,
                &crate::orchestration::AssignSpecialistTask {
                    specialist_run_id: member,
                    objective: format!("TASK027_WRITER_TASK_{phase}"),
                    context_refs: Vec::new(),
                    expected_output: vec!["Bounded marker output".into()],
                    requested_access: "canonical_workspace_write".into(),
                    deadline_seconds: 60,
                },
                &mut effects,
            )
            .unwrap();
        assert_eq!(admitted.state, "queued");
        let c = store.collaboration_connection().unwrap();
        let capture: String = c
            .query_row(
                "SELECT capture_json FROM policy_task_dispatch_captures WHERE task_id=?1",
                [admitted.task_id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let capture_value: Value = serde_json::from_str(&capture).unwrap();
        let deadline = capture_value["deadline_ms"].as_i64().unwrap();
        let effort = capture_value["effort"].as_str().unwrap().to_owned();
        let mut snapshot_conflicts = 0u64;
        let mut snapshot_observation = json!({"count":0,"first":null,"last":null,
            "type":{"code":"RUN_STATE_CONFLICT","retryable":false,"operation":"capture_snapshot",
                "run_id":member,"state":"idle","details_fields":3},"success":false,"success_scope":"task_wait_full_snapshot"});
        let observation_path = root.join(format!("writer-{phase}-snapshot-observation.json"));
        private(
            &observation_path,
            &serde_json::to_vec(&snapshot_observation).unwrap(),
        );
        let (turn, after) = loop {
            assert!(
                Instant::now() < phase_until,
                "original Writer task deadline elapsed"
            );
            let (status, turn): (String, Option<String>) = c
                .query_row(
                    "SELECT state,target_turn_id FROM brokered_tasks WHERE task_id=?1",
                    [admitted.task_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(
                !matches!(
                    status.as_str(),
                    "failed" | "cancelled" | "expired" | "interrupted_unknown"
                ),
                "original task did not complete"
            );
            assert!(
                Instant::now() < phase_until,
                "original snapshot observation deadline elapsed"
            );
            let snapshot = match RunSnapshot::load(&state, member, 0) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    let permitted = error.code == "RUN_STATE_CONFLICT"
                        && !error.retryable
                        && error.details.as_object().is_some_and(|details| {
                            details.len() == 3
                                && details.get("operation") == Some(&json!("capture_snapshot"))
                                && details.get("run_id") == Some(&json!(member))
                                && details.get("state") == Some(&json!("idle"))
                        });
                    if !permitted {
                        snapshot_observation["other_error"] = json!({"code":error.code,
                            "retryable":error.retryable,"operation":error.details.get("operation"),
                            "run_id":error.details.get("run_id"),"state":error.details.get("state")});
                        private(
                            &observation_path,
                            &serde_json::to_vec(&snapshot_observation).unwrap(),
                        );
                        panic!("unexpected full-snapshot capture error: {}", error.code);
                    }
                    snapshot_conflicts += 1;
                    let encoded = serde_json::to_value(&error).unwrap();
                    snapshot_observation["count"] = json!(snapshot_conflicts);
                    if snapshot_observation["first"].is_null() {
                        snapshot_observation["first"] = encoded.clone();
                    }
                    snapshot_observation["last"] = encoded;
                    private(
                        &observation_path,
                        &serde_json::to_vec(&snapshot_observation).unwrap(),
                    );
                    assert!(
                        Instant::now() < phase_until,
                        "original snapshot observation deadline elapsed"
                    );
                    std::thread::sleep(
                        Duration::from_millis(20)
                            .min(phase_until.saturating_duration_since(Instant::now())),
                    );
                    continue;
                }
            };
            assert!(
                Instant::now() < phase_until,
                "full snapshot exceeded original phase deadline"
            );
            if status == "completed_not_delivered"
                && snapshot.projection.lifecycle == RunLifecycle::Idle
                && (phase == "A"
                    || (snapshot.writer.state == crate::writer::WriterAuthorityState::None
                        && snapshot.writer.holder.is_none()
                        && snapshot.writer.transaction_id.is_none()))
            {
                snapshot_observation["success"] = json!(true);
                private(
                    &observation_path,
                    &serde_json::to_vec(&snapshot_observation).unwrap(),
                );
                break (turn.unwrap(), snapshot);
            }
            assert!(
                Instant::now() < phase_until,
                "original Writer task deadline elapsed"
            );
            std::thread::sleep(
                Duration::from_millis(20)
                    .min(phase_until.saturating_duration_since(Instant::now())),
            );
        };
        assert!(!target_turns.contains(&turn));
        target_turns.push(turn.clone());
        assert_eq!(after.projection.thread_id, born.projection.thread_id);
        assert_eq!(
            fs::read(workspace.join(format!("caller-policy-writer-{phase}.txt"))).unwrap(),
            format!("writer-{phase}\n").as_bytes()
        );
        let retained: String = c
            .query_row(
                "SELECT capture_json FROM policy_task_dispatch_captures WHERE task_id=?1",
                [admitted.task_id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, capture);
        let (writer_capture,writer_hash,writer_released):(Option<String>,Option<String>,u64)=c.query_row("SELECT writer_capture_json,writer_capture_sha256,writer_released FROM policy_task_dispatch_captures WHERE task_id=?1",[admitted.task_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        let ledger = after.observed_ledger(&state).unwrap();
        let starts = ledger
            .records()
            .iter()
            .filter(|r| {
                r.kind() == crate::audit::AuditKind::TurnStarted
                    && r.payload_value().is_ok_and(|v| v["turn_id"] == turn)
            })
            .count();
        assert_eq!(starts, 1);
        if phase == "A" {
            assert_eq!(
                after.writer.state,
                crate::writer::WriterAuthorityState::Active
            );
            assert_eq!(
                after.writer.writer_generation,
                born.writer.writer_generation
            );
            let mut before_owner = born.writer.holder.clone().unwrap();
            let mut after_owner = after.writer.holder.clone().unwrap();
            before_owner.last_event_cursor = None;
            after_owner.last_event_cursor = None;
            assert_eq!(after_owner, before_owner);
            assert!(after.writer.transaction_id.is_none());
            assert!(writer_capture.is_none() && writer_hash.is_none());
            assert_eq!(writer_released, 0);
        } else {
            let text = writer_capture.unwrap();
            let hash = writer_hash.unwrap();
            assert_eq!(crate::jcs::sha256_hex(text.as_bytes()), hash);
            let acquisition: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                acquisition["writer_acquire"]["work"]["work"],
                admitted.task_id.to_string()
            );
            assert_eq!(
                acquisition["writer_acquire"]["work"]["target"],
                member.to_string()
            );
            assert_eq!(
                acquisition["writer_acquire"]["work"]["scope_ref"],
                primary.to_string()
            );
            let claim = acquisition["writer_acquire"]["work"]["claim"]
                .as_str()
                .unwrap();
            assert_eq!(Uuid::parse_str(claim).unwrap().get_version_num(), 7);
            let transaction = acquisition["writer_acquire"]["transaction_id"]
                .as_str()
                .unwrap();
            assert_eq!(
                ledger
                    .records()
                    .iter()
                    .filter(|r| r.kind() == crate::audit::AuditKind::WriterAcquired
                        && r.payload_value()
                            .is_ok_and(|v| v["transaction_id"] == transaction))
                    .count(),
                1
            );
            for kind in ["policy_task_writer_captured", "policy_task_writer_released"] {
                assert_eq!(c.query_row("SELECT count(*) FROM orchestration_events WHERE session_id=?1 AND kind=?2 AND payload_sha256=?3",rusqlite::params![primary.to_string(),kind,hash],|r|r.get::<_,u64>(0)).unwrap(),1);
            }
            assert_eq!(writer_released, 1);
            assert_eq!(after.captured_effective_policy.access, Access::Read);
            assert_eq!(
                after.captured_effective_policy.verification,
                PolicyVerification::Verified
            );
        }
        if phase == "A" {
            completed_a = Some((admitted.task_id, capture.clone(), after.clone()));
        }
        proofs.push(json!({"phase":phase,"task_id":admitted.task_id,"target_turn":turn,"source_run":primary,"source_turn":source_turn,"source_thread":source.projection.thread_id,"source_generation":source.projection.run_generation,"source_thread_generation":source.captured_effective_policy.thread_generation,"source_controller_id":source.controller.controller_id,"source_controller_generation":source.controller.generation,"source_call":context.source_tool_call_id,"authority_kind":"synthetic_trusted_host_fixture","capture_sha256":crate::jcs::sha256_hex(capture.as_bytes()),"original_deadline_ms":deadline,"original_effort":effort,"member_generation":after.projection.run_generation,"writer_generation":after.writer.writer_generation,"writer_revision":after.writer.authority_revision,"task_writer_released":writer_released,"snapshot_observation":snapshot_observation}));
        drop(store);
        release_source(phase);
        wait_idle(phase_until);
    }
    component_cli(
        root,
        &[
            "run",
            "close",
            &primary.to_string(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--controller-fd",
            "0",
            "--interrupt",
        ],
        Some(owner),
    );
    let closed = RunSnapshot::load(&state, member, 0).unwrap();
    assert_eq!(closed.projection.lifecycle, RunLifecycle::Closed);
    assert_eq!(
        membership(&home, &closed.manifest),
        MembershipDisposition::Released
    );
    component_cli(
        root,
        &[
            "profile",
            "server",
            "stop",
            PROFILE,
            "--confirm-server-key",
            &binding.server_key,
            "--operator-file",
            root.join("operator").to_str().unwrap(),
        ],
        None,
    );
    private(&root.join("cleanup-observations.json"),&serde_json::to_vec(&json!([{"run_id":member,"stage":"member_release","ok":true},{"stage":"profile_retire","ok":true,"main_owned":true}])).unwrap());
    json!({"schema":"dolgorae.native-policy-writer-component/v1","passed":true,"authority_kind":"synthetic_trusted_host_fixture","normal_primary":primary,"member":member,"responses_posts":7,"native_turns":4,"primary_turns":source_turns,"target_turns":target_turns,"phases":proofs,"member_thread":born.projection.thread_id,"initial_generation":born.projection.run_generation,"explicit_reader_generation":released.projection.run_generation,"writer_release_barrier":release_observation,"normal_model_write_assignment_proven":false,"all_model_source_authorization_proven":false,"owned_cleanup_verified":true,"ordinary_session_completion_proven":false,"cleanup_kind":"owned_abort"})
}

fn component_writer_public_session(
    root: &Path,
    view: &WorkspaceView,
    run: Uuid,
    owner: &CredentialCarrier,
    until: Instant,
) {
    use crate::protocol::public_v2 as pb;
    let home = DolgoraeHome::system().unwrap();
    let dir = home
        .root()
        .join("controller-carriers/native-component/writer");
    for path in [
        home.root().join("controller-carriers"),
        home.root().join("controller-carriers/native-component"),
        dir.clone(),
    ] {
        if !path.exists() {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let carrier = dir.join("controller.json");
    if !carrier.exists() {
        fs::copy(root.join("controller"), &carrier).unwrap();
        fs::set_permissions(&carrier, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let binding = crate::controller::binding_from_carrier(owner, 1).unwrap();
    let path = root.join("gateway.sock");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            tokio::time::timeout(until.saturating_duration_since(Instant::now()), async {
                let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
                    .unwrap()
                    .connect_with_connector(tower::service_fn(move |_| {
                        let path = path.clone();
                        async move {
                            tokio::net::UnixStream::connect(path)
                                .await
                                .map(hyper_util::rt::TokioIo::new)
                        }
                    }))
                    .await
                    .unwrap();
                let context = || {
                    Some(pb::RequestContext {
                        protocol_version: 2,
                        client_request_id: Uuid::now_v7().to_string(),
                        client_instance_id: "native-policy-writer-host".into(),
                    })
                };
                let mut runtime =
                    pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
                let capabilities = runtime
                    .get_capabilities(pb::GetCapabilitiesRequest {
                        context: context(),
                        minimum_protocol_version: 2,
                        maximum_protocol_version: 2,
                    })
                    .await
                    .unwrap()
                    .into_inner();
                let ready: Value =
                    serde_json::from_slice(&fs::read(root.join("foreground-ready.json")).unwrap())
                        .unwrap();
                assert_eq!(
                    capabilities.context.as_ref().unwrap().server_instance_id,
                    ready["data"]["server_instance_id"].as_str().unwrap()
                );
                assert_eq!(
                    capabilities.descriptor_sha256,
                    crate::jcs::sha256_hex(crate::protocol::PUBLIC_V2_DESCRIPTOR)
                );
                let mut client =
                    pb::orchestration_service_client::OrchestrationServiceClient::new(channel);
                let session = client
                    .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
                        context: context(),
                        root_run: Some(pb::RunRef {
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
                            run_id: run.to_string(),
                        }),
                        controller: Some(pb::ControllerCarrierRef {
                            absolute_file_path: carrier.to_str().unwrap().into(),
                            expected_controller_id: binding.identity.controller_id.to_string(),
                            expected_controller_generation: binding.identity.generation,
                        }),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .session
                    .unwrap();
                assert_eq!(session.session_id, run.to_string());
                assert_eq!(session.primary_run.unwrap().run_id, run.to_string());
            })
            .await
            .expect("original Source budget expired during protected observation");
        });
}

fn component_cli(root: &Path, args: &[&str], carrier: Option<&CredentialCarrier>) -> Value {
    let mut command = std::process::Command::new(root.join("current-main"));
    command.args(args);
    if let Some(carrier) = carrier {
        command.stdin(std::process::Stdio::from(
            DarwinSystem.duplicate_fd_cloexec(carrier.raw_fd()).unwrap(),
        ));
    }
    let output = command.output().unwrap();
    if !output.status.success() {
        private(&root.join("component-cli-failure.stdout"), &output.stdout);
        private(&root.join("component-cli-failure.stderr"), &output.stderr);
    }
    assert!(output.status.success(), "current main CLI failed");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    value["data"].clone()
}

fn component_approve(
    root: &Path,
    view: &WorkspaceView,
    run: Uuid,
    owner: &CredentialCarrier,
    id: Uuid,
    until: Instant,
) {
    use crate::protocol::public_v2 as pb;
    let home = DolgoraeHome::system().unwrap();
    let dir = home
        .root()
        .join("controller-carriers/native-component/approval");
    for path in [
        home.root().join("controller-carriers"),
        home.root().join("controller-carriers/native-component"),
        dir.clone(),
    ] {
        if !path.exists() {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let carrier = dir.join("controller.json");
    if !carrier.exists() {
        fs::copy(root.join("controller"), &carrier).unwrap();
        fs::set_permissions(&carrier, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let binding = crate::controller::binding_from_carrier(owner, 1).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let path = root.join("gateway.sock");
    runtime.block_on(async move {
        let channel=tonic::transport::Endpoint::try_from("http://[::]:50051").unwrap().connect_with_connector(tower::service_fn(move |_|{
            let path=path.clone();async move{tokio::net::UnixStream::connect(path).await.map(hyper_util::rt::TokioIo::new)}
        })).await.unwrap();
        let mut runtime_client=pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
        let negotiated=runtime_client.get_capabilities(pb::GetCapabilitiesRequest{context:Some(pb::RequestContext{protocol_version:2,client_request_id:Uuid::now_v7().to_string(),client_instance_id:"native-collaboration-owner".into()}),minimum_protocol_version:2,maximum_protocol_version:2}).await.unwrap().into_inner();
        let negotiated_context=negotiated.context.as_ref().unwrap();
        assert_eq!(negotiated_context.protocol_version,2);
        let ready:Value=serde_json::from_slice(&fs::read(root.join("foreground-ready.json")).unwrap()).unwrap();
        assert_eq!(negotiated_context.server_instance_id,ready["data"]["server_instance_id"].as_str().unwrap());
        assert_eq!(negotiated.descriptor_sha256,crate::jcs::sha256_hex(crate::protocol::PUBLIC_V2_DESCRIPTOR));
        let mut client=pb::interaction_service_client::InteractionServiceClient::new(channel);
        let context=||Some(pb::RequestContext{protocol_version:2,client_request_id:Uuid::now_v7().to_string(),client_instance_id:"native-collaboration-owner".into()});
        let reference=pb::RunRef{workspace:Some(pb::WorkspaceRef{absolute_path:view.canonical_path.to_path_buf().unwrap().to_str().unwrap().into(),expected_workspace_id:view.workspace_id.clone()}),run_id:run.to_string()};
        let controller=pb::ControllerCarrierRef{absolute_file_path:carrier.to_str().unwrap().into(),expected_controller_id:binding.identity.controller_id.to_string(),expected_controller_generation:binding.identity.generation};
        let detail=client.get_controller_interaction(pb::GetControllerInteractionRequest{context:context(),run:Some(reference.clone()),controller:Some(controller.clone()),interaction_id:id.to_string()}).await;
        let detail=match detail {
            Ok(value)=>value.into_inner().interaction.unwrap(),
            Err(status)=>{
                use prost::Message as _;
                let rich=crate::gateway::GoogleRpcStatus::decode(status.details()).unwrap();let error=pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
                assert_eq!(error.dolgorae_error_code,"RECOVERY_REQUIRED","approval read failed for a cause other than its SQL-to-ledger publication gap");
                return;
            }
        };
        assert_eq!(detail.summary.as_ref().unwrap().interaction_id,id.to_string());
        assert!(matches!(detail.payload,Some(pb::controller_interaction::Payload::UserInput(_))));
        let stamp=detail.stamp.unwrap();let snapshot=RunSnapshot::load(&home.workspace_root(&view.workspace_id),run,0).unwrap();
        if stamp.run_state_revision!=snapshot.stamp.run_state_revision{return;}
        let turn=snapshot.projection.active_turn_id.or(snapshot.projection.latest_turn_id).unwrap();
        let result=client.resolve_interaction(pb::ResolveInteractionRequest{context:context(),run:Some(reference.clone()),controller:Some(controller.clone()),interaction_id:id.to_string(),idempotency_key:format!("approve-{id}"),
            expected_run_revision:stamp.run_state_revision,expected_interaction_revision:stamp.interaction_state_revision,expected_turn_id:turn,expected_thread_generation:snapshot.captured_effective_policy.thread_generation.unwrap(),
            answer:Some(pb::InteractionAnswer{decision:pb::InteractionDecision::AcceptOnce as i32,answers:vec![pb::QuestionAnswer{question_id:"specialist_approval".into(),selected_labels:vec!["approve".into()],text:None}],..Default::default()}),
        }).await;
        let result=match result{
            Ok(value)=>value.into_inner(),
            Err(status)=>{
                use prost::Message as _;
                let rich=crate::gateway::GoogleRpcStatus::decode(status.details()).unwrap();let error=pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
                if matches!(error.dolgorae_error_code.as_str(),"INTERACTION_STALE"|"RUN_STATE_CONFLICT") && error.effect==pb::EffectState::None as i32{return;}
                let safe_error=|status:&tonic::Status|{
                    let detail=crate::gateway::GoogleRpcStatus::decode(status.details()).ok()
                        .and_then(|rich|rich.details.first().and_then(|d|pb::DolgoraeErrorDetail::decode(d.value.as_slice()).ok()));
                    match detail {
                        Some(error)=>json!({"grpc_code":format!("{:?}",status.code()),"code":error.dolgorae_error_code,"effect":error.effect,"action":error.action,"retry_classification":error.retry_classification,"recovery_classification":error.recovery_classification,"failure_classification":error.classification}),
                        None=>json!({"grpc_code":format!("{:?}",status.code()),"checked_error_detail":false}),
                    }
                };
                let mut failure=safe_error(&status);
                failure["method"]=json!("InteractionService.ResolveInteraction");
                failure["prior_run_revision"]=json!(stamp.run_state_revision);
                failure["prior_interaction_revision"]=json!(stamp.interaction_state_revision);
                // Preserve the primary refusal even if the bounded diagnostic observation fails.
                let diagnostic=root.join("approval-control-error.json");
                private(&diagnostic,&serde_json::to_vec(&failure).unwrap());
                if let Some(remaining)=until.checked_duration_since(Instant::now()).filter(|remaining|!remaining.is_zero()) {
                    let mut request=tonic::Request::new(pb::GetControllerInteractionRequest{context:context(),run:Some(reference),controller:Some(controller),interaction_id:id.to_string()});
                    request.set_timeout(remaining);
                    failure["same_approval_observation"]=match client.get_controller_interaction(request).await {
                        Ok(response)=>{
                            let interaction=response.into_inner().interaction;
                            match interaction {
                                Some(value)=>json!({"ok":true,"status":value.summary.as_ref().map(|s|s.status),"run_revision":value.stamp.as_ref().map(|s|s.run_state_revision),"interaction_revision":value.stamp.as_ref().map(|s|s.interaction_state_revision)}),
                                None=>json!({"ok":false,"checked_interaction":false}),
                            }
                        },
                        Err(observation)=>safe_error(&observation),
                    };
                } else {
                    failure["same_approval_observation"]=json!({"attempted":false,"original_phase_deadline_elapsed":true});
                }
                private(&diagnostic,&serde_json::to_vec(&failure).unwrap());
                panic!("protected approval effect did not settle its original receipt");
            }
        };
        let store=crate::orchestration::OrchestrationStore::open_observer(&home.workspace_root(&view.workspace_id)).unwrap();
        let original=store.approval_interaction(run,id).unwrap().unwrap();
        assert_eq!(result.resolution_receipt,original.resolution_receipt_id.unwrap().to_string());
        assert_eq!(original.operation_state,"ready");
        let operation=store.specialist_operations(run,&[original.operation_id]).unwrap().remove(0);
        assert_eq!(operation.operation_id,original.operation_id);
        assert_eq!(operation.approval_request_id,Some(id));
        assert_eq!(operation.state,"ready");
        let pending=root.join(format!("approval-{id}.pending"));
        private(&pending,&serde_json::to_vec(&json!({"approval_id":id,"operation_id":original.operation_id,"original_sql_receipt":original.resolution_receipt_id,"protected_current_response":true,"operation_state":operation.state,"member_run_id":operation.specialist_run_id.unwrap()})).unwrap());
        fs::rename(pending,root.join(format!("approval-{id}.json"))).unwrap();
    });
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
    async_runtime.block_on(runtime.shutdown(tokio::time::Instant::now() + Duration::from_secs(5)));
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

// Caller policy members are owned by the original Primary Session. Its
// authenticated main-CLI close settles them; the driver never impersonates a
// peer Controller or sends its different ExecutingBuild to a Run Worker.
fn cleanup_collaboration_main(root: &Path) -> bool {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-successor-pipeline/v1\n"
    );
    let home = DolgoraeHome::system().unwrap();
    assert_eq!(home.root(), root.join("home/.dolgorae"));
    let mut observations = Vec::new();
    let mut clean = true;
    let close = (|| -> Result<(), String> {
        let workspaces = match fs::read_dir(home.root().join("workspaces")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                observations.push(json!({"stage":"workspace_initialization","applicability":"absent_before_initialization"}));
                return Ok(());
            }
            Err(error) => return Err(format!("workspace_read:{:?}", error.kind())),
        };
        for entry in workspaces {
            let state = entry
                .map_err(|e| format!("workspace_entry:{:?}", e.kind()))?
                .path();
            let store = RunStore::new(SystemWorkspacePlatform, &state);
            let entries = match fs::read_dir(state.join("runs")) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    observations.push(json!({"stage":"run_initialization","applicability":"absent_before_run_start"}));
                    continue;
                }
                Err(error) => return Err(format!("run_read:{:?}", error.kind())),
            };
            let mut manifests = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|e| format!("run_entry:{:?}", e.kind()))?;
                let id = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or("unknown_run_entry")?;
                manifests.push(store.load_manifest(id).map_err(|e| e.code)?);
            }
            for manifest in &manifests {
                if manifest.schema_version != 3 {
                    return Err("unexpected_manifest_generation".into());
                }
                let aggregate = manifest
                    .aggregate_binding
                    .as_ref()
                    .ok_or("missing_session_binding")?;
                if aggregate.member_kind != crate::run::AggregateMemberKind::Primary {
                    continue;
                }
                let output = std::process::Command::new(root.join("current-main"))
                    .args([
                        "run",
                        "close",
                        &manifest.run_id.to_string(),
                        "--workspace",
                        manifest
                            .canonical_workspace
                            .to_path_buf()
                            .map_err(|e| e.code)?
                            .to_str()
                            .ok_or("workspace_not_utf8")?,
                        "--controller-file",
                        root.join("controller").to_str().unwrap(),
                        "--interrupt",
                    ])
                    .output()
                    .map_err(|e| format!("main_close:{:?}", e.kind()))?;
                let value: Value = serde_json::from_slice(&output.stdout)
                    .map_err(|_| "invalid_main_close_response")?;
                observations.push(json!({"stage":"primary_session_close","run_id":manifest.run_id,"ok":output.status.success(),"response":value}));
                if !output.status.success() || value["ok"] != true {
                    return Err("original_main_close_refused".into());
                }
            }
            for manifest in &manifests {
                let snapshot = RunSnapshot::load(&state, manifest.run_id, 0).map_err(|e| e.code)?;
                if snapshot.projection.lifecycle != RunLifecycle::Closed {
                    return Err("run_not_closed".into());
                }
                let binding = manifest
                    .global_profile_binding
                    .as_ref()
                    .ok_or("missing_profile_binding")?;
                let membership =
                    GlobalMembershipStore::new(&home, &binding.selected_name, &binding.server_key)
                        .map_err(|e| e.code)?
                        .load()
                        .map_err(|e| e.code)?;
                let released = membership
                    .members
                    .get(&format!("{}:{}", manifest.workspace_id, manifest.run_id))
                    .is_some_and(|m| m.disposition == MembershipDisposition::Released);
                observations
                    .push(json!({"stage":"member_release","run_id":manifest.run_id,"ok":released}));
                if !released {
                    return Err("membership_release_unproved".into());
                }
            }
        }
        Ok(())
    })();
    if let Err(reason) = close {
        clean = false;
        observations.push(json!({"stage":"main_owner_cleanup","ok":false,"reason":reason}));
    }
    match fs::read(root.join("qualified-binding.json")) {
        Ok(bytes) => {
            if let Ok(binding) =
                serde_json::from_slice::<crate::global_runtime::GlobalProfileBinding>(&bytes)
            {
                let retired = std::process::Command::new(root.join("current-main"))
                    .args([
                        "profile",
                        "server",
                        "stop",
                        PROFILE,
                        "--confirm-server-key",
                        &binding.server_key,
                        "--operator-file",
                        root.join("operator").to_str().unwrap(),
                    ])
                    .output();
                let ok = retired.as_ref().is_ok_and(|o| {
                    o.status.success()
                        && serde_json::from_slice::<Value>(&o.stdout).is_ok_and(|v| v["ok"] == true)
                });
                clean &= ok;
                observations.push(json!({"stage":"profile_retire","ok":ok,"main_owned":true}));
            } else {
                clean = false;
                observations.push(json!({"stage":"profile_binding","ok":false,"reason":"invalid_retained_binding"}));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            observations.push(
                json!({"stage":"profile_candidate","applicability":"absent_before_candidate"}),
            );
        }
        Err(error) => {
            clean = false;
            observations.push(
                json!({"stage":"profile_binding","ok":false,"reason":format!("{:?}",error.kind())}),
            );
        }
    }
    private(
        &root.join("cleanup-observations.json"),
        &serde_json::to_vec(&observations).unwrap(),
    );
    clean
}

pub fn cleanup(root: &Path) -> bool {
    if root.join("component-policy-case.json").is_file() {
        return cleanup_collaboration_main(root);
    }
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
