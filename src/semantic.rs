use crate::app_server::JsonRpcConnection;
use crate::conformance::{
    BootstrapRecordKind, BootstrapRequest, ConformantLedger, IdempotencyIntent,
    IdempotencyOperation,
};
use crate::controller::{
    CredentialCarrier, RunMutationLock, RunResetEnvironment, authorize_controller,
    binding_from_carrier, carrier_from_options, default_operator_root,
    load_reconciled_controller_binding,
};
use crate::darwin::DarwinSystem;
use crate::domain::{Access, Assurance, ControlMode, ExecutionLane, Purpose, PurposeKind};
use crate::event::EventProjection;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{LedgerClock, SystemLedgerClock};
use crate::machine::MachineError;
use crate::profile::{ProfileOperation, ServerState};
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, AppServerFacts, AuditPolicy,
    CompatibilityVerdict, DolgoraeBuild, ExecutableIdentity, InstructionSnapshot, ParentReference,
    ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest, RunStore, StartReservation,
    StartReservationStore, launch_contract_digest, runtime_profile_snapshot_digest,
};
use crate::runtime::{RuntimeCapabilities, capabilities};
use crate::specialist::ReviewerRuntimePlan;
use crate::specialist::ReviewerRuntimeRequest;
use crate::turn::ImageDetail;
use crate::worker::{
    ControlRequestV1, ControlResponseV1, SessionAttach, TurnControlImage, TurnControlRequest,
    WorkerSessionBootstrap,
};
use crate::workspace::{
    GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode, WorkspaceService,
    WorkspaceView,
};
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// One `run` verb the Machine CLI can execute against a live Run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunVerb {
    Start,
    Status,
    Send,
    Submit,
    Wait,
    Events,
    Respond,
    Interrupt,
    Close,
}

impl RunVerb {
    #[must_use]
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Self::Send | Self::Submit | Self::Respond | Self::Interrupt | Self::Close
        )
    }

    #[must_use]
    pub const fn operation_name(self) -> &'static str {
        match self {
            Self::Start => "run.start",
            Self::Status => "run.status",
            Self::Send => "run.send",
            Self::Submit => "run.submit",
            Self::Wait => "run.wait",
            Self::Events => "run.events",
            Self::Respond => "run.respond",
            Self::Interrupt => "run.interrupt",
            Self::Close => "run.close",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticCommand {
    RuntimeCapabilities,
    Initialize {
        path: Option<PathBuf>,
        mode: WorkspaceMode,
    },
    WorkspaceInspect {
        workspace: Option<PathBuf>,
    },
    Run {
        verb: RunVerb,
        args: Vec<OsString>,
    },
    Future {
        dotted_name: String,
    },
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(untagged)]
pub enum SemanticResult {
    RuntimeCapabilities(Box<RuntimeCapabilities>),
    Workspace(WorkspaceView),
    Run(Value),
    /// `run events` is the only run verb that answers with more than one
    /// machine object: SPEC-006 has it emit one envelope per durable record
    /// and then the `end` frame, so the adapter renders each of these as its
    /// own envelope rather than wrapping them in an array the schema does not
    /// have.
    RunStream(Vec<Value>),
}

pub trait SemanticService: Send + Sync {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError>;
}

pub(crate) struct PreparedReviewer {
    pub view: WorkspaceView,
    pub state_root: PathBuf,
    pub plan: ReviewerRuntimePlan,
    pub model: String,
    pub effort: String,
    pub profile_snapshot: crate::profile::ProfileSnapshot,
}

pub(crate) fn prepare_reviewer(
    workspace: Option<&Path>,
    profile_name: &str,
    objective: &str,
) -> Result<PreparedReviewer, MachineError> {
    let view = WorkspaceService::system()?.discover(workspace)?;
    let state = profile_server_state(&view, profile_name)?;
    let model = state.default_model.clone();
    let efforts = advertised_efforts(&state, profile_name, &model)?;
    let effort = default_effort(&efforts);
    let profile_snapshot = state.snapshot.clone();
    let profile = run_profile_snapshot(&profile_snapshot)?;
    let plan = ReviewerRuntimePlan::resolve(
        &profile,
        ReviewerRuntimeRequest {
            runtime_profile: profile_name.to_owned(),
            model: model.clone(),
            effort: effort.clone(),
            objective: objective.to_owned(),
            required_capabilities: Vec::new(),
        },
    )?;
    let state_root = workspace_state_root(&view)?;
    Ok(PreparedReviewer {
        view,
        state_root,
        plan,
        model,
        effort,
        profile_snapshot,
    })
}

pub(crate) fn control_reviewer_run(
    verb: RunVerb,
    args: &[OsString],
) -> Result<Value, MachineError> {
    match run_control(verb, args)? {
        SemanticResult::Run(value) => Ok(value),
        _ => Err(internal("reviewer control returned a non-run result")),
    }
}

#[derive(Default)]
pub struct CoreSemanticService;

impl SemanticService for CoreSemanticService {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError> {
        match command {
            SemanticCommand::RuntimeCapabilities => Ok(SemanticResult::RuntimeCapabilities(
                Box::new(capabilities()),
            )),
            SemanticCommand::Initialize { path, mode } => WorkspaceService::system()?
                .initialize(path.as_deref(), *mode)
                .map(SemanticResult::Workspace),
            SemanticCommand::WorkspaceInspect { workspace } => WorkspaceService::system()?
                .discover(workspace.as_deref())
                .map(SemanticResult::Workspace),
            SemanticCommand::Run { verb, args } => match verb {
                RunVerb::Start => run_start(args).map(SemanticResult::Run),
                other => run_control(*other, args),
            },
            SemanticCommand::Future { dotted_name } => Err(MachineError::invalid_argument(
                "command",
                format!("{dotted_name} is owned by a later roadmap task"),
            )),
        }
    }
}

/// Start a Run: pin the Runtime Profile, publish the run record, bootstrap the
/// durable ledger, and hand the hidden worker the app-server session it will own.
fn run_start(args: &[OsString]) -> Result<Value, MachineError> {
    run_start_with_context(args, None)
}

pub(crate) struct ReviewerStartContext<'a> {
    pub reserved_run_id: Uuid,
    pub aggregate_binding: &'a AggregateBinding,
    pub plan: &'a ReviewerRuntimePlan,
    pub review_cwd: Option<&'a Path>,
}

pub(crate) fn start_reviewer_run(
    args: &[OsString],
    context: ReviewerStartContext<'_>,
) -> Result<Value, MachineError> {
    run_start_with_context(args, Some(&context))
}

fn run_start_with_context(
    args: &[OsString],
    reviewer: Option<&ReviewerStartContext<'_>>,
) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover_for_run_start(workspace.as_deref())?;
    let profile_name = required(args, "--profile")?;
    let control_mode = parse_control_mode(&required(args, "--control-mode")?)?;
    let execution_lane = parse_execution_lane(&required(args, "--execution-lane")?)?;
    let assurance = parse_assurance(&required(args, "--required-assurance")?)?;
    let purpose = Purpose {
        kind: parse_purpose(&required(args, "--purpose")?)?,
        external_label: bounded_option(args, "--purpose-label", 128)?,
    };
    let parent_ref = parent_reference(args, control_mode)?;
    // Checked before the reservation, not after it: a key this command would
    // refuse must never first become durable state under a Run identity.
    let idempotency_key = bounded_key(args, "--idempotency-key", 256)?;
    if execution_lane != ExecutionLane::SharedReadonly {
        return Err(MachineError::invalid_argument(
            "--execution-lane",
            "only shared_readonly Runs are available in this slice",
        ));
    }
    let state = profile_server_state(&view, &profile_name)?;
    let model = optional(args, "--model").unwrap_or_else(|| state.default_model.clone());
    if !state.models.contains(&model) {
        return Err(compatibility_rejected(
            &profile_name,
            "model",
            json!(state.models),
            json!(model),
            "requested model is not advertised by the profile server",
        ));
    }
    let efforts = advertised_efforts(&state, &profile_name, &model)?;
    let effort = optional(args, "--effort").unwrap_or_else(|| default_effort(&efforts));
    if !efforts.contains(&effort) {
        return Err(compatibility_rejected(
            &profile_name,
            "reasoning_effort",
            json!(efforts),
            json!(effort),
            "requested reasoning effort is not advertised for this model",
        ));
    }
    let instructions_text = instructions_from(args)?;
    if instructions_text.is_empty() || instructions_text.len() > 65_536 {
        return Err(MachineError::invalid_argument(
            "--instructions",
            "a Run pins nonempty instructions of at most 65536 bytes",
        ));
    }
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let binding = binding_from_carrier(&carrier, 1)?;

    let required_capabilities = required_capabilities(args);
    for capability in &required_capabilities {
        let support = match state.capabilities.get(capability) {
            Some(crate::profile::ProfileCapabilityState::Supported) => continue,
            Some(crate::profile::ProfileCapabilityState::RecognizedUnsupported) => {
                "recognized_unsupported"
            }
            Some(crate::profile::ProfileCapabilityState::Unavailable) => "unavailable",
            Some(crate::profile::ProfileCapabilityState::Unverified) | None => "unavailable",
        };
        return Err(MachineError::new(
            "CAPABILITY_UNSUPPORTED",
            "the selected profile does not provide a required capability",
            false,
            json!({
                "profile": profile_name,
                "capability_name": capability,
                "support": support,
            }),
        ));
    }

    let state_root = workspace_state_root(&view)?;
    let profile = run_profile_snapshot(&state.snapshot)?;
    // docs/specs/README.md: "Run allocation reserves its key before publishing a Run."
    // The normalization is decided here, before any Run identity exists, so a
    // retried allocation resolves to the same digest and therefore the same
    // Run rather than to a second one.
    let mut normalized_identity_sha256 = start_normalized_digest(
        &view,
        &profile,
        &binding,
        control_mode,
        execution_lane,
        assurance,
        &purpose,
        parent_ref.as_ref(),
        &model,
        &effort,
        &required_capabilities,
        &instructions_text,
    )?;
    if let Some(context) = reviewer {
        normalized_identity_sha256 = sha256_hex(
            format!(
                "reviewer-v1\0{normalized_identity_sha256}\0{}\0{}",
                serde_json::to_string(context.aggregate_binding)
                    .map_err(|error| internal(error.to_string()))?,
                crate::run::agent_configuration_digest(&context.plan.agent_configuration)
                    .map_err(internal)?
            )
            .as_bytes(),
        );
    }
    let reservations = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    let reservation = match reservations.load(&idempotency_key)? {
        Some(held) => held,
        None => reservations.reserve(&StartReservation {
            schema_version: 1,
            operation: "start_run".to_owned(),
            idempotency_key: idempotency_key.clone(),
            normalized_identity_sha256: normalized_identity_sha256.clone(),
            run_id: reviewer.map_or_else(Uuid::now_v7, |context| context.reserved_run_id),
        })?,
    };
    if reservation.normalized_identity_sha256 != normalized_identity_sha256 {
        return Err(crate::run::idempotency_conflict(
            reservation.run_id,
            &reservation.normalized_identity_sha256,
            &normalized_identity_sha256,
        ));
    }
    if let Some(context) = reviewer
        && reservation.run_id != context.reserved_run_id
    {
        return Err(crate::run::idempotency_conflict(
            reservation.run_id,
            &reservation.normalized_identity_sha256,
            &normalized_identity_sha256,
        ));
    }
    let run_id = reservation.run_id;
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    // The identical retry returns the original Run.  Reaching a published Run
    // through its own reservation is exactly the response-loss reconciliation
    // docs/specs/README.md describes, so nothing is allocated, published, or started again
    // — and because nothing here reached that Run's worker, the verdict is
    // derived from what durable state proves rather than asserted as `Match`.
    if store.load_manifest(run_id).is_ok() {
        ensure_run_membership(workspace.as_deref(), &state, run_id)?;
        return run_object(
            &state_root,
            run_id,
            observed_control_socket_epoch(&state_root, run_id),
            projection_identity_verdict(&state_root, run_id),
        );
    }
    let start_baseline = WorkspaceService::system()?.capture_run_baseline(&view)?;
    let mut manifest = build_manifest(
        run_id,
        &view,
        &state,
        profile,
        &model,
        &effort,
        &instructions_text,
        purpose,
        parent_ref,
        control_mode,
        execution_lane,
        assurance,
        binding,
        required_capabilities,
        start_baseline,
    )?;
    if let Some(context) = reviewer {
        manifest.agent_configuration = context.plan.agent_configuration.clone();
        manifest.aggregate_binding = Some(context.aggregate_binding.clone());
        context.plan.validate_manifest(&manifest)?;
    }
    let directory = store.publish(&manifest)?;

    let mut ledger = ConformantLedger::open_for_bootstrap(&directory.root, run_id)
        .map_err(|error| internal(error.to_string()))?;
    ledger
        .bootstrap(&BootstrapRequest {
            timestamp: SystemLedgerClock::default().timestamp(),
            workspace_id: view.workspace_id.clone(),
            intent: IdempotencyIntent {
                schema_version: 1,
                operation: IdempotencyOperation::StartRun,
                idempotency_key,
                normalized_identity_sha256,
                run_id,
            },
            record_kind: BootstrapRecordKind::RunCreated,
            initial_access: Access::Read.as_str().to_owned(),
            default_effort: Some(effort.clone()),
        })
        .map_err(|error| internal(error.to_string()))?;
    drop(ledger);

    // Membership is part of publishing the Run's connection authority, not a
    // later best-effort side effect.  The profile lifecycle must be able to
    // refuse stop/restart before the worker can attach to this server.
    crate::profile::register_run_member(workspace.as_deref(), &state, &run_id.to_string())?;

    let session = WorkerSessionBootstrap {
        app_server_socket: PathBuf::from(&state.socket_path),
        canonical_codex_home: state.snapshot.canonical_codex_home.clone(),
        server_key: state.snapshot.server_key.clone(),
        server_epoch: state.server_epoch,
        fixed_model: model.clone(),
        default_effort: effort.clone(),
        supported_efforts: efforts,
        cwd: match reviewer.and_then(|context| context.review_cwd) {
            Some(path) => path.to_path_buf(),
            None => view
                .canonical_path
                .to_path_buf()
                .map_err(|_| internal("workspace path is not representable"))?,
        },
        developer_instructions: instructions_text,
        sandbox: "read-only".to_owned(),
        approval_policy: "never".to_owned(),
        safety_policy: reviewer.map_or(crate::turn::SessionSafetyPolicy::Standard, |context| {
            context.plan.safety_policy
        }),
        artifact_root: directory.root.join("artifacts"),
        attach: SessionAttach::Start,
        transport_timeout_seconds: 900,
    };
    let control_socket_epoch = 1;
    if let Err(error) = start_worker(
        &state_root,
        &crate::worker::RunWorkerStart {
            workspace_id: view.workspace_id.clone(),
            run_id,
            run_generation: 1,
            ledger_root: directory.root.clone(),
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: crate::worker::WORKER_PROTOCOL_VERSION,
            control_socket_epoch,
            profile: profile_name,
            session: Some(session),
        },
    ) {
        let _ = crate::profile::release_run_member(
            workspace.as_deref(),
            &state,
            &run_id.to_string(),
            crate::profile::RunMemberRelease::Closed,
        );
        return Err(error);
    }
    // The Run's own record is the answer, not a summary of what start did:
    // SPEC-006 has every lifecycle command return the resulting `run`.
    run_object(&state_root, run_id, control_socket_epoch, "Match")
}

