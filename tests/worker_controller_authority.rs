//! ADR-016 worker-side Controller authority.
//!
//! The CLI's own credential check is only an early rejection. These cases hold
//! the authoritative check to its contract: the hidden worker rereads the
//! descriptor it received over `SCM_RIGHTS` and revalidates it against the
//! Run's reset-journal-reconciled binding under the Run mutation lock, before
//! any effect, while same-uid observation stays open without a credential.

use dolgorae::controller::{CredentialCarrier, create_controller_credential};
use dolgorae::domain::{
    Access, AggregateKind, Assurance, ControlMode, ControllerIdentity, ControllerKind,
    ExecutionLane, Purpose, PurposeKind,
};
use dolgorae::engagement::{EngagementStore, RuntimeOutcome};
use dolgorae::run::{
    AgentConfigurationSnapshot, AggregateBinding, AggregateMemberKind, AppServerFacts, AuditPolicy,
    CapabilityState, CompatibilityVerdict, ControllerBinding, DolgoraeBuild, ExecutableIdentity,
    InstructionSnapshot, ParentReference, ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest,
    RunStore, agent_configuration_digest, launch_contract_digest, runtime_profile_snapshot_digest,
};
use dolgorae::worker::{
    ControlRequestV1, ControlResponseV1, ExecutingBuild, RunControllerAuthority, RunFacts,
    WorkerControlServer, WorkerControlState, WorkerHello, WorkerIdentity, bind_worker_socket,
    read_frame, write_control_request,
};
use dolgorae::workspace::{GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode};
use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("dolgorae-task007-authority-{}", Uuid::now_v7()));
        make_dir(&root);
        make_dir(&root.join("state"));
        make_dir(&root.join("state/runs"));
        make_dir(&root.join("credentials"));
        Self { root }
    }

    fn state_root(&self) -> PathBuf {
        self.root.join("state")
    }

    fn credential(&self, name: &str) -> PathBuf {
        self.root.join("credentials").join(name)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn make_dir(path: &Path) {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Mint a fresh Controller credential and return its already-open carrier
/// alongside the binding a Run published for it would carry.
fn mint(path: &Path, instance: &str) -> (CredentialCarrier, ControllerBinding) {
    mint_kind(path, instance, ControllerKind::HumanCli)
}

fn mint_kind(
    path: &Path,
    instance: &str,
    kind: ControllerKind,
) -> (CredentialCarrier, ControllerBinding) {
    let created =
        create_controller_credential(path, kind, instance.to_owned(), None, None).unwrap();
    let carrier = CredentialCarrier::open_path(path).unwrap();
    let binding = dolgorae::controller::binding_from_carrier(&carrier, 1).unwrap();
    assert_eq!(
        binding.identity.controller_id,
        created.credential.controller_id
    );
    (carrier, binding)
}

#[test]
fn aggregate_owner_delegation_requires_binding_and_active_membership() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, owner_binding) = mint_kind(
        &tree.credential("aggregate-owner.json"),
        "host",
        ControllerKind::WorkflowOrchestrator,
    );
    let (stranger, _) = mint_kind(
        &tree.credential("stranger-owner.json"),
        "other-host",
        ControllerKind::WorkflowOrchestrator,
    );
    let (_child, child_binding) = mint_kind(
        &tree.credential("child.json"),
        "specialist",
        ControllerKind::WorkflowOrchestrator,
    );
    let database = state_root.join("orchestration/orchestration.sqlite3");
    let mut engagements = EngagementStore::open(&database).unwrap();
    let opened = engagements
        .open_external_engagement(
            &"1".repeat(64),
            &owner_binding,
            &serde_json::json!({"namespace":"test","kind":"host","id":"one"}),
            None,
            "open",
        )
        .unwrap();
    let mut run_manifest = manifest(Uuid::now_v7(), child_binding);
    run_manifest.control_mode = ControlMode::ManagedAgent;
    run_manifest.parent_ref = Some(ParentReference {
        namespace: "dolgorae.external-specialist-engagement.v1".to_owned(),
        kind: "specialist".to_owned(),
        id: opened.engagement_id.to_string(),
    });
    run_manifest.agent_configuration.role_reference = Some("researcher".to_owned());
    let hired = engagements
        .reserve_external_hire(
            opened.engagement_id,
            "researcher",
            &run_manifest.agent_configuration,
            "inspect authority",
            "read_only",
            "hire",
        )
        .unwrap();
    run_manifest.run_id = hired.specialist_run_id;
    run_manifest.aggregate_binding = Some(AggregateBinding {
        aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
        aggregate_id: opened.engagement_id,
        operation_id: hired.hire_operation_id,
        member_kind: AggregateMemberKind::Specialist,
        policy_sha256: None,
        role_reference: Some("researcher".to_owned()),
        role_snapshot_sha256: Some(dolgorae::jcs::sha256_hex(b"researcher")),
        agent_configuration_sha256: Some(
            agent_configuration_digest(&run_manifest.agent_configuration).unwrap(),
        ),
    });
    engagements
        .finish_external_hire(&hired, RuntimeOutcome::Accepted, "hire")
        .unwrap();
    publish_manifest(&state_root, &run_manifest);
    let worker = Worker::start(&state_root, hired.specialist_run_id);
    let external_requests = [
        ControlRequestV1::ExternalSubmit {
            expected: worker.identity.clone(),
            caller: None,
            engagement_id: opened.engagement_id,
            request: dolgorae::worker::TurnControlRequest {
                message: "delegated submit".to_owned(),
                idempotency_key: "delegated-submit".to_owned(),
                effort: None,
                images: Vec::new(),
            },
        },
        ControlRequestV1::ExternalInterrupt {
            expected: worker.identity.clone(),
            caller: None,
            engagement_id: opened.engagement_id,
        },
        ControlRequestV1::ExternalClose {
            expected: worker.identity.clone(),
            caller: None,
            engagement_id: opened.engagement_id,
            interrupt: false,
        },
        ControlRequestV1::ExternalSetWriterAccess {
            expected: worker.identity.clone(),
            caller: None,
            engagement_id: opened.engagement_id,
            write: true,
            writer_generation: 1,
            transaction_id: Uuid::now_v7(),
        },
    ];

    for request in &external_requests {
        assert_eq!(
            failure_code(&worker.call(request, Some(&stranger))),
            "CONTROLLER_MISMATCH"
        );
        authorized(&worker.call(request, Some(&owner)));
    }
    assert_eq!(
        failure_code(&worker.call(
            &ControlRequestV1::Interrupt {
                expected: worker.identity.clone(),
                caller: None,
            },
            Some(&owner),
        )),
        "CONTROLLER_MISMATCH",
        "aggregate-owner delegation must not authorize ordinary Run mutations"
    );

    engagements
        .release_external_member(
            opened.engagement_id,
            hired.specialist_run_id,
            "authority test complete",
            "release",
        )
        .unwrap();
    assert_eq!(
        failure_code(&worker.call(&external_requests[1], Some(&owner))),
        "ENGAGEMENT_STATE_CONFLICT"
    );
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

/// A worker control socket serving one Run's authority, with no app-server
/// session attached: authorization is proven before the Run is reached, so a
/// credential that passes reaches the replay-window refusal — the registered,
/// retryable `RUN_BUSY` owned by startup — and one that fails never gets that
/// far.
struct Worker {
    path: PathBuf,
    identity: WorkerIdentity,
    server: WorkerControlServer,
    joiner: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn start(state_root: &Path, run_id: Uuid) -> Self {
        let identity = worker_identity(run_id);
        let lease = bind_worker_socket(&identity, None).unwrap();
        let path = lease.path().to_owned();
        let hello = WorkerHello {
            schema_version: 1,
            identity: identity.clone(),
            control_socket_epoch: 1,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "3".repeat(64),
        };
        let server = WorkerControlServer::new(
            hello,
            RunFacts {
                run_id,
                profile: "default".to_owned(),
            },
            WorkerControlState {
                lifecycle: "idle".to_owned(),
                active_turn: None,
            },
            RunControllerAuthority::new(state_root.to_path_buf(), run_id),
        );
        let serving = server.clone();
        let joiner = thread::spawn(move || {
            serving.serve(&lease).unwrap();
        });
        let worker = Self {
            path,
            identity,
            server,
            joiner: Some(joiner),
        };
        // Readiness is proven with a real observer request rather than a bare
        // connect-and-drop, so the probe never leaves an aborted connection
        // for the accept loop to trip over.
        let hello = ControlRequestV1::Hello {
            expected: worker.identity.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker
            .try_call(&hello)
            .is_some_and(|response| matches!(response, ControlResponseV1::Hello { .. }))
        {
            assert!(
                Instant::now() < deadline,
                "control socket never became ready"
            );
            thread::sleep(Duration::from_millis(2));
        }
        worker
    }

    /// One complete request/response exchange, or `None` if the socket is not
    /// answering yet.
    fn try_call(&self, request: &ControlRequestV1) -> Option<ControlResponseV1> {
        let mut stream = UnixStream::connect(&self.path).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        write_control_request(&mut stream, &declared(request), None).ok()?;
        read_frame(&mut BufReader::new(stream)).ok()
    }

    fn call(
        &self,
        request: &ControlRequestV1,
        credential: Option<&CredentialCarrier>,
    ) -> ControlResponseV1 {
        let mut stream = UnixStream::connect(&self.path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_control_request(
            &mut stream,
            &declared(request),
            credential.map(CredentialCarrier::raw_fd),
        )
        .unwrap();
        read_frame(&mut BufReader::new(stream)).unwrap()
    }

    /// One exchange whose request is written exactly as given, so a case can
    /// declare a build this worker does not share — or none at all.
    fn call_exact(&self, request: &ControlRequestV1) -> ControlResponseV1 {
        let mut stream = UnixStream::connect(&self.path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_control_request(&mut stream, request, None).unwrap();
        read_frame(&mut BufReader::new(stream)).unwrap()
    }

    /// Present a raw descriptor the caller opened itself, so a carrier the
    /// worker never validated still has to survive the worker's own checks.
    fn call_with_raw_fd(&self, request: &ControlRequestV1, fd: i32) -> ControlResponseV1 {
        let mut stream = UnixStream::connect(&self.path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_control_request(&mut stream, &declared(request), Some(fd)).unwrap();
        read_frame(&mut BufReader::new(stream)).unwrap()
    }

    fn send(&self) -> ControlRequestV1 {
        ControlRequestV1::Send {
            caller: None,
            expected: self.identity.clone(),
            request: dolgorae::worker::TurnControlRequest {
                message: "authorized?".to_owned(),
                idempotency_key: "authority-probe".to_owned(),
                effort: None,
                images: Vec::new(),
            },
            timeout_ms: None,
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.server.stop();
        if let Some(joiner) = self.joiner.take() {
            let _ = joiner.join();
        }
    }
}

fn failure_code(response: &ControlResponseV1) -> &str {
    match response {
        ControlResponseV1::Failed { code, .. } => code,
        other => panic!("expected a failure response, got {other:?}"),
    }
}

fn authorized(response: &ControlResponseV1) {
    let ControlResponseV1::Failed {
        code,
        retryable,
        details,
        ..
    } = response
    else {
        panic!("expected a failure response, got {response:?}");
    };
    assert_eq!(
        code, "RUN_BUSY",
        "credential was refused before it could reach the Run: {response:?}"
    );
    assert!(retryable, "the replay window is retryable: {response:?}");
    assert_eq!(
        details["owner_kind"], "startup",
        "the replay window is owned by startup: {response:?}"
    );
}

#[test]
fn worker_revalidates_every_mutating_request_against_the_current_binding() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, binding) = mint(&tree.credential("owner.json"), "owner");
    let (stranger, _) = mint(&tree.credential("stranger.json"), "stranger");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding);
    let worker = Worker::start(&state_root, run_id);

    // A direct socket caller that presents no descriptor at all is refused,
    // even though its worker identity is exactly right.
    assert_eq!(
        failure_code(&worker.call(&worker.send(), None)),
        "CONTROLLER_MISMATCH"
    );

    // A well-formed credential that this Run is not bound to is refused with
    // the same non-oracular code.
    assert_eq!(
        failure_code(&worker.call(&worker.send(), Some(&stranger))),
        "CONTROLLER_MISMATCH"
    );

    // The Run's own credential passes authorization and reaches the Run.
    authorized(&worker.call(&worker.send(), Some(&owner)));

    // Every mutating verb takes the same gate, not just `send`.
    for request in [
        ControlRequestV1::Submit {
            caller: None,
            expected: worker.identity.clone(),
            request: dolgorae::worker::TurnControlRequest {
                message: "submit".to_owned(),
                idempotency_key: "authority-submit".to_owned(),
                effort: None,
                images: Vec::new(),
            },
        },
        ControlRequestV1::Respond {
            caller: None,
            expected: worker.identity.clone(),
            request_id: 7001,
            idempotency_key: "approval-7001".to_owned(),
            response: serde_json::json!({"decision": "accept_once"}),
        },
        ControlRequestV1::Interrupt {
            caller: None,
            expected: worker.identity.clone(),
        },
        ControlRequestV1::Close {
            caller: None,
            expected: worker.identity.clone(),
            interrupt: true,
        },
    ] {
        assert_eq!(
            failure_code(&worker.call(&request, None)),
            "CONTROLLER_MISMATCH",
            "{request:?} accepted a mutation with no credential"
        );
        assert_eq!(
            failure_code(&worker.call(&request, Some(&stranger))),
            "CONTROLLER_MISMATCH",
            "{request:?} accepted a foreign credential"
        );
        authorized(&worker.call(&request, Some(&owner)));
    }
}

#[test]
fn observers_stay_open_to_any_same_uid_caller_without_a_credential() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (_owner, binding) = mint(&tree.credential("owner.json"), "owner");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding);
    let worker = Worker::start(&state_root, run_id);

    assert!(matches!(
        worker.call(
            &ControlRequestV1::Hello {
                expected: worker.identity.clone()
            },
            None
        ),
        ControlResponseV1::Hello { .. }
    ));
    assert!(matches!(
        worker.call(
            &ControlRequestV1::Status {
                expected: worker.identity.clone()
            },
            None
        ),
        ControlResponseV1::Status { .. }
    ));
    // `events` and `wait` reach their own "no run yet" answers rather than a
    // credential refusal, which is the property that must not regress.
    for observer in [
        ControlRequestV1::Events {
            caller: None,
            expected: worker.identity.clone(),
            after: 0,
            projection: dolgorae::event::EventProjection::Operational,
            limit: 8,
        },
        ControlRequestV1::Wait {
            caller: None,
            expected: worker.identity.clone(),
            turn_id: "turn-1".to_owned(),
            timeout_ms: None,
        },
    ] {
        authorized(&worker.call(&observer, None));
    }
}

#[test]
fn a_controller_reset_strands_the_previous_credential_at_the_worker() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (outgoing, binding) = mint(&tree.credential("outgoing.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding.clone());
    let worker = Worker::start(&state_root, run_id);
    authorized(&worker.call(&worker.send(), Some(&outgoing)));

    // A completed reset republishes `controller.json` at the next generation.
    let (incoming, replacement) = mint(&tree.credential("incoming.json"), "cli");
    let next = ControllerBinding {
        identity: ControllerIdentity {
            generation: binding.identity.generation + 1,
            ..replacement.identity
        },
        capability_sha256: replacement.capability_sha256,
    };
    write_binding(&state_root, run_id, &next);

    // The worker was never restarted, yet the stranded credential stops
    // authorizing the moment the durable binding advances.
    assert_eq!(
        failure_code(&worker.call(&worker.send(), Some(&outgoing))),
        "CONTROLLER_MISMATCH"
    );
    authorized(&worker.call(&worker.send(), Some(&incoming)));
}

#[test]
fn an_unresolved_reset_prepare_fails_mutations_closed() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, binding) = mint(&tree.credential("owner.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding.clone());
    let worker = Worker::start(&state_root, run_id);
    authorized(&worker.call(&worker.send(), Some(&owner)));

    // A crash between the binding write and the journal's commit leaves a
    // trailing "prepared" record naming an older generation than the binding
    // on disk. The binding is then unproven, so mutations must stop.
    let advanced = ControllerBinding {
        identity: ControllerIdentity {
            generation: binding.identity.generation + 1,
            ..binding.identity.clone()
        },
        capability_sha256: binding.capability_sha256.clone(),
    };
    write_binding(&state_root, run_id, &advanced);
    let journal = state_root
        .join("runs")
        .join(run_id.to_string())
        .join("recovery/controller-reset.jsonl");
    write_private(
        &journal,
        format!(
            "{}\n",
            serde_json::json!({
                "status": "prepared",
                "controller_generation": binding.identity.generation,
            })
        )
        .as_bytes(),
    );

    let refused = worker.call(&worker.send(), Some(&owner));
    assert_eq!(
        failure_code(&refused),
        "RECOVERY_REQUIRED",
        "an unproven binding must refuse mutations under its own recovery code"
    );
    let ControlResponseV1::Failed {
        details, retryable, ..
    } = &refused
    else {
        panic!("expected a failure");
    };
    assert!(!retryable, "a durable inconsistency is never retryable");
    assert_eq!(details["identity_verdict"], "Unverifiable");
    assert_eq!(
        details["reason"], "controller_binding_newer_than_reset_journal",
        "the refusal names what could not be proved"
    );
}

/// Write the fsynced reset token an operator PREPARE leaves behind, naming the
/// generation it is replacing — exactly what `reset_controller` writes.
fn write_reset_prepare(state_root: &Path, run_id: Uuid, controller_generation: u64) {
    write_private(
        &state_root
            .join("runs")
            .join(run_id.to_string())
            .join("recovery/controller-reset.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema_version": 1,
                "operation_id": Uuid::now_v7(),
                "status": "prepared",
                "run_id": run_id,
                "controller_generation": controller_generation,
            })
        )
        .as_bytes(),
    );
}