/// Drive one Run verb through the worker's control socket.
fn run_control(verb: RunVerb, args: &[OsString]) -> Result<SemanticResult, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let run_id = positional_run_id(args)?;
    let state_root = workspace_state_root(&view)?;
    // Authority is checked against the Run's own published binding before the
    // control socket is touched, so an unauthorised caller never occupies a
    // worker thread.  This is only an early rejection: the same already-open
    // descriptor travels to the worker with `SCM_RIGHTS`, and the worker's
    // revalidation under the Run mutation lock is the authoritative one.
    let carrier = if verb.mutates() {
        let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
        let binding = load_reconciled_controller_binding(&state_root, run_id)?;
        authorize_controller(run_id, verb.operation_name(), &binding, &carrier)?;
        Some(carrier)
    } else {
        None
    };
    let uid = DarwinSystem.current_uid();
    // Every option is parsed before the socket is touched, so a malformed
    // request never reaches the worker and a worker thread is never held open
    // waiting for a caller that will fail anyway.
    let prepared = match verb {
        RunVerb::Start => return Err(internal("run start is composed separately")),
        RunVerb::Status | RunVerb::Interrupt => Prepared::Plain,
        // SPEC-005 grammar: `run wait <run-id> <turn-id>`.  The Turn is the
        // caller's, so it is carried to the worker instead of being inferred
        // there from whatever happens to be live.
        RunVerb::Wait => Prepared::Wait {
            turn_id: positional_turn_id(args)?,
            timeout_ms: caller_timeout_ms(args)?,
        },
        RunVerb::Close => Prepared::Close {
            interrupt: switch(args, "--interrupt"),
        },
        // docs/specs/README.md: "Writer acquisition is lazy and explicit. `run
        // send|submit --write` MUST activate" it — and a `shared_readonly`
        // Run that requested write is `SHARED_RUN_WRITE_FORBIDDEN`, whose
        // required action is a dedicated write continuation.  The lane comes
        // from the Run's own manifest, so the refusal names what this Run
        // actually is rather than what this slice happens to publish.
        RunVerb::Send | RunVerb::Submit if switch(args, "--write") => {
            return Err(write_forbidden(&state_root, run_id));
        }
        RunVerb::Send | RunVerb::Submit => Prepared::Turn {
            request: turn_request(args)?,
            timeout_ms: caller_timeout_ms(args)?,
        },
        RunVerb::Events => {
            let requested = requested_cursor(args)?;
            let projection = parse_projection(optional(args, "--projection").as_deref())?;
            return run_events(&state_root, run_id, uid, requested, &projection);
        }
        RunVerb::Respond => Prepared::Respond {
            request_id: required(args, "--request-id")?
                .parse::<u64>()
                .map_err(|_| {
                    MachineError::invalid_argument("--request-id", "request id must be an integer")
                })?,
            response: response_body(args)?,
        },
    };
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(&state_root), run_id)
            .map_err(|error| error.machine_error(run_id, &state_root))?;
    let response = crate::worker::call_run_worker(
        &state_root,
        run_id,
        uid,
        carrier.as_ref().map(CredentialCarrier::raw_fd),
        |expected| match prepared {
            Prepared::Plain if verb == RunVerb::Status => ControlRequestV1::Status { expected },
            Prepared::Plain => ControlRequestV1::Interrupt {
                expected,
                caller: None,
            },
            Prepared::Wait {
                turn_id,
                timeout_ms,
            } => ControlRequestV1::Wait {
                expected,
                caller: None,
                turn_id,
                timeout_ms,
            },
            Prepared::Close { interrupt } => ControlRequestV1::Close {
                expected,
                caller: None,
                interrupt,
            },
            Prepared::Turn {
                request,
                timeout_ms,
            } if verb == RunVerb::Send => ControlRequestV1::Send {
                expected,
                caller: None,
                request,
                timeout_ms,
            },
            Prepared::Turn { request, .. } => ControlRequestV1::Submit {
                expected,
                caller: None,
                request,
            },
            Prepared::Respond {
                request_id,
                response,
            } => ControlRequestV1::Respond {
                expected,
                caller: None,
                request_id,
                response,
            },
        },
    );
    let response = match response {
        Ok(response) => response,
        // SPEC-006: projection-only `status` reads the fsynced projection
        // directly and MUST NOT fail merely because the current process
        // identity verdict is unverifiable.  `events` answers the same way
        // above; every verb left here either changes Run state or rejoins a
        // live Turn, and only the worker that owns the Run can do either.
        Err(error) if verb == RunVerb::Status && worker_unreachable(error) => {
            let verdict = absent_or_unverifiable(&runtime_record);
            let sources = RunSources::load(&state_root, run_id, 1)?;
            let last_terminal = durable_last_terminal(&state_root, run_id, &sources)?;
            return Ok(SemanticResult::Run(
                sources.run_value(verdict, last_terminal),
            ));
        }
        Err(error) => return Err(error.machine_error(run_id, &runtime_record)),
    };
    let control_socket_epoch = crate::worker::read_runtime_record(&runtime_record, uid)
        .map_or(1, |record| record.control_socket_epoch);
    let closed = matches!(response, ControlResponseV1::Closed { .. });
    let value = control_response_value(&state_root, run_id, control_socket_epoch, verb, response)?;
    if closed {
        let manifest = RunStore::new(SystemWorkspacePlatform, &state_root).load_manifest(run_id)?;
        // The Run is already durably closed. A concurrently vanished profile
        // lifetime cannot turn that completed mutation into a retryable close
        // failure (which would invite replay of an effect that already
        // happened). A live lifetime still receives the membership release;
        // an absent/replaced one has no server left for the member to gate.
        let _ = crate::profile::release_run_member_by_identity(
            workspace.as_deref(),
            &manifest.profile_capability_snapshot.profile_name,
            &manifest.profile_capability_snapshot.server_key,
            &run_id.to_string(),
            crate::profile::RunMemberRelease::Closed,
        );
    }
    Ok(value)
}

fn ensure_run_membership(
    workspace: Option<&Path>,
    state: &ServerState,
    run_id: Uuid,
) -> Result<(), MachineError> {
    let run_id = run_id.to_string();
    if !crate::profile::live_run_members(workspace, state)?
        .iter()
        .any(|member| member.run_id == run_id)
    {
        crate::profile::register_run_member(workspace, state, &run_id)?;
    }
    Ok(())
}

/// Whether this failure means no worker answered, as opposed to a worker that
/// answered and refused.
///
/// Only the three ways a Run can have no reachable control socket count: its
/// runtime record is gone or unsafe, the socket it named is gone or is another
/// node, or nothing answered on it.  A build-skew refusal is a live worker
/// saying no, so it is deliberately absent from this list.
const fn worker_unreachable(error: crate::worker::WorkerProtocolError) -> bool {
    matches!(
        error,
        crate::worker::WorkerProtocolError::InvalidRuntimeRecord
            | crate::worker::WorkerProtocolError::InvalidOwnerRecord
            | crate::worker::WorkerProtocolError::SocketIdentityMismatch
            | crate::worker::WorkerProtocolError::Io
    )
}

/// The refusal a `--write` turn earns from the Run it addresses.
///
/// The error contract requires this refusal to name the lane it refused and
/// the action that resolves it, so the lane is read rather than assumed.  A
/// Run whose lane is not `shared_readonly` is not refused for being shared:
/// writer acquisition simply does not exist yet, which is a fact about this
/// build and not about the Run.
fn write_forbidden(state_root: &Path, run_id: Uuid) -> MachineError {
    match RunStore::new(SystemWorkspacePlatform, state_root)
        .load_manifest(run_id)
        .map(|manifest| manifest.execution_lane)
    {
        Ok(ExecutionLane::SharedReadonly) => MachineError::new(
            "SHARED_RUN_WRITE_FORBIDDEN",
            "a shared_readonly Run cannot acquire writer authority",
            false,
            json!({
                "run_id": run_id,
                "execution_lane": ExecutionLane::SharedReadonly.as_str(),
                "reason": "a shared_readonly run requested write",
                "required_action": "create_dedicated_write_continuation",
            }),
        ),
        Ok(ExecutionLane::Dedicated) => MachineError::invalid_argument(
            "--write",
            "writer acquisition is owned by a later roadmap task",
        ),
        Err(error) => error,
    }
}

/// The identity verdict a projection-only read may honestly publish.
///
/// A Run with no runtime record has no worker to disagree with, so its
/// verdict is `Absent`.  One whose record exists but did not answer was not
/// proved either way, which is exactly `Unverifiable`.
fn absent_or_unverifiable(runtime_record: &Path) -> &'static str {
    if std::fs::symlink_metadata(runtime_record).is_ok() {
        "Unverifiable"
    } else {
        "Absent"
    }
}

/// The verdict a command that read only durable state may publish about a Run.
///
/// A runtime record that cannot even be addressed is the same evidence as one
/// that is not there: no worker was found, so the verdict is `Absent`.
fn projection_identity_verdict(state_root: &Path, run_id: Uuid) -> &'static str {
    crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
        .map_or("Absent", |record| absent_or_unverifiable(&record))
}

/// Read the durable event stream, paging to the head captured at command start.
///
/// SPEC-006: without `--follow`, `run events` "emits records through the head
/// captured at command start, then one `end` frame and exits 0".  One control
/// page is bounded, so the head is captured from the first answer and the
/// caller keeps asking until it is reached; records appended afterwards belong
/// to the next command, not this one.
fn run_events(
    state_root: &Path,
    run_id: Uuid,
    uid: u32,
    requested: RequestedCursor,
    projection: &EventProjection,
) -> Result<SemanticResult, MachineError> {
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    let after = match requested {
        RequestedCursor::Exact(after) => after,
        RequestedCursor::Refused(cursor) => {
            let head = durable_head(state_root, run_id)?;
            return Err(event_cursor_invalid(run_id, &cursor, &head.to_string()));
        }
    };
    let first = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
        ControlRequestV1::Events {
            expected,
            caller: None,
            after,
            projection: projection.clone(),
            limit: crate::worker::MAX_CONTROL_EVENT_PAGE,
        }
    });
    let page = match first {
        Ok(response) => response,
        // The durable ledger is the authority, and an observer may read it
        // without a worker: docs/specs/README.md has projection-only `events` open the
        // fsynced ledger rather than start, attach, or contend for one.
        Err(error) if worker_unreachable(error) => {
            return observed_events(state_root, run_id, after, projection);
        }
        Err(error) => return Err(error.machine_error(run_id, &runtime_record)),
    };
    let (deliveries, mut next_cursor, head) = match page {
        ControlResponseV1::Events {
            deliveries,
            next_cursor,
            head_cursor,
        } => {
            let head = parse_cursor(&head_cursor)?;
            (deliveries, parse_cursor(&next_cursor)?, head)
        }
        ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        } => return Err(MachineError::new(&code, message, retryable, details)),
        ControlResponseV1::Rejected { code } => return Err(rejected_error(&code)),
        _ => return Err(internal("control reply does not describe an event page")),
    };
    let mut objects = Vec::new();
    let mut cursor = after;
    let mut page = deliveries;
    loop {
        let complete = collect_deliveries(page, head, &mut objects)?;
        if !complete || next_cursor >= head || next_cursor <= cursor {
            break;
        }
        cursor = next_cursor;
        let response = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
            ControlRequestV1::Events {
                expected,
                caller: None,
                after: cursor,
                projection: projection.clone(),
                limit: crate::worker::MAX_CONTROL_EVENT_PAGE,
            }
        })
        .map_err(|error| error.machine_error(run_id, &runtime_record))?;
        match response {
            ControlResponseV1::Events {
                deliveries,
                next_cursor: next,
                ..
            } => {
                page = deliveries;
                next_cursor = parse_cursor(&next)?;
            }
            ControlResponseV1::Failed {
                code,
                message,
                retryable,
                details,
            } => return Err(MachineError::new(&code, message, retryable, details)),
            _ => return Err(internal("control reply does not describe an event page")),
        }
    }
    objects.push(json!({"kind": "end", "cursor": head.to_string()}));
    Ok(SemanticResult::RunStream(objects))
}

/// The same read, served from the fsynced ledger when no worker owns it.
fn observed_events(
    state_root: &Path,
    run_id: Uuid,
    after: u64,
    projection: &EventProjection,
) -> Result<SemanticResult, MachineError> {
    let head = durable_head(state_root, run_id)?;
    if after > head {
        return Err(event_cursor_invalid(
            run_id,
            &after.to_string(),
            &head.to_string(),
        ));
    }
    let root = state_root.join("runs").join(run_id.to_string());
    let ledger = crate::ledger::ObservedLedger::open(&root, run_id, head)
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?;
    let mut objects = Vec::new();
    let mut cursor = after;
    loop {
        let page = ledger
            .events_after(cursor, projection.clone())
            .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?;
        let next = page
            .last()
            .map(|delivery| parse_cursor(&delivery.record.cursor))
            .transpose()?
            .unwrap_or(head);
        let complete = collect_deliveries(page, head, &mut objects)?;
        if !complete || next >= head || next <= cursor {
            break;
        }
        cursor = next;
    }
    objects.push(json!({"kind": "end", "cursor": head.to_string()}));
    Ok(SemanticResult::RunStream(objects))
}

/// Render one page, stopping at the head this command captured.
///
/// Returns whether the whole page belonged to that head; a page that runs past
/// it describes records this command must not publish.
fn collect_deliveries(
    page: Vec<crate::event::EventDelivery>,
    head: u64,
    objects: &mut Vec<Value>,
) -> Result<bool, MachineError> {
    for delivery in page {
        if parse_cursor(&delivery.record.cursor)? > head {
            return Ok(false);
        }
        objects.push(
            serde_json::to_value(delivery)
                .map_err(|_| internal("event delivery is unrepresentable"))?,
        );
    }
    Ok(true)
}

/// The ledger head the fsynced projection commits.
fn durable_head(state_root: &Path, run_id: Uuid) -> Result<u64, MachineError> {
    Ok(RunStore::new(SystemWorkspacePlatform, state_root)
        .load_state_projection(run_id)?
        .ledger_head
        .sequence)
}

fn parse_cursor(value: &str) -> Result<u64, MachineError> {
    value
        .parse::<u64>()
        .map_err(|_| internal("ledger cursor is not a decimal sequence"))
}

/// The registered refusal for a noncanonical or beyond-head event cursor.
fn event_cursor_invalid(run_id: Uuid, requested: &str, head: &str) -> MachineError {
    MachineError::new(
        "EVENT_CURSOR_INVALID",
        "event cursor is noncanonical or beyond the run ledger head",
        false,
        json!({
            "run_id": run_id,
            "requested_cursor": requested,
            "head_cursor": head,
        }),
    )
}

/// A durable ledger an observer could not replay is an integrity failure.
fn audit_integrity(run_id: Uuid, sequence: u64, reason: &str) -> MachineError {
    MachineError::new(
        "AUDIT_INTEGRITY_FAILURE",
        "run ledger could not be replayed",
        false,
        json!({"run_id": run_id, "sequence": sequence, "reason": reason}),
    )
}

/// The `--after` cursor a caller supplied.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestedCursor {
    /// A canonical decimal cursor inside the ledger sequence domain.
    Exact(u64),
    /// A cursor the ledger can never hold, carried in the canonical form the
    /// error contract requires of `requested_cursor`.
    Refused(String),
}

/// Read `--after` as SPEC-006 defines it.
///
/// It "accepts the canonical unsigned decimal ledger sequence string without
/// leading zeroes" and defaults to `"0"`; a noncanonical or beyond-head cursor
/// is `EVENT_CURSOR_INVALID`, which needs the Run's head and so is decided
/// where that head is readable.
fn requested_cursor(args: &[OsString]) -> Result<RequestedCursor, MachineError> {
    let Some(value) = optional(args, "--after") else {
        return Ok(RequestedCursor::Exact(0));
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MachineError::invalid_argument(
            "--after",
            "cursor must be an unsigned decimal ledger sequence",
        ));
    }
    let trimmed = value.trim_start_matches('0');
    let canonical = if trimmed.is_empty() { "0" } else { trimmed };
    if canonical.len() > 20 {
        return Err(MachineError::invalid_argument(
            "--after",
            "cursor exceeds the ledger sequence domain",
        ));
    }
    if canonical != value {
        return Ok(RequestedCursor::Refused(canonical.to_owned()));
    }
    canonical.parse::<u64>().map_or_else(
        |_| Ok(RequestedCursor::Refused(canonical.to_owned())),
        |cursor| Ok(RequestedCursor::Exact(cursor)),
    )
}

/// A control request with everything but the worker's identity already decided.
enum Prepared {
    Plain,
    Wait {
        turn_id: String,
        timeout_ms: Option<u64>,
    },
    Close {
        interrupt: bool,
    },
    Turn {
        request: TurnControlRequest,
        timeout_ms: Option<u64>,
    },
    Respond {
        request_id: u64,
        response: Value,
    },
}

/// Restate one control reply as the checked machine `data` for its verb.
///
/// SPEC-006 fixes the shape per verb: a lifecycle verb answers with the
/// resulting `run`, `send`/`submit`/`wait` answer with a `turn`, and `events`
/// answers with one `event_data` per durable record. The private control
/// vocabulary never reaches a consumer.
fn control_response_value(
    state_root: &Path,
    run_id: Uuid,
    control_socket_epoch: u64,
    verb: RunVerb,
    response: ControlResponseV1,
) -> Result<SemanticResult, MachineError> {
    match response {
        ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        } => Err(MachineError::new(&code, message, retryable, details)),
        ControlResponseV1::Rejected { code } => Err(rejected_error(&code)),
        other => {
            let sources = RunSources::load(state_root, run_id, control_socket_epoch)?;
            let value = match verb {
                RunVerb::Send | RunVerb::Submit | RunVerb::Wait => {
                    let turn = turn_value(&sources, &other)?;
                    // The error table gives `run send/wait` exit 7 for a
                    // failed or interrupted terminal.  A Master classifies by
                    // exit class, so the outcome cannot ride inside a success
                    // envelope; the response, usage, and cursor stay reachable
                    // through `run status.data.last_terminal`.
                    if verb != RunVerb::Submit
                        && let Some(refusal) = terminal_refusal(run_id, &turn)
                    {
                        return Err(refusal);
                    }
                    turn
                }
                // `events` answers with the event stream or nothing; anything
                // else here would publish a shape the verb's schema forbids.
                RunVerb::Events => return Err(internal("event read answered with run state")),
                RunVerb::Status => {
                    // The worker reports the terminal its own drain observed;
                    // a worker that was restarted, or never ran the Turn, has
                    // none, and the durable record answers instead.
                    let observed = last_terminal_value(&sources, &other);
                    let last_terminal = if observed.is_null() {
                        durable_last_terminal(state_root, run_id, &sources)?
                    } else {
                        observed
                    };
                    sources.run_value("Match", last_terminal)
                }
                _ => sources.run_value("Match", Value::Null),
            };
            Ok(SemanticResult::Run(value))
        }
    }
}

/// Frozen control v1 answers an identity refusal with a bare code, so the
/// contract's details are supplied here from the two builds the caller
/// actually addressed.
fn rejected_error(code: &str) -> MachineError {
    MachineError::new(
        code,
        "worker refused the control request",
        false,
        json!({
            "expected_protocol": crate::worker::WORKER_PROTOCOL_VERSION,
            "actual_protocol": crate::worker::WORKER_PROTOCOL_VERSION,
            "control_v1_available": true,
        }),
    )
}

/// The exit-7 refusal a terminal Turn earns, if it earned one.
fn terminal_refusal(run_id: Uuid, turn: &Value) -> Option<MachineError> {
    let status = turn.get("status").and_then(Value::as_str)?;
    let code = match status {
        "failed" => "TURN_FAILED",
        "interrupted" => "TURN_INTERRUPTED",
        _ => return None,
    };
    Some(MachineError::new(
        code,
        format!("the addressed turn reached {status} terminal state"),
        false,
        json!({
            "run_id": run_id,
            "turn_id": turn.get("turn_id").cloned().unwrap_or(Value::Null),
            "status": status,
        }),
    ))
}

/// The Run's last terminal Turn, reconstructed from its durable ledger.
///
/// A worker's control state is process memory: shutdown, restart, or a
/// quarantine erases it, and a projection-only `status` has no worker to ask
/// at all.  The `turn_terminal` record is the durable authority for the same
/// fact, so it is what an offline read answers from.
fn durable_last_terminal(
    state_root: &Path,
    run_id: Uuid,
    sources: &RunSources,
) -> Result<Value, MachineError> {
    let head = sources.projection.ledger_head.sequence;
    // A Run that never started a Turn has no terminal to reconstruct, and the
    // projection already knows that, so the ledger is not reopened to be told.
    if head == 0 || sources.projection.latest_turn_id.is_none() {
        return Ok(Value::Null);
    }
    let root = state_root.join("runs").join(run_id.to_string());
    let ledger = crate::ledger::ObservedLedger::open(&root, run_id, head)
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?;
    let Some(payload) = ledger
        .last_terminal()
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?
    else {
        return Ok(Value::Null);
    };
    let terminal: crate::turn::TerminalTurn = serde_json::from_value(payload)
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?;
    Ok(terminal_turn_value(sources, &terminal))
}

/// The checked `turn` object one recorded terminal describes.
fn terminal_turn_value(sources: &RunSources, terminal: &crate::turn::TerminalTurn) -> Value {
    turn_object(
        sources,
        &terminal.thread_id,
        &terminal.turn_id,
        &terminal.status,
        &terminal.effort,
        terminal.usage.clone(),
        final_response_value(sources.manifest.run_id, terminal.final_response.as_ref()),
    )
}