fn resolve_reset_prepare(state_root: &Path, run_id: Uuid, controller_generation: u64) {
    let path = state_root
        .join("runs")
        .join(run_id.to_string())
        .join("recovery/controller-reset.jsonl");
    let mut journal = fs::read_to_string(&path).unwrap();
    journal.push_str(&format!(
        "{}\n",
        serde_json::json!({
            "schema_version": 1,
            "operation_id": Uuid::now_v7(),
            "status": "failed",
            "controller_generation": controller_generation,
        })
    ));
    write_private(&path, journal.as_bytes());
}

#[test]
fn a_reset_fence_is_refused_until_the_operator_prepare_is_durable() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, binding) = mint(&tree.credential("owner.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding.clone());
    let worker = Worker::start(&state_root, run_id);

    let fence = ControlRequestV1::ResetFence {
        caller: None,
        expected: worker.identity.clone(),
        confirmation: run_id,
    };

    // Nothing durable has fenced this Run, so a same-uid caller cannot make
    // the worker answer for it. No Controller credential is involved either
    // way: the operator performing a reset is precisely the caller who may
    // not hold one.
    let refused = worker.call(&fence, None);
    assert_eq!(failure_code(&refused), "CONTROLLER_RESET_NOT_ALLOWED");
    let ControlResponseV1::Failed { details, .. } = &refused else {
        panic!("expected a failure");
    };
    assert_eq!(
        details["blockers"],
        serde_json::json!(["reset_prepare_absent"])
    );

    // A confirmation that names another Run is refused before anything else.
    let wrong = worker.call(
        &ControlRequestV1::ResetFence {
            caller: None,
            expected: worker.identity.clone(),
            confirmation: Uuid::now_v7(),
        },
        None,
    );
    assert_eq!(failure_code(&wrong), "INVALID_ARGUMENT");

    // Mutations are unaffected while no reset is in flight.
    authorized(&worker.call(&worker.send(), Some(&owner)));

    // Once the operator's PREPARE is fsynced, the same request is admitted and
    // reaches the Run.
    write_reset_prepare(&state_root, run_id, binding.identity.generation);
    let admitted = worker.call(&fence, None);
    assert_eq!(
        failure_code(&admitted),
        "RUN_BUSY",
        "the fence reached the Run; this worker has no session to answer from yet"
    );
}

#[test]
fn a_durable_reset_prepare_fences_every_mutation_until_it_resolves() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, binding) = mint(&tree.credential("owner.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding.clone());
    let worker = Worker::start(&state_root, run_id);
    authorized(&worker.call(&worker.send(), Some(&owner)));

    // The PREPARE names the generation it is replacing, so the binding on disk
    // is not "newer" than the journal — and the token must still stop every
    // mutation. This is the half of "Operator, run startup/mutation" that
    // reaches a worker in another process.
    write_reset_prepare(&state_root, run_id, binding.identity.generation);
    let fenced = worker.call(&worker.send(), Some(&owner));
    // `CONTROLLER_RESET_NOT_ALLOWED` answers `run controller reset`; a
    // mutation that loses to the reset's durable hold on this Run's
    // startup/mutation serialization is busy, and may come back.
    assert_eq!(failure_code(&fenced), "RUN_BUSY");
    let ControlResponseV1::Failed {
        details, retryable, ..
    } = &fenced
    else {
        panic!("expected a failure");
    };
    assert!(retryable, "a fence lifts when the reset resolves");
    assert_eq!(
        details["owner_kind"], "startup",
        "a prepare in flight is a fence, not a corrupt binding"
    );

    // A reset that rolls back lifts its own fence.
    resolve_reset_prepare(&state_root, run_id, binding.identity.generation);
    authorized(&worker.call(&worker.send(), Some(&owner)));
}

#[test]
fn the_open_descriptor_outlives_replacement_of_its_own_path() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let path = tree.credential("owner.json");
    let (owner, binding) = mint(&path, "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding);
    let worker = Worker::start(&state_root, run_id);

    // Replace the pathname with a different credential entirely. Authority
    // follows the open descriptor the caller passed, not whatever the name
    // resolves to when the request lands.
    fs::remove_file(&path).unwrap();
    let (usurper, _) = mint(&path, "usurper");
    authorized(&worker.call(&worker.send(), Some(&owner)));
    assert_eq!(
        failure_code(&worker.call(&worker.send(), Some(&usurper))),
        "CONTROLLER_MISMATCH"
    );

    // The same holds for a raw descriptor the worker opens from the wire: the
    // replaced pathname cannot smuggle the old authority back in.
    let reopened = fs::File::open(&path).unwrap();
    assert_eq!(
        failure_code(&worker.call_with_raw_fd(&worker.send(), reopened.as_raw_fd())),
        "CONTROLLER_MISMATCH"
    );
}

#[test]
fn an_unsafe_descriptor_never_becomes_a_carrier() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (_owner, binding) = mint(&tree.credential("owner.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding);
    let worker = Worker::start(&state_root, run_id);

    // A world-readable file, and a directory, are both refused as carriers
    // rather than parsed, so a descriptor is never trusted for being present.
    let loose = tree.root.join("loose.json");
    write_private(&loose, b"{}");
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    let loose_file = fs::File::open(&loose).unwrap();
    assert_eq!(
        failure_code(&worker.call_with_raw_fd(&worker.send(), loose_file.as_raw_fd())),
        "CONTROLLER_MISMATCH"
    );

    let directory = fs::File::open(&tree.root).unwrap();
    assert_eq!(
        failure_code(&worker.call_with_raw_fd(&worker.send(), directory.as_raw_fd())),
        "CONTROLLER_MISMATCH"
    );
}

fn write_private(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        make_dir(parent);
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Republish `controller.json` the way a committed controller reset does.
fn write_binding(state_root: &Path, run_id: Uuid, binding: &ControllerBinding) {
    write_private(
        &state_root
            .join("runs")
            .join(run_id.to_string())
            .join("controller.json"),
        &serde_json::to_vec(binding).unwrap(),
    );
}

fn publish(state_root: &Path, run_id: Uuid, controller: ControllerBinding) {
    publish_manifest(state_root, &manifest(run_id, controller));
}

fn publish_manifest(state_root: &Path, manifest: &RunManifest) {
    RunStore::new(SystemWorkspacePlatform, state_root)
        .publish(manifest)
        .unwrap();
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
        model: "gpt-5.6".to_owned(),
        default_effort: "high".to_owned(),
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
        model: "gpt-5.6".to_owned(),
        initial_reasoning_effort: "high".to_owned(),
        default_reasoning_effort: "high".to_owned(),
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

/// SPEC-004: "the CLI-worker handshake includes schema version, Dolgorae
/// semantic version, binary SHA-256 ... A mismatch returns
/// `DOLGORAE_PROTOCOL_MISMATCH`; upgrade does not silently mix CLI and worker
/// versions within one run generation."
///
/// A caller-side check alone cannot enforce that: the build that would have to
/// perform it is exactly the build that does not have it. The worker refuses
/// the skew itself, from what the request declares, while frozen control v1
/// keeps crossing the same skew.
#[test]
fn a_worker_refuses_an_ordinary_request_from_another_build() {
    let tree = Tree::new();
    let state_root = tree.state_root();
    let (owner, binding) = mint(&tree.credential("owner.json"), "cli");
    let run_id = Uuid::now_v7();
    publish(&state_root, run_id, binding);
    let worker = Worker::start(&state_root, run_id);

    // An older CLI declares nothing at all.
    let undeclared = worker.call_exact(&worker.send());
    assert!(
        matches!(
            &undeclared,
            ControlResponseV1::Rejected { code } if code == "DOLGORAE_PROTOCOL_MISMATCH"
        ),
        "an undeclared caller was admitted: {undeclared:?}"
    );

    // A newer or differently built CLI declares a build this worker is not.
    for skew in [
        ExecutingBuild {
            version: "0.2.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "3".repeat(64),
        },
        ExecutingBuild {
            version: "0.1.0".to_owned(),
            mutation_protocol_version: 2,
            binary_sha256: "3".repeat(64),
        },
        ExecutingBuild {
            version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "4".repeat(64),
        },
    ] {
        let mut request = worker.send();
        request.declare_caller(skew.clone());
        let refused = worker.call_exact(&request);
        assert!(
            matches!(
                &refused,
                ControlResponseV1::Rejected { code } if code == "DOLGORAE_PROTOCOL_MISMATCH"
            ),
            "{skew:?} crossed a build boundary: {refused:?}"
        );
    }

    // The Run was never touched, and frozen control v1 still answers the same
    // worker across exactly that skew.
    let status = worker.call_exact(&ControlRequestV1::Status {
        expected: worker.identity.clone(),
    });
    assert!(
        matches!(status, ControlResponseV1::Status { .. }),
        "frozen control v1 stopped crossing a build upgrade"
    );
    // And the matching build is still admitted, so the check refuses skew
    // rather than everything.
    authorized(&worker.call(&worker.send(), Some(&owner)));
}