/// The Turn a `status` reply reports as the Run's last terminal.
///
/// docs/specs/README.md sends a Master here for the response, usage, and cursor behind the
/// intentionally minimal exit-7 envelope.  The worker carries the terminal its
/// own drain observed, so this restates observed evidence and publishes
/// nothing when there is none.
fn last_terminal_value(sources: &RunSources, response: &ControlResponseV1) -> Value {
    let ControlResponseV1::Status {
        last_terminal: Some(terminal),
        ..
    } = response
    else {
        return Value::Null;
    };
    terminal_turn_value(sources, terminal)
}

/// The durable sources a checked `run` object is projected from.
///
/// A `run` is a statement about the Run, not about the control call that
/// produced it, so it is read from the Run's own manifest and the durable
/// state projection its worker commits rather than assembled out of a control
/// reply that only describes one operation.
struct RunSources {
    manifest: RunManifest,
    projection: crate::projection::RunStateProjection,
    control_socket_epoch: u64,
}

impl RunSources {
    fn load(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
    ) -> Result<Self, MachineError> {
        let store = RunStore::new(SystemWorkspacePlatform, state_root);
        Ok(Self {
            manifest: store.load_manifest(run_id)?,
            projection: store.load_state_projection(run_id)?,
            control_socket_epoch,
        })
    }

    /// The event cursor an observer may resume from.
    fn event_cursor(&self) -> String {
        self.projection
            .last_event_cursor
            .clone()
            .unwrap_or_else(|| "0".to_owned())
    }

    fn server_epoch(&self) -> u64 {
        self.manifest.profile_capability_snapshot.server_epoch
    }

    /// The Run's recorded default reasoning effort.
    fn default_effort(&self) -> String {
        self.projection
            .default_effort
            .clone()
            .unwrap_or_else(|| self.manifest.default_reasoning_effort.clone())
    }

    /// This slice only publishes `shared_readonly` Runs, and the checked
    /// schema binds every lane-shaped member of such a Run to one value.
    /// Writing them out here rather than deriving them keeps the projection
    /// honest about what it does and does not observe: nothing here claims a
    /// dedicated lane, a writer, or a background census this slice never runs.
    fn run_value(&self, identity_verdict: &str, last_terminal: Value) -> Value {
        let manifest = &self.manifest;
        let projection = &self.projection;
        let lifecycle = projection.lifecycle.as_str();
        json!({
            "workspace_id": manifest.workspace_id,
            "run_id": manifest.run_id,
            "state": lifecycle,
            "state_variant": "shared_readonly",
            "control_mode": manifest.control_mode.as_str(),
            "purpose": manifest.purpose,
            "execution_lane": manifest.execution_lane.as_str(),
            "effective_policy": {
                "access": "read",
                "verification": "verified",
                // No policy transition is recordable for a read-only Run, and
                // this slice binds one Thread per Run and never rebinds it, so
                // the Thread's generation is exactly whether it has one.
                "policy_epoch": 0,
                "thread_generation": u64::from(projection.thread_id.is_some()),
                "server_epoch": self.server_epoch(),
                "writer_generation": Value::Null,
            },
            "writer_authority": {
                "state": "none",
                "writer_generation": 0,
                "transaction_id": Value::Null,
                "reconciliation_action": Value::Null,
            },
            "server_lane": {
                "kind": "shared_readonly",
                "lane_id": Value::Null,
                "process_generation": Value::Null,
                "server_epoch": self.server_epoch(),
                // The Run reached its worker, which owns the profile server
                // session; the lane itself is not re-probed by a run verb.
                "state": "unverified",
                "socket_identity_sha256": Value::Null,
            },
            "workload_background_state": {
                "state": "not_applicable",
                "mechanism": "shared_profile_aggregate",
                "census_revision": 0,
                "observed_process_count": 0,
                "quiescent_since": Value::Null,
                "consecutive_empty_samples": 0,
            },
            "requested_assurance": manifest.requested_assurance.as_str(),
            "achieved_assurance": manifest.achieved_assurance.as_str(),
            "instruction_contract": {
                "schema": manifest.instructions.schema,
                "common_prefix_version": manifest.instructions.common_prefix_version,
                "mode_prefix_version": manifest.instructions.mode_prefix_version,
                "purpose_prefix_version": manifest.instructions.purpose_prefix_version,
            },
            "lineage": Value::Null,
            "server_epoch": self.server_epoch(),
            "run_generation": projection.run_generation,
            "control_socket_epoch": self.control_socket_epoch,
            "profile": manifest.profile.profile_name,
            "thread_id": projection.thread_id,
            "active_turn_id": projection.active_turn_id,
            "controller": manifest.controller.identity,
            "parent_ref": manifest.parent_ref,
            "required_capabilities": manifest.required_capabilities,
            "model": manifest.model,
            "effort": projection
                .default_effort
                .clone()
                .unwrap_or_else(|| manifest.default_reasoning_effort.clone()),
            "event_cursor": self.event_cursor(),
            "pending_count": projection.pending_requests.len(),
            "identity_verdict": identity_verdict,
            "recovery": recovery_value(manifest, lifecycle),
            // Observed evidence or nothing: the durable projection records
            // which Turn was last active, not its response, usage, or measured
            // changes, so a verb that did not read a terminal publishes null
            // rather than a fabricated `turn`.
            "last_terminal": last_terminal,
        })
    }
}

/// What a caller may safely do next, derived from the Run's own lifecycle.
fn recovery_value(manifest: &RunManifest, lifecycle: &str) -> Value {
    let (status, unsafe_actions): (&str, &[&str]) = match lifecycle {
        "closed" => (
            "closed",
            &["run.send", "run.submit", "run.respond", "run.interrupt"],
        ),
        "outcome_unknown" => ("outcome_unknown", &["run.send", "run.submit"]),
        "reconciliation_required" => (
            "reconciliation_required",
            &["run.send", "run.submit", "run.respond"],
        ),
        "start_failed" => (
            "recovery_required",
            &["run.send", "run.submit", "run.respond"],
        ),
        "running" | "waiting_interaction" => ("ready", &["run.send", "run.submit"]),
        _ => ("ready", &[]),
    };
    json!({
        "status": status,
        "reason": Value::Null,
        "safe_actions": ["run.status", "run.events", "run.wait"],
        "unsafe_actions": unsafe_actions,
        "recorded_server_key": manifest.profile_capability_snapshot.server_key,
        "recorded_server_epoch": manifest.profile_capability_snapshot.server_epoch,
        "recorded_epoch_absence": "not_required",
        // A run verb reaches the worker, never the profile server, so nothing
        // about the live server was observed here.
        "observed_server_key": Value::Null,
        "observed_server_epoch": Value::Null,
        "thread_history_result": "not_read",
    })
}

/// Restate one Turn-producing control reply as the checked `turn` object.
fn turn_value(sources: &RunSources, response: &ControlResponseV1) -> Result<Value, MachineError> {
    // The effort is the Turn's own, carried back from the coordinator that
    // started it: SPEC-006 has a terminal result report "turn reasoning
    // effort", which a one-turn `--effort` makes different from the Run's
    // recorded default.
    let (thread_id, turn_id, status, effort, final_response, usage) = match response {
        ControlResponseV1::Terminal { terminal } => (
            terminal.thread_id.clone(),
            terminal.turn_id.clone(),
            terminal.status.clone(),
            terminal.effort.clone(),
            final_response_value(sources.manifest.run_id, terminal.final_response.as_ref()),
            terminal.usage.clone(),
        ),
        ControlResponseV1::Accepted { accepted } => (
            accepted.thread_id.clone(),
            accepted.turn_id.clone(),
            "accepted".to_owned(),
            accepted.effort.clone(),
            Value::Null,
            None,
        ),
        ControlResponseV1::WaitingInteraction {
            thread_id,
            turn_id,
            effort,
            ..
        } => (
            thread_id.clone(),
            turn_id.clone(),
            "waiting_interaction".to_owned(),
            effort.clone(),
            Value::Null,
            None,
        ),
        ControlResponseV1::Interrupted {
            thread_id,
            turn_id,
            effort,
        } => (
            thread_id.clone(),
            turn_id.clone(),
            "interrupting".to_owned(),
            effort.clone(),
            Value::Null,
            None,
        ),
        ControlResponseV1::Running {
            thread_id,
            turn_id,
            effort,
        } => (
            thread_id.clone(),
            turn_id.clone(),
            "running".to_owned(),
            effort.clone(),
            Value::Null,
            None,
        ),
        _ => return Err(internal("control reply does not describe a turn")),
    };
    Ok(turn_object(
        sources,
        &thread_id,
        &turn_id,
        &status,
        &effort,
        usage,
        final_response,
    ))
}

/// The checked `turn` object, however it was observed.
#[allow(
    clippy::too_many_arguments,
    reason = "a turn object names every fact the contract requires of it"
)]
fn turn_object(
    sources: &RunSources,
    thread_id: &str,
    turn_id: &str,
    status: &str,
    effort: &str,
    usage: Option<Value>,
    final_response: Value,
) -> Value {
    json!({
        "run_id": sources.manifest.run_id,
        "thread_id": thread_id,
        "turn_id": turn_id,
        "server_epoch": sources.server_epoch(),
        "status": status,
        "model": sources.manifest.model,
        // A Turn whose effort was never observed falls back to the Run's own
        // recorded default rather than inventing one.
        "effort": if effort.is_empty() {
            sources.default_effort()
        } else {
            effort.to_owned()
        },
        "usage": usage,
        "final_response": final_response,
        "event_cursor": sources.event_cursor(),
        // Read-only Runs measure nothing: SPEC-002 keeps attribution
        // unverified until a writer Run observes the workspace itself.
        "workspace_changes": {
            "measured": false,
            "observed_paths": [],
            "truncated": false,
            "attribution": "unverified",
        },
    })
}

/// Restate the worker's final response as the published artifact contract's.
fn final_response_value(run_id: Uuid, response: Option<&crate::turn::FinalResponse>) -> Value {
    match response {
        None => Value::Null,
        Some(crate::turn::FinalResponse::Inline { text }) => {
            json!({"kind": "inline", "text": text})
        }
        Some(crate::turn::FinalResponse::Artifact {
            artifact_id,
            byte_length,
            sha256,
            created_at,
        }) => json!({
            "kind": "artifact",
            "artifact": {
                "schema_version": 1,
                "artifact_id": artifact_id,
                "run_id": run_id,
                "kind": "final_response",
                "visibility": "observer",
                "interaction_request_id": Value::Null,
                "media_type": "text/markdown",
                "byte_length": byte_length,
                "sha256": sha256,
                "created_at": created_at,
                "retention": "run_lifetime",
                "integrity": "verified",
            },
        }),
        Some(crate::turn::FinalResponse::Unavailable {
            byte_length,
            sha256,
            reason,
        }) => json!({
            "kind": "unavailable",
            "reason": reason,
            "observed_byte_length": byte_length,
            "sha256": sha256,
        }),
    }
}

/// The Run's own record, read after a lifecycle verb has taken effect.
fn run_object(
    state_root: &Path,
    run_id: Uuid,
    control_socket_epoch: u64,
    identity_verdict: &str,
) -> Result<Value, MachineError> {
    Ok(RunSources::load(state_root, run_id, control_socket_epoch)?
        .run_value(identity_verdict, Value::Null))
}

fn turn_request(args: &[OsString]) -> Result<TurnControlRequest, MachineError> {
    let message = turn_message(args)?;
    let mut images = Vec::new();
    for value in all(args, "--image") {
        let (detail, path) = value.split_once('=').ok_or_else(|| {
            MachineError::invalid_argument("--image", "expected <auto|low|high>=<path>")
        })?;
        // docs/specs/README.md stores "the canonical path, detail, byte length, and
        // streaming SHA-256" and puts the tuple in idempotency normalization,
        // so the detail token the caller wrote travels with its path.
        let detail = match detail {
            "auto" => ImageDetail::Auto,
            "low" => ImageDetail::Low,
            "high" => ImageDetail::High,
            _ => {
                return Err(MachineError::invalid_argument(
                    "--image",
                    "expected <auto|low|high>=<path>",
                ));
            }
        };
        images.push(TurnControlImage {
            detail,
            path: PathBuf::from(path),
        });
    }
    Ok(TurnControlRequest {
        message,
        idempotency_key: required(args, "--idempotency-key")?,
        effort: optional(args, "--effort"),
        images,
    })
}

/// The Turn's text, from the one source docs/specs/README.md allows.
///
/// "`send` and `submit` accept exactly one text source: `--message` or stdin.
/// Empty text is rejected.  If `--message` is absent, stdin is required and
/// MUST NOT be a TTY."  A terminal stdin is refused rather than read, because
/// a Master that forgot the text would otherwise block on a prompt that never
/// comes.  The piped bytes are bounded and taken verbatim: "normalization is
/// UTF-8 message bytes", so trimming anything here would silently change what
/// a retry of the same idempotency key is compared against.
fn turn_message(args: &[OsString]) -> Result<String, MachineError> {
    if let Some(message) = optional(args, "--message") {
        return nonempty_message("--message", message);
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(MachineError::invalid_argument(
            "--message",
            "a turn's text comes from --message or non-TTY stdin",
        ));
    }
    let mut bytes = Vec::new();
    std::io::Read::take(std::io::stdin(), MAX_MESSAGE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument("stdin", "message read failed"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MESSAGE_BYTES {
        return Err(MachineError::invalid_argument(
            "stdin",
            format!("a turn message is at most {MAX_MESSAGE_BYTES} bytes"),
        ));
    }
    let message = String::from_utf8(bytes)
        .map_err(|_| MachineError::invalid_argument("stdin", "message is not UTF-8"))?;
    nonempty_message("stdin", message)
}

fn nonempty_message(argument: &str, message: String) -> Result<String, MachineError> {
    if message.is_empty() {
        return Err(MachineError::invalid_argument(
            argument,
            "a turn needs a nonempty message",
        ));
    }
    Ok(message)
}

/// The interaction response body, from the one source docs/specs/README.md allows.
///
/// "`respond` accepts a JSON body only from exactly one protected inherited
/// `--response-fd` or non-TTY stdin; an interaction response body is never
/// accepted in argv."  A terminal stdin is refused rather than read, because a
/// Master that forgot the descriptor would otherwise block on a prompt that
/// never comes.
fn response_body(args: &[OsString]) -> Result<Value, MachineError> {
    let Some(value) = optional(args, "--response-fd") else {
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return Err(MachineError::invalid_argument(
                "--response-fd",
                "a response body comes from --response-fd or non-TTY stdin",
            ));
        }
        let mut bytes = Vec::new();
        std::io::Read::take(std::io::stdin(), MAX_RESPONSE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|_| MachineError::invalid_argument("stdin", "response read failed"))?;
        return decode_response(&bytes, "stdin");
    };
    let fd = value
        .parse::<i32>()
        .map_err(|_| MachineError::invalid_argument("--response-fd", "fd must be an integer"))?;
    let mut file = std::fs::File::from(
        DarwinSystem
            .duplicate_fd_cloexec(fd)
            .map_err(|_| MachineError::invalid_argument("--response-fd", "fd is not readable"))?,
    );
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, MAX_RESPONSE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument("--response-fd", "response read failed"))?;
    decode_response(&bytes, "--response-fd")
}

/// One bounded body, canonicalized before it is allowed to travel further.
fn decode_response(bytes: &[u8], argument: &str) -> Result<Value, MachineError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not UTF-8"))?;
    let parsed = parse(text)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))?;
    let canonical = canonicalize(&parsed)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))?;
    serde_json::from_slice(&canonical)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))
}

fn profile_server_state(view: &WorkspaceView, profile: &str) -> Result<ServerState, MachineError> {
    let workspace = view
        .canonical_path
        .to_path_buf()
        .map_err(|_| internal("workspace path is not representable"))?;
    let arguments = vec![
        OsString::from("--workspace"),
        workspace.into_os_string(),
        OsString::from(profile),
    ];
    let status = crate::profile::execute(ProfileOperation::ServerStatus, &arguments)?;
    let running = status
        .get("lifecycle")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "running");
    let document = if running {
        status.get("state").cloned().unwrap_or(Value::Null)
    } else {
        crate::profile::execute(ProfileOperation::ServerStart, &arguments)?
            .get("state")
            .cloned()
            .unwrap_or(Value::Null)
    };
    serde_json::from_value(document)
        .map_err(|_| internal("profile server state is not the expected shape"))
}

/// Ask the profile's app-server which reasoning efforts the chosen model
/// advertises.
///
/// The profile record keeps models but discards the efforts each one supports,
/// so this reads them where they are authoritative rather than assuming a
/// vocabulary the pinned Codex release never promised.
fn advertised_efforts(
    state: &ServerState,
    profile: &str,
    model: &str,
) -> Result<Vec<String>, MachineError> {
    let mut connection =
        JsonRpcConnection::connect(Path::new(&state.socket_path), Duration::from_secs(30))
            .map_err(|error| transport("connect", &error.to_string()))?;
    let initialized = connection
        .request(
            "initialize",
            json!({
                "clientInfo": {"name":"dolgorae","title":"Dolgorae","version":env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi":false,"optOutNotificationMethods":[]}
            }),
        )
        .map_err(|error| transport("read", &error.to_string()))?;
    let observed_home = initialized.get("codexHome").and_then(Value::as_str);
    if observed_home != Some(state.snapshot.canonical_codex_home.as_str()) {
        return Err(compatibility_rejected(
            profile,
            "canonical_codex_home",
            json!(state.snapshot.canonical_codex_home),
            json!(observed_home),
            "profile app-server reported a different account home",
        ));
    }
    connection
        .notify("initialized", json!({}))
        .map_err(|error| transport("write", &error.to_string()))?;
    let efforts = model_efforts(&mut connection, profile, model);
    let _ = connection.close();
    let efforts = efforts?;
    if efforts.is_empty() {
        return Err(compatibility_rejected(
            profile,
            "supported_reasoning_efforts",
            json!("at least one advertised effort"),
            json!([]),
            "profile app-server advertises no reasoning effort for this model",
        ));
    }
    Ok(efforts)
}

/// How many `model/list` pages one resolution may walk.
///
/// Model resolution "exhausts every `model/list.nextCursor`", which is only
/// safe against a server that keeps handing out cursors if the walk is bounded.
const MAX_MODEL_LIST_PAGES: usize = 1000;

/// Walk `model/list` for the reasoning efforts one model advertises.
///
/// The shapes are the pinned Codex 0.149 ones, not a guess: the required
/// subset makes `model` the model item's identity field, `nextCursor` the
/// pagination cursor, and `reasoningEffort` the identity field of each
/// `supportedReasoningEfforts` entry.
///
/// A page that disagrees is `COMPATIBILITY_REJECTED`, not `TRANSPORT_FAILURE`:
/// the bytes arrived intact and were read, and what they said is that this
/// server does not speak the pinned schema.  Calling that a transport failure
/// would advertise it as retryable and invite a caller to hammer a server that
/// will answer exactly the same way forever.  Only a connection that failed to
/// carry a reply stays a transport failure.
fn model_efforts(
    connection: &mut JsonRpcConnection<std::os::unix::net::UnixStream>,
    profile: &str,
    model: &str,
) -> Result<Vec<String>, MachineError> {
    let shape = |check: &str, expected: Value, actual: Value, reason: &str| {
        compatibility_rejected(profile, check, expected, actual, reason)
    };
    let mut cursor = Value::Null;
    for _ in 0..MAX_MODEL_LIST_PAGES {
        let page = connection
            .request("model/list", json!({"cursor": cursor, "limit": 100}))
            .map_err(|error| transport("read", &error.to_string()))?;
        let data = page.get("data").and_then(Value::as_array).ok_or_else(|| {
            shape(
                "model_list_data",
                json!("array"),
                page.get("data").cloned().unwrap_or(Value::Null),
                "model/list response lacks data",
            )
        })?;
        if let Some(item) = data
            .iter()
            .find(|item| item.get("model").and_then(Value::as_str) == Some(model))
        {
            return advertised_effort_names(profile, item);
        }
        cursor = page.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            return Ok(Vec::new());
        }
        if !cursor.is_string() {
            return Err(shape(
                "model_list_next_cursor",
                json!("string or null"),
                cursor,
                "model/list pagination cursor is invalid",
            ));
        }
    }
    Err(shape(
        "model_list_pagination",
        json!(MAX_MODEL_LIST_PAGES),
        json!("unbounded"),
        "model/list pagination did not terminate",
    ))
}

/// The efforts one `model/list` item advertises, in the server's own order.
fn advertised_effort_names(profile: &str, item: &Value) -> Result<Vec<String>, MachineError> {
    let entries = item
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            compatibility_rejected(
                profile,
                "supported_reasoning_efforts",
                json!("array"),
                item.get("supportedReasoningEfforts")
                    .cloned()
                    .unwrap_or(Value::Null),
                "model/list item lacks reasoning efforts",
            )
        })?;
    let mut efforts = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                compatibility_rejected(
                    profile,
                    "reasoning_effort",
                    json!("nonempty string"),
                    entry.get("reasoningEffort").cloned().unwrap_or(Value::Null),
                    "reasoning effort lacks its identity field",
                )
            })?;
        if !efforts.iter().any(|known: &String| known == name) {
            efforts.push(name.to_owned());
        }
    }
    Ok(efforts)
}

/// The effort an omitted `--effort` selects at Run creation.
///
/// docs/specs/README.md: "Omitted `--effort` at run creation selects the first advertised
/// effort for the resolved model and records it as the run default."  Which
/// effort that is belongs to the app-server's advertised order, not to a
/// preference of ours.
fn default_effort(efforts: &[String]) -> String {
    efforts.first().cloned().unwrap_or_default()
}

/// The digest an allocation key is bound to.
///
/// docs/specs/README.md fixes the members: "canonical workspace identity, resolved profile
/// snapshot, Controller ID/generation, control mode, execution lane,
/// purpose/parent, model/effort, assurance, required capabilities, and
/// instruction byte length and SHA-256.  Their carrier paths and secret bytes
/// are excluded."  The Run's own identity is deliberately absent: it is
/// allocated *from* this digest, so including it would make every retry
/// normalize differently and no response loss would ever be reconcilable.
#[allow(
    clippy::too_many_arguments,
    reason = "the normalized allocation identity names every start-time input at once"
)]
fn start_normalized_digest(
    view: &WorkspaceView,
    profile: &ProfileSnapshot,
    controller: &crate::run::ControllerBinding,
    control_mode: ControlMode,
    execution_lane: ExecutionLane,
    assurance: Assurance,
    purpose: &Purpose,
    parent_ref: Option<&ParentReference>,
    model: &str,
    effort: &str,
    required_capabilities: &[String],
    instructions: &str,
) -> Result<String, MachineError> {
    let document = json!({
        "operation": "start_run",
        "workspace_id": view.workspace_id,
        "canonical_workspace": view.canonical_path,
        "profile": profile.profile_name,
        "profile_snapshot_sha256": runtime_profile_snapshot_digest(profile).map_err(internal)?,
        "controller_id": controller.identity.controller_id,
        "controller_generation": controller.identity.generation,
        "control_mode": control_mode.as_str(),
        "execution_lane": execution_lane.as_str(),
        "purpose": purpose,
        "parent_ref": parent_ref,
        "model": model,
        "effort": effort,
        "requested_assurance": assurance.as_str(),
        "required_capabilities": required_capabilities,
        "instruction_byte_length": instructions.len(),
        "instruction_sha256": sha256_hex(instructions.as_bytes()),
    });
    let text = serde_json::to_string(&document)
        .map_err(|_| internal("allocation identity is unrepresentable"))?;
    let canonical = canonicalize(
        &parse(&text).map_err(|_| internal("allocation identity is unrepresentable"))?,
    )
    .map_err(|_| internal("allocation identity is unrepresentable"))?;
    Ok(sha256_hex(&canonical))
}

/// The control-socket epoch a Run's runtime record publishes, if it has one.
fn observed_control_socket_epoch(state_root: &Path, run_id: Uuid) -> u64 {
    let uid = DarwinSystem.current_uid();
    crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
        .and_then(|path| crate::worker::read_runtime_record(&path, uid))
        .map_or(1, |record| record.control_socket_epoch)
}

#[allow(
    clippy::too_many_arguments,
    reason = "a Run manifest pins every start-time decision at once"
)]
fn build_manifest(
    run_id: Uuid,
    view: &WorkspaceView,
    state: &ServerState,
    profile: ProfileSnapshot,
    model: &str,
    effort: &str,
    instructions_text: &str,
    purpose: Purpose,
    parent_ref: Option<ParentReference>,
    control_mode: ControlMode,
    execution_lane: ExecutionLane,
    assurance: Assurance,
    controller: crate::run::ControllerBinding,
    required_capabilities: Vec<String>,
    start_baseline: GitBaseline,
) -> Result<RunManifest, MachineError> {
    let instructions = InstructionSnapshot {
        schema: "dolgorae.instructions/v1".to_owned(),
        common_prefix_version: 1,
        mode_prefix_version: 1,
        purpose_prefix_version: 1,
        normalized_byte_length: instructions_text.len() as u64,
        normalized_sha256: sha256_hex(instructions_text.as_bytes()),
    };
    let agent_configuration = AgentConfigurationSnapshot {
        schema_version: 1,
        runtime_profile: profile.profile_name.clone(),
        runtime_profile_snapshot_sha256: runtime_profile_snapshot_digest(&profile)
            .map_err(internal)?,
        model: model.to_owned(),
        default_effort: effort.to_owned(),
        purpose: purpose.clone(),
        required_capabilities: required_capabilities.clone(),
        role_reference: None,
        normalized_instructions: instructions_text.to_owned(),
        instructions: instructions.clone(),
        execution_lane,
        required_assurance: assurance,
        native_subagent_policy: "enabled".to_owned(),
    };
    Ok(RunManifest {
        schema_version: 1,
        run_id,
        workspace_id: view.workspace_id.clone(),
        canonical_workspace: view.canonical_path.clone(),
        workspace_mode: view.mode,
        start_baseline,
        created_at: SystemLedgerClock::default().timestamp(),
        initial_access: Access::Read,
        control_mode,
        execution_lane,
        requested_assurance: assurance,
        achieved_assurance: assurance,
        profile_capability_snapshot: ProfileCapabilitySnapshot {
            schema_version: 1,
            profile_name: profile.profile_name.clone(),
            server_key: profile.initial_server_key.clone(),
            server_epoch: state.server_epoch,
            app_server_version: profile.codex_version.clone(),
            schema_sha256: profile.app_server_schema_sha256.clone(),
            capabilities: state
                .capabilities
                .iter()
                .map(|(name, value)| {
                    let state = serde_json::to_value(value)
                        .ok()
                        .and_then(|value| serde_json::from_value(value).ok())
                        .unwrap_or(crate::run::CapabilityState::Unverified);
                    (name.clone(), state)
                })
                .collect(),
        },
        app_server: AppServerFacts {
            version: Some(profile.codex_version.clone()),
            schema_status: Some("accepted".to_owned()),
            actual_codex_home: Some(profile.canonical_codex_home.clone()),
        },
        dolgorae: DolgoraeBuild {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            binary_sha256: current_binary_sha256()?,
            ipc_protocol_version: crate::worker::WORKER_PROTOCOL_VERSION,
        },
        model: model.to_owned(),
        initial_reasoning_effort: effort.to_owned(),
        default_reasoning_effort: effort.to_owned(),
        instructions,
        controller,
        purpose,
        parent_ref,
        required_capabilities,
        thread_id: None,
        fork_provenance: None,
        aggregate_binding: None,
        audit: AuditPolicy::default(),
        compatibility: CompatibilityVerdict::Accepted,
        profile,
        agent_configuration,
    })
}

/// Restate a Runtime Profile snapshot in the Run record's own vocabulary.
///
/// The two modules keep separate types on purpose: the profile record describes
/// a live server, the Run record pins what a Run may never change afterwards.
/// The launch-contract digest is recomputed here because it is a statement
/// about the Run record's shape, not the profile record's.
fn run_profile_snapshot(
    snapshot: &crate::profile::ProfileSnapshot,
) -> Result<ProfileSnapshot, MachineError> {
    let mut enabled = snapshot.enabled_features.clone();
    enabled.sort();
    enabled.dedup();
    let mut disabled = snapshot.disabled_features.clone();
    disabled.sort();
    disabled.dedup();
    let mut profile = ProfileSnapshot {
        schema_version: snapshot.schema_version,
        profile_name: snapshot.profile_name.clone(),
        canonical_codex_home: snapshot.canonical_codex_home.clone(),
        normalized_argv: snapshot.normalized_argv.clone(),
        launch_cwd_policy: snapshot.launch_cwd_policy.clone(),
        derived_launch_cwd: snapshot.derived_launch_cwd.clone(),
        sanitized_environment: snapshot.sanitized_environment.clone(),
        enabled_features: enabled,
        disabled_features: disabled,
        process_static_configuration: snapshot.process_static_configuration.clone(),
        initial_configuration_observation: snapshot.initial_configuration_observation.clone(),
        executable_identity: ExecutableIdentity {
            resolved_path: LosslessPath::from_path(Path::new(
                &snapshot.executable_identity.resolved_path,
            )),
            device: snapshot.executable_identity.device,
            inode: snapshot.executable_identity.inode,
            sha256: snapshot.executable_identity.sha256.clone(),
        },
        codex_version: snapshot.codex_version.clone(),
        app_server_schema_sha256: snapshot.schema_bundle_sha256.clone(),
        compatibility_manifest_sha256: snapshot.compatibility_manifest_sha256.clone(),
        launch_contract_sha256: String::new(),
        initial_server_key: snapshot.server_key.clone(),
    };
    profile.launch_contract_sha256 = launch_contract_digest(&profile).map_err(internal)?;
    Ok(profile)
}

#[cfg(target_os = "macos")]
fn start_worker(
    state_root: &Path,
    start: &crate::worker::RunWorkerStart,
) -> Result<crate::worker::StartedRun, MachineError> {
    let record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), start.run_id)
            .map_err(|error| error.machine_error(start.run_id, state_root))?;
    crate::worker::start_run_worker(state_root, start)
        .map_err(|error| error.machine_error(start.run_id, &record))
}

#[cfg(not(target_os = "macos"))]
fn start_worker(
    _state_root: &Path,
    _start: &crate::worker::RunWorkerStart,
) -> Result<crate::worker::StartedRun, MachineError> {
    Err(MachineError::new(
        "INTERNAL_ERROR",
        "per-Run workers are supported on macOS only",
        false,
        json!({"invariant": "per-Run workers are supported on macOS only"}),
    ))
}

fn current_binary_sha256() -> Result<String, MachineError> {
    let executable =
        std::env::current_exe().map_err(|_| internal("dolgorae executable is unreadable"))?;
    let bytes =
        std::fs::read(&executable).map_err(|_| internal("dolgorae executable is unreadable"))?;
    Ok(sha256_hex(&bytes))
}

fn workspace_state_root(view: &WorkspaceView) -> Result<PathBuf, MachineError> {
    Ok(default_operator_root()?
        .parent()
        .ok_or_else(|| internal("operator root has no Application Support parent"))?
        .join("workspaces")
        .join(&view.workspace_id))
}

fn required_capabilities(args: &[OsString]) -> Vec<String> {
    let mut values = all(args, "--require-capability");
    values.sort();
    values.dedup();
    values
}

fn instructions_from(args: &[OsString]) -> Result<String, MachineError> {
    if let Some(text) = optional(args, "--instructions") {
        return Ok(text);
    }
    if let Some(path) = optional(args, "--instructions-file") {
        return std::fs::read_to_string(&path).map_err(|_| {
            MachineError::invalid_argument("--instructions-file", "instructions are unreadable")
        });
    }
    if args
        .iter()
        .any(|arg| arg == OsStr::new("--instructions-stdin"))
    {
        let mut text = String::new();
        std::io::Read::take(std::io::stdin(), MAX_INSTRUCTION_BYTES)
            .read_to_string(&mut text)
            .map_err(|_| {
                MachineError::invalid_argument(
                    "--instructions-stdin",
                    "instructions are unreadable",
                )
            })?;
        return Ok(text);
    }
    Ok(String::new())
}

/// Every positional argument, in order, with option values skipped.
fn positionals(args: &[OsString]) -> Vec<&OsStr> {
    let value_flags = [
        "--workspace",
        "--controller-file",
        "--controller-fd",
        "--message",
        "--image",
        "--effort",
        "--idempotency-key",
        "--timeout",
        "--after",
        "--projection",
        "--request-id",
        "--response-fd",
    ];
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if value_flags
            .iter()
            .any(|flag| args[index] == OsStr::new(flag))
        {
            index += 2;
            continue;
        }
        if !args[index].as_encoded_bytes().starts_with(b"--") {
            values.push(args[index].as_os_str());
        }
        index += 1;
    }
    values
}

fn positional_run_id(args: &[OsString]) -> Result<Uuid, MachineError> {
    let Some(value) = positionals(args).first().copied() else {
        return Err(MachineError::invalid_argument(
            "run-id",
            "required positional argument is missing",
        ));
    };
    value
        .to_str()
        .and_then(|value| value.parse::<Uuid>().ok())
        .ok_or_else(|| MachineError::invalid_argument("run-id", "run id must be a UUID"))
}

/// The Turn `run wait <run-id> <turn-id>` addresses.
///
/// A Turn ID is an opaque app-server string, so only its presence and bound
/// are checked here; whether this Run ever had it is the worker's answer.
fn positional_turn_id(args: &[OsString]) -> Result<String, MachineError> {
    let Some(value) = positionals(args).get(1).copied() else {
        return Err(MachineError::invalid_argument(
            "turn-id",
            "wait requires both the run and turn ids",
        ));
    };
    let value = value
        .to_str()
        .ok_or_else(|| MachineError::invalid_argument("turn-id", "turn id must be UTF-8"))?;
    if value.is_empty() || value.len() > 256 {
        return Err(MachineError::invalid_argument(
            "turn-id",
            "turn id must be nonempty and at most 256 bytes",
        ));
    }
    Ok(value.to_owned())
}

/// The caller's own `--timeout`, in milliseconds.
///
/// The grammar belongs to the CLI and is measured there, so this reads the one
/// duration the argument contract accepted rather than a second, drifting copy
/// of the same rules.  SPEC-006 makes the value a bound on this reply alone,
/// never an instruction to the worker to stop the Turn.
fn caller_timeout_ms(args: &[OsString]) -> Result<Option<u64>, MachineError> {
    let Some(value) = optional(args, "--timeout") else {
        return Ok(None);
    };
    crate::cli::duration_milliseconds(&value)
        .map(Some)
        .ok_or_else(|| {
            MachineError::invalid_argument(
                "--timeout",
                "timeout must be a positive duration of at most 24h, such as 500ms, 30s, or 2m",
            )
        })
}

/// Whether a boolean switch is present.
fn switch(args: &[OsString], flag: &str) -> bool {
    args.iter().any(|arg| arg == OsStr::new(flag))
}

/// SPEC-006: "`--projection` defaults to `minimal`."  The operational
/// projection carries command argv/output, diagnostics, and generation
/// internals, so an observer that did not ask for them never receives them.
fn parse_projection(value: Option<&str>) -> Result<EventProjection, MachineError> {
    match value.unwrap_or("minimal") {
        "operational" => Ok(EventProjection::Operational),
        "minimal" => Ok(EventProjection::Minimal),
        other => Err(MachineError::invalid_argument(
            "--projection",
            format!("unsupported event projection {other}"),
        )),
    }
}

fn parse_control_mode(value: &str) -> Result<ControlMode, MachineError> {
    match value {
        "direct-interactive" | "direct_interactive" => Ok(ControlMode::DirectInteractive),
        "managed-agent" | "managed_agent" => Ok(ControlMode::ManagedAgent),
        other => Err(MachineError::invalid_argument(
            "--control-mode",
            format!("unsupported control mode {other}"),
        )),
    }
}

fn parse_execution_lane(value: &str) -> Result<ExecutionLane, MachineError> {
    match value {
        "shared-readonly" | "shared_readonly" => Ok(ExecutionLane::SharedReadonly),
        "dedicated" => Ok(ExecutionLane::Dedicated),
        other => Err(MachineError::invalid_argument(
            "--execution-lane",
            format!("unsupported execution lane {other}"),
        )),
    }
}

fn parse_assurance(value: &str) -> Result<Assurance, MachineError> {
    match value {
        "best-effort-personal-alpha" | "best_effort_personal_alpha" => {
            Ok(Assurance::BestEffortPersonalAlpha)
        }
        "verified-thread-scoped-control" | "verified_thread_scoped_control" => {
            Ok(Assurance::VerifiedThreadScopedControl)
        }
        "strong-process-containment" | "strong_process_containment" => {
            Ok(Assurance::StrongProcessContainment)
        }
        other => Err(MachineError::invalid_argument(
            "--required-assurance",
            format!("unsupported assurance level {other}"),
        )),
    }
}

fn parse_purpose(value: &str) -> Result<PurposeKind, MachineError> {
    match value {
        "interactive" => Ok(PurposeKind::Interactive),
        "planning" => Ok(PurposeKind::Planning),
        "implementation" => Ok(PurposeKind::Implementation),
        "review" => Ok(PurposeKind::Review),
        "research" => Ok(PurposeKind::Research),
        "discussion" => Ok(PurposeKind::Discussion),
        "workflow-stage" | "workflow_stage" => Ok(PurposeKind::WorkflowStage),
        "other" => Ok(PurposeKind::Other),
        other => Err(MachineError::invalid_argument(
            "--purpose",
            format!("unsupported purpose {other}"),
        )),
    }
}

fn optional(args: &[OsString], flag: &str) -> Option<String> {
    all(args, flag).into_iter().next()
}

/// The Run's public parent metadata, or nothing.
///
/// docs/specs/README.md: "`parent_ref.namespace`, `kind`, and `id` are all-or-none,
/// limited to 128, 64, and 256 UTF-8 bytes, and reject NUL/control
/// characters", and "a `direct_interactive` Primary Run MUST NOT carry a
/// parent reference".  Both are decided before the allocation key is reserved:
/// a Run the record layer could never publish must never consume a key, and a
/// caller's own argv mistake is `INVALID_ARGUMENT`, not a durable-state
/// invariant violation.
fn parent_reference(
    args: &[OsString],
    control_mode: ControlMode,
) -> Result<Option<ParentReference>, MachineError> {
    let members = (
        optional(args, "--parent-namespace"),
        optional(args, "--parent-kind"),
        optional(args, "--parent-id"),
    );
    let (namespace, kind, id) = match members {
        (None, None, None) => return Ok(None),
        (Some(namespace), Some(kind), Some(id)) => (namespace, kind, id),
        _ => {
            return Err(MachineError::invalid_argument(
                "--parent-namespace",
                "parent namespace, kind, and id are all-or-none",
            ));
        }
    };
    if control_mode == ControlMode::DirectInteractive {
        return Err(MachineError::invalid_argument(
            "--parent-namespace",
            "a direct_interactive Run cannot carry a parent reference",
        ));
    }
    Ok(Some(ParentReference {
        namespace: bounded_metadata("--parent-namespace", namespace, 128)?,
        kind: bounded_metadata("--parent-kind", kind, 64)?,
        id: bounded_metadata("--parent-id", id, 256)?,
    }))
}

/// One optional bounded metadata value, refused at the boundary.
fn bounded_option(
    args: &[OsString],
    flag: &str,
    maximum: usize,
) -> Result<Option<String>, MachineError> {
    optional(args, flag)
        .map(|value| bounded_metadata(flag, value, maximum))
        .transpose()
}

/// docs/specs/README.md bounds public Run metadata in UTF-8 bytes and rejects NUL and
/// control characters.  A value a Run manifest could never hold is a fact
/// about the caller's argv, so it is refused here rather than reaching the
/// record layer and surfacing as a state-invariant violation.
fn bounded_metadata(flag: &str, value: String, maximum: usize) -> Result<String, MachineError> {
    if value.is_empty() {
        return Err(MachineError::invalid_argument(
            flag,
            "value cannot be empty",
        ));
    }
    if value.len() > maximum {
        return Err(MachineError::invalid_argument(
            flag,
            format!("value exceeds {maximum} UTF-8 bytes"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(MachineError::invalid_argument(
            flag,
            "value cannot contain NUL or control characters",
        ));
    }
    Ok(value)
}

/// One opaque allocation key, bounded before anything durable is written.
///
/// docs/specs/README.md: "`--idempotency-key` is an opaque, nonempty UTF-8 string scoped
/// to the run."  Opaque is why only emptiness and the bound are checked: the
/// bytes are never interpreted, only stored and compared.  The bound is the
/// 256 UTF-8 bytes the checked schema already allows a Run's longest public
/// identity string, because an unbounded key is a caller-chosen amount of
/// durable state and reaches the ledger's own bounded records.
fn bounded_key(args: &[OsString], flag: &str, maximum: usize) -> Result<String, MachineError> {
    let value = required(args, flag)?;
    if value.is_empty() {
        return Err(MachineError::invalid_argument(
            flag,
            "value cannot be empty",
        ));
    }
    if value.len() > maximum {
        return Err(MachineError::invalid_argument(
            flag,
            format!("value exceeds {maximum} UTF-8 bytes"),
        ));
    }
    Ok(value)
}

fn required(args: &[OsString], flag: &str) -> Result<String, MachineError> {
    optional(args, flag)
        .ok_or_else(|| MachineError::invalid_argument(flag, "required option is missing"))
}

fn all(args: &[OsString], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new(flag) {
            if let Some(value) = args.get(index + 1).and_then(|value| value.to_str()) {
                values.push(value.to_owned());
            }
            index += 2;
            continue;
        }
        let bytes = args[index].as_encoded_bytes();
        if bytes.starts_with(flag.as_bytes())
            && bytes.get(flag.len()) == Some(&b'=')
            && let Ok(value) = std::str::from_utf8(&bytes[flag.len() + 1..])
        {
            values.push(value.to_owned());
        }
        index += 1;
    }
    values
}

fn internal(reason: impl Into<String>) -> MachineError {
    let reason = reason.into();
    MachineError::new(
        "INTERNAL_ERROR",
        reason.clone(),
        false,
        json!({"invariant": reason}),
    )
}

/// A profile app-server call that never reached an accepted request.
///
/// Every caller of this is a probe made before the Run exists, so nothing the
/// caller asked for can have been accepted; that is what makes it retryable.
fn transport(stage: &str, reason: &str) -> MachineError {
    MachineError::new(
        "TRANSPORT_FAILURE",
        reason,
        true,
        json!({"stage": stage, "acceptance": "not_written", "request_id": Value::Null}),
    )
}

fn compatibility_rejected(
    profile: &str,
    check: &str,
    expected: Value,
    actual: Value,
    reason: &str,
) -> MachineError {
    MachineError::new(
        "COMPATIBILITY_REJECTED",
        reason,
        false,
        json!({
            "profile": profile,
            "check": check,
            "expected": expected,
            "actual": actual,
        }),
    )
}

/// A control response body is bounded by the interaction payload contract.
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// A Turn's text is one bounded payload, not a stream: docs/specs/README.md bounds "raw
/// app-server payloads selected for ledger representation to 2 MiB", and the
/// message the caller pipes in is exactly such a payload.
const MAX_MESSAGE_BYTES: u64 = 2 * 1024 * 1024;
/// Run instructions are pinned at start and never streamed.
const MAX_INSTRUCTION_BYTES: u64 = 1024 * 1024;

/// The live-Run half of a controller reset, wired to the real Run worker.
///
/// `controller` owns the reset's decision and its durable journal; this owns
/// how the reset reaches the Run's startup lock and the Run's worker, which is
/// exactly the composition this module exists for.
pub struct LiveRunReset;

impl RunResetEnvironment for LiveRunReset {
    fn open_run_lock(
        &self,
        state_root: &Path,
        run_id: Uuid,
    ) -> Result<Box<dyn RunMutationLock>, MachineError> {
        Ok(Box::new(RunStartupLock::open(state_root, run_id)?))
    }

    /// A worker is only "live" if it answers.  A runtime record on its own is
    /// a claim; `hello` is frozen control v1, so it answers across builds, and
    /// the identity it returns is what the fingerprint is made of.
    fn worker_fingerprint(&self, state_root: &Path, run_id: Uuid) -> Option<String> {
        let uid = DarwinSystem.current_uid();
        let response = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
            ControlRequestV1::Hello { expected }
        });
        match response {
            Ok(ControlResponseV1::Hello { hello }) => serde_json::to_string(&hello.identity).ok(),
            _ => None,
        }
    }

    /// APPLY for a Run with a live worker.
    ///
    /// The reset's durable prepare is already fsynced, so the worker refuses
    /// every mutation that reaches it from here on.  This call is queued to
    /// the Run's own drain thread, which is the in-process mutation serializer
    /// the resetting process cannot hold from outside: it is ordered behind
    /// whatever the drain was already doing, so a Turn accepted before the
    /// prepare landed is reported rather than missed.  An active Turn, a
    /// pending interaction, an unreachable worker, or a drain that does not
    /// answer inside its budget all roll the prepare back instead of
    /// committing a new Controller over live work.
    fn fence_live_worker(
        &self,
        state_root: &Path,
        run_id: Uuid,
        confirmation: Uuid,
    ) -> Result<(), MachineError> {
        let uid = DarwinSystem.current_uid();
        let response = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
            ControlRequestV1::ResetFence {
                expected,
                caller: None,
                confirmation,
            }
        })
        .map_err(|_| {
            // A worker this reset already proved live stopped answering.  That
            // is not a statement about the Run's lifecycle, which is exactly
            // what a reset refusal would have to name, so it is reported as
            // the retryable contention it is.
            crate::controller::run_busy(
                run_id,
                "attachment",
                "this run's worker stopped answering while the reset fenced it",
            )
        })?;
        match response {
            ControlResponseV1::ResetFence {
                lifecycle,
                active_turn,
                pending_interactions,
                ..
            } => {
                let state = parse_lifecycle(&lifecycle)?;
                let mut blockers = Vec::new();
                if active_turn.is_some() {
                    blockers.push("active_turn".to_owned());
                }
                if pending_interactions > 0 {
                    blockers.push("pending_interaction".to_owned());
                }
                if !matches!(lifecycle.as_str(), "idle" | "closed" | "outcome_unknown") {
                    blockers.push("lifecycle_not_resettable".to_owned());
                }
                if blockers.is_empty() {
                    return Ok(());
                }
                Err(crate::controller::reset_blocked_by(run_id, state, blockers))
            }
            ControlResponseV1::Failed {
                code,
                message,
                retryable,
                details,
            } => Err(MachineError::new(&code, &message, retryable, details)),
            _ => Err(internal("reset fence answer is unrecognized")),
        }
    }
}

/// The Run lifecycle a worker reported, as the checked contract names it.
fn parse_lifecycle(value: &str) -> Result<crate::domain::RunLifecycle, MachineError> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|_| internal("worker reported an unknown run lifecycle"))
}

/// The Run startup/mutation lock as a file range, for the resetting CLI.
///
/// One descriptor is opened for the whole reset and kept alive across PREPARE,
/// APPLY, and COMMIT: POSIX record locks are per process, so closing any
/// descriptor for this file would drop every range this process holds on it,
/// including the one the next phase depends on.
struct RunStartupLock {
    lock: crate::worker::StartupLockFile,
    run_id: Uuid,
}

impl RunStartupLock {
    fn open(state_root: &Path, run_id: Uuid) -> Result<Self, MachineError> {
        let uid = DarwinSystem.current_uid();
        let runtime = crate::worker::runtime_root(state_root);
        let unavailable = || {
            crate::controller::run_busy(run_id, "startup", "this run's startup lock is unavailable")
        };
        crate::worker::prepare_runtime_root(&runtime, uid).map_err(|_| unavailable())?;
        let path = crate::worker::startup_lock_path(&runtime, run_id);
        let lock = crate::worker::StartupLockFile::open(&path, uid).map_err(|_| unavailable())?;
        Ok(Self { lock, run_id })
    }
}

impl RunMutationLock for RunStartupLock {
    fn acquire(&self) -> Result<(), MachineError> {
        // The normative ten-second startup contention budget: a reset waits
        // for the range exactly as long as a start attempt does, and reports
        // the same registered busy answer when it loses.
        self.lock
            .hold_startup_range(crate::worker::STARTUP_BOUND_TIMEOUT)
            .map_err(|_| {
                MachineError::new(
                    "RUN_BUSY",
                    "another operation holds this run's startup lock",
                    true,
                    serde_json::json!({"run_id": self.run_id, "owner_kind": "startup"}),
                )
            })
    }

    fn release(&self) {
        let _ = self.lock.release_startup_range();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared fake app-server, running one manifest-validated scenario.
    ///
    /// ADR-014 keeps the fixture an independent process with an independent
    /// JSON reader, so what these cases prove about model resolution is a
    /// property of the product's own ingest path rather than of a Rust stub
    /// agreeing with the Rust code it is checking.  Nothing here needs a Codex
    /// on the machine, which is the point: the walk, its bound, and every
    /// pinned shape are decided by the scenario, not by whatever release
    /// happens to be installed.
    struct FakeAppServer {
        child: std::process::Child,
        root: PathBuf,
        socket: PathBuf,
    }

    impl FakeAppServer {
        fn start(scenario: &str) -> Self {
            let root = std::env::temp_dir().join(format!("dlg-fas-{}", Uuid::now_v7()));
            std::fs::create_dir_all(&root).expect("fixture root");
            let socket = root.join("s.sock");
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/fake_app_server");
            let child = std::process::Command::new(
                std::env::var("DOLGORAE_TEST_PYTHON").unwrap_or_else(|_| "python3".to_owned()),
            )
            .arg(&fixture)
            .arg("--socket")
            .arg(&socket)
            .arg("--scenario")
            .arg(fixture.join("scenarios").join(scenario))
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("the fake app-server fixture must be launchable");
            Self {
                child,
                root,
                socket,
            }
        }

        /// One connection to the fixture, retried until it is listening.
        ///
        /// The fixture binds before it serves, so the only race is between
        /// this process and the child's own startup; polling the connect is
        /// what removes it without an inherited descriptor.
        fn connect(&self) -> JsonRpcConnection<std::os::unix::net::UnixStream> {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                match JsonRpcConnection::connect(&self.socket, Duration::from_secs(30)) {
                    Ok(connection) => return connection,
                    Err(error) if std::time::Instant::now() < deadline => {
                        let _ = error;
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => panic!("the fake app-server never listened: {error}"),
                }
            }
        }

        /// Complete the handshake the product performs before it resolves a
        /// model, so the walk happens on the connection it really uses.
        fn initialized(&self) -> JsonRpcConnection<std::os::unix::net::UnixStream> {
            let mut connection = self.connect();
            connection
                .request("initialize", json!({"clientInfo": {"name": "dolgorae"}}))
                .expect("initialize");
            connection.notify("initialized", json!({})).expect("notify");
            connection
        }
    }

    impl Drop for FakeAppServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// docs/specs/README.md has model resolution "exhaust every `model/list.nextCursor`",
    /// and SPEC-006 makes `supportedReasoningEfforts[].reasoningEffort` the
    /// advertised order an omitted `--effort` selects from.
    #[test]
    fn model_resolution_walks_every_page_and_keeps_the_advertised_order() {
        let fake = FakeAppServer::start("model_list_paginated.json");
        let mut connection = fake.initialized();
        assert_eq!(
            model_efforts(&mut connection, "default", "gpt-5.6").unwrap(),
            ["medium", "low", "high"],
            "the walk must reach the last page and keep the server's own order"
        );
    }

    /// A model the whole catalogue never names is not an error here: the walk
    /// ends at the null cursor with nothing advertised, and it is the caller
    /// that decides what an empty answer means.
    #[test]
    fn a_model_no_page_advertises_resolves_to_no_effort() {
        let fake = FakeAppServer::start("model_list_paginated.json");
        let mut connection = fake.initialized();
        assert!(
            model_efforts(&mut connection, "default", "gpt-absent")
                .unwrap()
                .is_empty()
        );
    }

    /// A reply that arrived intact and disagrees with the pinned Codex 0.149
    /// shapes is `COMPATIBILITY_REJECTED`, never a retryable transport
    /// failure: nothing about the connection failed, and asking the same
    /// server again can only produce the same answer.
    #[test]
    fn model_list_shape_faults_are_compatibility_rejections() {
        let fake = FakeAppServer::start("model_list_shape_faults.json");
        let mut connection = fake.initialized();
        for check in [
            "model_list_data",
            "model_list_next_cursor",
            "supported_reasoning_efforts",
            "reasoning_effort",
            "reasoning_effort",
        ] {
            let error = model_efforts(&mut connection, "default", "gpt-5.6").unwrap_err();
            assert_eq!(error.code, "COMPATIBILITY_REJECTED", "{check}");
            assert!(!error.retryable, "{check}");
            assert_eq!(error.details["check"], check);
            assert_eq!(error.details["profile"], "default");
            assert_eq!(
                error.details.as_object().unwrap().len(),
                4,
                "the contract names profile, check, expected, and actual"
            );
        }
    }

    /// The walk is bounded, so a server that keeps handing out cursors is
    /// refused rather than followed forever.
    #[test]
    fn unbounded_model_list_pagination_is_refused() {
        let fake = FakeAppServer::start("model_list_unbounded_pagination.json");
        let mut connection = fake.initialized();
        let error = model_efforts(&mut connection, "default", "gpt-5.6").unwrap_err();
        assert_eq!(error.code, "COMPATIBILITY_REJECTED");
        assert_eq!(error.details["check"], "model_list_pagination");
        assert_eq!(error.details["expected"], json!(MAX_MODEL_LIST_PAGES));
    }

    /// docs/specs/README.md: "`parent_ref.namespace`, `kind`, and `id` are all-or-none,
    /// limited to 128, 64, and 256 UTF-8 bytes, and reject NUL/control
    /// characters", and "a `direct_interactive` Primary Run MUST NOT carry a
    /// parent reference".
    #[test]
    fn parent_metadata_is_all_or_none_bounded_and_managed_agent_only() {
        let argv = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .flat_map(|(flag, value)| [OsString::from(*flag), OsString::from(*value)])
                .collect::<Vec<_>>()
        };
        let complete = argv(&[
            ("--parent-namespace", "dolgorae.orchestrated-session.v1"),
            ("--parent-kind", "specialist"),
            ("--parent-id", "018f0c6a-7b01-7abc-8def-0123456789ab"),
        ]);
        let parent = parent_reference(&complete, ControlMode::ManagedAgent)
            .unwrap()
            .unwrap();
        assert_eq!(parent.kind, "specialist");
        assert_eq!(parent.id, "018f0c6a-7b01-7abc-8def-0123456789ab");

        assert!(
            parent_reference(&Vec::new(), ControlMode::ManagedAgent)
                .unwrap()
                .is_none()
        );
        assert!(
            parent_reference(&complete, ControlMode::DirectInteractive).is_err(),
            "a direct_interactive Run cannot carry a parent reference"
        );
        for partial in [
            argv(&[("--parent-id", "x")]),
            argv(&[("--parent-namespace", "n"), ("--parent-kind", "k")]),
            argv(&[("--parent-namespace", "n"), ("--parent-id", "i")]),
        ] {
            assert_eq!(
                parent_reference(&partial, ControlMode::ManagedAgent)
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
        let long_namespace = "n".repeat(129);
        let long_kind = "k".repeat(65);
        // 86 characters, 258 UTF-8 bytes: the bound counts bytes, not chars.
        let long_id = "\u{fe0f}".repeat(86);
        for over in [
            argv(&[
                ("--parent-namespace", long_namespace.as_str()),
                ("--parent-kind", "k"),
                ("--parent-id", "i"),
            ]),
            argv(&[
                ("--parent-namespace", "n"),
                ("--parent-kind", long_kind.as_str()),
                ("--parent-id", "i"),
            ]),
            argv(&[
                ("--parent-namespace", "n"),
                ("--parent-kind", "k"),
                ("--parent-id", long_id.as_str()),
            ]),
            argv(&[
                ("--parent-namespace", "n"),
                ("--parent-kind", "k"),
                ("--parent-id", "with\u{7}control"),
            ]),
            argv(&[
                ("--parent-namespace", ""),
                ("--parent-kind", "k"),
                ("--parent-id", "i"),
            ]),
        ] {
            assert_eq!(
                parent_reference(&over, ControlMode::ManagedAgent)
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
    }

    /// docs/specs/README.md: "`--idempotency-key` is an opaque, nonempty UTF-8 string
    /// scoped to the run."  Opaque is why only emptiness and the bound are
    /// checked, and the check happens before anything durable is written.
    #[test]
    fn an_allocation_key_is_nonempty_and_bounded() {
        let argv = |value: &str| {
            vec![
                OsString::from("--idempotency-key"),
                OsString::from(value.to_owned()),
            ]
        };
        assert_eq!(
            bounded_key(&argv("k1"), "--idempotency-key", 256).unwrap(),
            "k1"
        );
        assert_eq!(
            bounded_key(&argv(&"k".repeat(256)), "--idempotency-key", 256).unwrap(),
            "k".repeat(256)
        );
        // 86 characters, 258 UTF-8 bytes: the bound counts bytes, not chars.
        for refused in [String::new(), "k".repeat(257), "\u{fe0f}".repeat(86)] {
            assert_eq!(
                bounded_key(&argv(&refused), "--idempotency-key", 256)
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
        assert_eq!(
            bounded_key(&Vec::new(), "--idempotency-key", 256)
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn capabilities_flow_through_adapter_independent_service() {
        let result = CoreSemanticService
            .execute(&SemanticCommand::RuntimeCapabilities)
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["features"]["persistent_runs"], false);
    }

    #[test]
    fn observer_verbs_never_demand_controller_authority() {
        for verb in [RunVerb::Status, RunVerb::Wait, RunVerb::Events] {
            assert!(
                !verb.mutates(),
                "{} must stay an observer",
                verb.operation_name()
            );
        }
        for verb in [
            RunVerb::Send,
            RunVerb::Submit,
            RunVerb::Respond,
            RunVerb::Interrupt,
            RunVerb::Close,
        ] {
            assert!(
                verb.mutates(),
                "{} must require a controller",
                verb.operation_name()
            );
        }
    }

    /// SPEC-006: "`--projection` defaults to `minimal`."  The operational
    /// projection carries command argv/output, diagnostics, and generation
    /// internals, which an observer who did not ask for them never receives.
    #[test]
    fn an_omitted_projection_selects_minimal() {
        assert_eq!(parse_projection(None).unwrap(), EventProjection::Minimal);
        assert_eq!(
            parse_projection(Some("operational")).unwrap(),
            EventProjection::Operational
        );
        assert_eq!(
            parse_projection(Some("minimal")).unwrap(),
            EventProjection::Minimal
        );
        assert_eq!(
            parse_projection(Some("controller_timeline"))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    /// SPEC-006: `--after` "defaults to the string `\"0\"` and accepts the
    /// canonical unsigned decimal ledger sequence string without leading
    /// zeroes"; a noncanonical or beyond-head cursor is `EVENT_CURSOR_INVALID`,
    /// whose `requested_cursor` is itself a canonical cursor.
    #[test]
    fn event_cursors_are_canonical_decimal_or_refused_as_cursors() {
        let with =
            |value: &str| requested_cursor(&[OsString::from("--after"), OsString::from(value)]);
        assert!(matches!(
            requested_cursor(&[]).unwrap(),
            RequestedCursor::Exact(0)
        ));
        assert!(matches!(with("0").unwrap(), RequestedCursor::Exact(0)));
        assert!(matches!(with("7").unwrap(), RequestedCursor::Exact(7)));
        // Noncanonical but nameable: the refusal reports the canonical form
        // the checked contract binds `requested_cursor` to.
        let RequestedCursor::Refused(canonical) = with("007").unwrap() else {
            panic!("a leading-zero cursor is not canonical");
        };
        assert_eq!(canonical, "7");
        let RequestedCursor::Refused(canonical) = with("00").unwrap() else {
            panic!("a leading-zero zero is not canonical");
        };
        assert_eq!(canonical, "0");
        // Inside the contract's twenty-digit cursor domain but past every
        // reachable ledger head.
        let RequestedCursor::Refused(canonical) = with("99999999999999999999").unwrap() else {
            panic!("a cursor beyond the sequence domain is refused as a cursor");
        };
        assert_eq!(canonical, "99999999999999999999");
        for rejected in ["", "+5", "5x", "-1", "1e3", " 5", "1_000"] {
            assert_eq!(
                with(rejected).unwrap_err().code,
                "INVALID_ARGUMENT",
                "{rejected} is not a cursor at all"
            );
        }
    }

    #[test]
    fn leaf_options_are_read_without_swallowing_the_positional_run_id() {
        let args = [
            "--workspace",
            "/tmp/w",
            "018f0c6a-7b01-7abc-8def-0123456789ab",
            "--message",
            "hello",
            "--idempotency-key",
            "k1",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        assert_eq!(optional(&args, "--message").as_deref(), Some("hello"));
        assert_eq!(
            positional_run_id(&args).unwrap().to_string(),
            "018f0c6a-7b01-7abc-8def-0123456789ab"
        );
        let request = turn_request(&args).unwrap();
        assert_eq!(request.idempotency_key, "k1");
        assert!(request.images.is_empty());
    }
}
