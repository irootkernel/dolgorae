use crate::app_server::JsonRpcConnection;
use crate::audit::AuditKind;
use crate::conformance::{
    BootstrapRecordKind, BootstrapRequest, ConformantLedger, IdempotencyIntent,
    IdempotencyOperation,
};
use crate::controller::{
    CredentialCarrier, RunMutationLock, RunResetEnvironment, authorize_controller,
    binding_from_carrier, carrier_from_options, load_reconciled_controller_binding,
    presented_controller_from_carrier,
};
use crate::darwin::DarwinSystem;
use crate::domain::{
    Access, AggregateKind, Assurance, ControlMode, ExecutionLane, Purpose, PurposeKind,
    RunLifecycle,
};
use crate::engagement::EngagementStore;
use crate::event::EventProjection;
use crate::global_runtime::{
    GlobalMembershipStore, GlobalProfileBinding, MembershipDisposition, ResolvedGlobalProfile,
};
use crate::interaction::MAX_RESPONSE_BYTES;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{
    LedgerClock, SystemLedgerClock, audit_integrity_error as audit_integrity, event_cursor_invalid,
    parse_event_cursor,
};
use crate::machine::MachineError;
use crate::mutation_admission::{
    MutationAdmission, admit_mutation_digest, complete_mutation_admission,
};
use crate::paths::DolgoraeHome;
use crate::profile::ServerState;
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, AppServerFacts, AuditPolicy,
    CompatibilityVerdict, DolgoraeBuild, ExecutableIdentity, ForkProvenance, InstructionSnapshot,
    ParentReference, ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest, RunStore,
    StartReservation, StartReservationStore, WriteContinuationProvenance,
    agent_configuration_digest, launch_contract_digest, run_root,
};
use crate::runtime::{RuntimeCapabilities, capabilities};
use crate::specialist::ReviewerRuntimePlan;
use crate::specialist::ReviewerRuntimeRequest;
use crate::turn::ImageDetail;
use crate::worker::{
    ControlRequestV1, ControlResponseV1, DedicatedServerBootstrap, SessionAttach, TurnControlImage,
    TurnControlRequest, WorkerSessionBootstrap,
};
use crate::workspace::{
    GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode, WorkspaceService,
    WorkspaceView, isolated_specialist_root, verify_secure_directory,
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    Pending,
    Respond,
    Interrupt,
    Pause,
    Resume,
    Recover,
    Reconcile,
    Fork,
    AcquireWrite,
    ReleaseWrite,
    CreateWriteContinuation,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceWriterVerb {
    Status,
    Reset,
    HandoffPrepare,
    HandoffCommit,
    HandoffCancel,
}

pub fn workspace_writer(
    verb: WorkspaceWriterVerb,
    args: &[OsString],
) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let state_root = workspace_state_root(&view)?;
    let uid = DarwinSystem.current_uid();
    let writer = crate::writer::WriterStore::new(&state_root, &view.workspace_id, uid);
    match verb {
        WorkspaceWriterVerb::Status => writer.status_value(),
        WorkspaceWriterVerb::Reset => reset_writer(&view, &state_root, args),
        WorkspaceWriterVerb::HandoffPrepare => prepare_writer_handoff(&view, &state_root, args),
        WorkspaceWriterVerb::HandoffCommit => commit_writer_handoff(&view, &state_root, args),
        WorkspaceWriterVerb::HandoffCancel => cancel_writer_handoff(&view, &state_root, args),
    }
}

pub fn interaction_get(args: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let state_root = workspace_state_root(&view)?;
    let run_id = positional_run_id(args)?;
    let request_id = positional_interaction_id(args)?;
    if optional(args, "--controller-file").is_none() && optional(args, "--controller-fd").is_none()
    {
        return Err(MachineError::interaction_full_payload_requires_controller(
            run_id,
            "run.interaction.get",
        ));
    }
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let binding = load_reconciled_controller_binding(&state_root, run_id)?;
    authorize_controller(run_id, "run.interaction.get", &binding, &carrier)?;
    checked_interaction(
        &durable_interactions(&state_root, run_id)?,
        run_id,
        request_id,
    )
}

fn checked_interaction(
    interactions: &[(Value, u64)],
    run_id: Uuid,
    request_id: Uuid,
) -> Result<Value, MachineError> {
    let interaction = interactions
        .iter()
        .find_map(|(interaction, _)| {
            (interaction.get("request_id") == Some(&json!(request_id))).then_some(interaction)
        })
        .ok_or_else(|| {
            MachineError::interaction_not_found(
                run_id,
                request_id,
                "interaction is not present in this run",
            )
        })?
        .clone();
    let parsed = crate::interaction::Interaction::parse(interaction.clone(), run_id)?;
    crate::interaction_payload::validate(&parsed)?;
    Ok(interaction)
}

pub fn run_list(args: &[OsString]) -> Result<Value, MachineError> {
    let path = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let workspace = WorkspaceService::system()?.discover(path.as_deref())?;
    let state_root = workspace_state_root(&workspace)?;
    let observations = CoreSemanticService.list_runs(&workspace, None)?;
    let mut items = Vec::new();
    for (snapshot, _) in observations {
        let verdict = projection_identity_verdict(&state_root, snapshot.manifest.run_id);
        let terminal = durable_last_terminal(&state_root, snapshot.manifest.run_id, &snapshot)?;
        items.push(snapshot.run_value(verdict, terminal));
    }
    Ok(json!({"items":items}))
}

pub fn run_artifact(args: &[OsString], read: bool) -> Result<Value, MachineError> {
    let path = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let workspace = WorkspaceService::system()?.discover(path.as_deref())?;
    let root = workspace_state_root(&workspace)?;
    let run_id = positional_run_id(args)?;
    let positions = positionals(args);
    let id = positions
        .get(1)
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            MachineError::invalid_argument("artifact-id", "artifact UUID is required")
        })?;
    let carrier = if optional(args, "--controller-file").is_some()
        || optional(args, "--controller-fd").is_some()
    {
        Some(carrier_from_options(
            args,
            "--controller-file",
            "--controller-fd",
        )?)
    } else {
        None
    };
    let snapshot = RunSources::load(&root, run_id, observed_control_socket_epoch(&root, run_id))?;
    if !read {
        return Ok(
            crate::artifact::metadata(&root, &snapshot, id, carrier.as_ref())?
                .artifact
                .machine_value(run_id),
        );
    }
    let offset = parse_u64_flag(args, "--offset")?;
    let length = u32::try_from(parse_u64_flag(args, "--length")?).map_err(|_| {
        MachineError::invalid_argument("--length", "chunk length exceeds the wire bound")
    })?;
    let chunk = crate::artifact::chunk(&root, &snapshot, id, (offset, length), carrier.as_ref())?;
    use base64::Engine as _;
    Ok(
        json!({"metadata":chunk.metadata.machine_value(run_id),"offset":chunk.offset,"byte_length":chunk.length,"content_base64":base64::engine::general_purpose::STANDARD.encode(&chunk.data),"eof":chunk.eof}),
    )
}

fn interaction_pending_at(state_root: &Path, run_id: Uuid) -> Result<Value, MachineError> {
    let snapshot = RunSources::load(
        state_root,
        run_id,
        observed_control_socket_epoch(state_root, run_id),
    )?;
    let mut items = Vec::new();
    for (interaction, _) in
        durable_interactions_at(state_root, run_id, snapshot.stamp.run_state_revision)?
    {
        let interaction = crate::interaction::Interaction::parse(interaction, run_id)?;
        if !interaction.pending_at(&snapshot) {
            continue;
        }
        let summary =
            interaction.observer_summary(crate::interaction::InteractionStatus::Pending)?;
        items.push(json!({
            "schema_version": 1,
            "request_id": interaction.request_id,
            "run_id": run_id,
            "kind": interaction.kind,
            "status": "pending",
            "title": summary.safe_title,
            "controller_kind": snapshot.manifest.controller.identity.kind.as_str(),
            "user_escalation_required": summary.requires_user_escalation,
            "protected_input": summary.contains_protected_input,
            "created_at": interaction.opened_at,
            "expires_at": Value::Null,
            "resolved_at": Value::Null,
        }));
    }
    Ok(json!({"items": items}))
}

fn durable_interactions(
    state_root: &Path,
    run_id: Uuid,
) -> Result<Vec<(Value, u64)>, MachineError> {
    durable_interactions_at(state_root, run_id, durable_head(state_root, run_id)?)
}

pub(crate) fn durable_interactions_at(
    state_root: &Path,
    run_id: Uuid,
    head: u64,
) -> Result<Vec<(Value, u64)>, MachineError> {
    let ledger = crate::ledger::ObservedLedger::open_run(state_root, run_id, head)?;
    let decisions = ledger
        .payloads_of_kind(AuditKind::ApprovalDecided)
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?
        .into_iter()
        .filter_map(|payload| {
            let request_id = payload.get("request_id")?.as_str()?.parse::<u64>().ok()?;
            Some((request_id, payload))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    ledger
        .payloads_of_kind(AuditKind::ApprovalRequested)
        .map_err(|error| audit_integrity(run_id, head, &error.to_string()))?
        .into_iter()
        .map(|wrapper| {
            let upstream_id = wrapper
                .get("upstream_request_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| internal("durable interaction upstream id is missing"))?;
            let mut interaction = wrapper
                .get("interaction")
                .cloned()
                .ok_or_else(|| internal("durable normalized interaction is missing"))?;
            if let Some(decision) = decisions.get(&upstream_id) {
                let object = interaction
                    .as_object_mut()
                    .ok_or_else(|| internal("durable normalized interaction is invalid"))?;
                let resolution = decision
                    .get("resolution")
                    .cloned()
                    .ok_or_else(|| internal("durable interaction resolution is missing"))?;
                object.insert(
                    "status".to_owned(),
                    json!(
                        if resolution.get("outcome").and_then(Value::as_str) == Some("stale") {
                            "stale"
                        } else {
                            "resolved"
                        }
                    ),
                );
                object.insert(
                    "resolved_at".to_owned(),
                    decision.get("resolved_at").cloned().unwrap_or(Value::Null),
                );
                object.insert("resolution".to_owned(), resolution);
            }
            Ok((interaction, upstream_id))
        })
        .collect()
}

fn resolve_public_interaction_id(
    state_root: &Path,
    run_id: Uuid,
    request_id: &str,
) -> Result<u64, MachineError> {
    let public_id = Uuid::parse_str(request_id)
        .ok()
        .filter(|value| value.get_version_num() == 7)
        .ok_or_else(|| {
            MachineError::invalid_argument("--request-id", "request id must be a UUIDv7")
        })?;
    durable_interactions(state_root, run_id)?
        .into_iter()
        .find_map(|(interaction, upstream)| {
            (interaction.get("request_id") == Some(&json!(public_id))).then_some(upstream)
        })
        .ok_or_else(|| {
            MachineError::interaction_not_found(
                run_id,
                public_id,
                "interaction is not present in this run",
            )
        })
}

impl RunVerb {
    #[must_use]
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Self::Send
                | Self::Submit
                | Self::Respond
                | Self::Interrupt
                | Self::Pause
                | Self::Resume
                | Self::Recover
                | Self::Reconcile
                | Self::Fork
                | Self::Close
                | Self::AcquireWrite
                | Self::ReleaseWrite
                | Self::CreateWriteContinuation
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
            Self::Pending => "run.pending",
            Self::Respond => "run.respond",
            Self::Interrupt => "run.interrupt",
            Self::Pause => "run.pause",
            Self::Resume => "run.resume",
            Self::Recover => "run.recover",
            Self::Reconcile => "run.reconcile",
            Self::Fork => "run.fork",
            Self::AcquireWrite => "run.acquire_write",
            Self::ReleaseWrite => "run.release_write",
            Self::CreateWriteContinuation => "run.create_write_continuation",
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
    pub global_profile_binding: GlobalProfileBinding,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentConfigurationInput {
    pub schema_version: u32,
    #[serde(rename = "selected_profile")]
    pub runtime_profile: String,
    #[serde(default)]
    pub global_profile_binding_sha256: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub model: Option<String>,
    pub default_effort: String,
    pub purpose: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub purpose_label: Option<String>,
    pub required_capabilities: Vec<String>,
    pub instructions: String,
    pub execution_lane: String,
    pub required_assurance: String,
    pub native_subagent_policy: String,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub(crate) struct PreparedExternalSpecialist {
    pub view: WorkspaceView,
    pub agent_configuration: AgentConfigurationSnapshot,
    pub global_profile_binding: GlobalProfileBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExternalSpecialistPreparation {
    EnsureServer,
    ObserveOnly,
}

fn verify_external_profile_binding(
    input: &ExternalAgentConfigurationInput,
    binding: &GlobalProfileBinding,
) -> Result<(), MachineError> {
    if let Some(expected) = &input.global_profile_binding_sha256
        && expected != &binding.digest()?
    {
        return Err(MachineError::invalid_argument(
            "global_profile_binding_sha256",
            "the requested global Profile binding has changed",
        ));
    }
    Ok(())
}

pub(crate) fn prepare_external_specialist(
    workspace: Option<&Path>,
    role_ref: &str,
    input: ExternalAgentConfigurationInput,
) -> Result<PreparedExternalSpecialist, MachineError> {
    prepare_external_specialist_with(
        workspace,
        role_ref,
        input,
        ExternalSpecialistPreparation::EnsureServer,
    )
}

pub(crate) fn prepare_external_specialist_read_only(
    workspace: Option<&Path>,
    role_ref: &str,
    input: ExternalAgentConfigurationInput,
) -> Result<PreparedExternalSpecialist, MachineError> {
    prepare_external_specialist_with(
        workspace,
        role_ref,
        input,
        ExternalSpecialistPreparation::ObserveOnly,
    )
}

fn prepare_external_specialist_with(
    workspace: Option<&Path>,
    role_ref: &str,
    input: ExternalAgentConfigurationInput,
    preparation: ExternalSpecialistPreparation,
) -> Result<PreparedExternalSpecialist, MachineError> {
    if input.schema_version != 2
        || input.required_assurance != "best_effort_personal_alpha"
        || input.native_subagent_policy != "enabled"
    {
        return Err(MachineError::invalid_argument(
            "agent_configuration",
            "unsupported Agent Configuration contract",
        ));
    }
    let view = WorkspaceService::system()?.discover(workspace)?;
    if input.runtime_profile.is_empty()
        || input.runtime_profile.len() > 128
        || !input
            .runtime_profile
            .bytes()
            .enumerate()
            .all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
            })
    {
        return Err(MachineError::invalid_argument(
            "selected_profile",
            "selected profile does not match the checked pattern",
        ));
    }
    let home = DolgoraeHome::system()?;
    let resolved = ResolvedGlobalProfile::resolve(&home, &input.runtime_profile)?;
    let (
        global_profile_binding,
        default_model,
        models,
        profile_capabilities,
        observed_efforts,
        live_state,
    ) = match preparation {
        ExternalSpecialistPreparation::EnsureServer => {
            let binding = resolved.prepare(&home)?;
            verify_external_profile_binding(&input, &binding)?;
            let state = crate::profile::ensure_global_server(&binding)?;
            (
                binding,
                state.default_model.clone(),
                state.models.clone(),
                state.capabilities.clone(),
                None,
                Some(state),
            )
        }
        ExternalSpecialistPreparation::ObserveOnly => {
            let observation =
                crate::profile::observe_global_profile(&home, &input.runtime_profile)?;
            if !observation.blockers.is_empty() {
                let blockers = observation
                    .blockers
                    .iter()
                    .map(|blocker| match blocker {
                        crate::profile::ProfileObservationBlocker::ServerUnavailable => {
                            "profile_server_unavailable"
                        }
                        crate::profile::ProfileObservationBlocker::RuntimeIncompatible => {
                            "profile_runtime_incompatible"
                        }
                    })
                    .collect::<Vec<_>>();
                return Err(compatibility_rejected(
                    &input.runtime_profile,
                    "profile_observation",
                    json!("an already-running compatible Profile Server"),
                    json!(blockers),
                    "read-only Specialist Policy validation cannot start or repair a Profile Server",
                ));
            }
            let binding = resolved.bind(observation.snapshot)?;
            verify_external_profile_binding(&input, &binding)?;
            let default_model = observation
                .models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.model_id.clone())
                .ok_or_else(|| {
                    compatibility_rejected(
                        &input.runtime_profile,
                        "default_model",
                        json!("exactly one advertised default model"),
                        Value::Null,
                        "profile model observation has no default model",
                    )
                })?;
            let models = observation
                .models
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            let efforts = observation
                .models
                .into_iter()
                .map(|model| (model.model_id, model.supported_efforts))
                .collect::<BTreeMap<_, _>>();
            (
                binding,
                default_model,
                models,
                observation.capabilities,
                Some(efforts),
                None,
            )
        }
    };
    let model = input.model.unwrap_or(default_model);
    if !models.contains(&model) {
        return Err(compatibility_rejected(
            &input.runtime_profile,
            "model",
            json!(models),
            json!(model),
            "requested model is not advertised by the profile server",
        ));
    }
    let efforts = match observed_efforts {
        Some(catalog) => catalog.get(&model).cloned().ok_or_else(|| {
            compatibility_rejected(
                &input.runtime_profile,
                "model",
                json!(models),
                json!(model),
                "requested model is not present in the observed profile catalog",
            )
        })?,
        None => advertised_efforts(
            live_state.as_ref().expect("ensure-server evidence"),
            &input.runtime_profile,
            &model,
        )?,
    };
    if !efforts.contains(&input.default_effort) {
        return Err(compatibility_rejected(
            &input.runtime_profile,
            "reasoning_effort",
            json!(efforts),
            json!(input.default_effort),
            "requested reasoning effort is not advertised for the model",
        ));
    }
    let purpose_kind = parse_purpose(&input.purpose)?;
    if (purpose_kind == PurposeKind::Other) != input.purpose_label.is_some() {
        return Err(MachineError::invalid_argument(
            "purpose_label",
            "purpose_label is required only when purpose is other",
        ));
    }
    if input
        .purpose_label
        .as_ref()
        .is_some_and(|label| label.is_empty() || label.len() > 256 || label.contains('\0'))
    {
        return Err(MachineError::invalid_argument(
            "purpose_label",
            "purpose label is outside the checked bound",
        ));
    }
    if input.instructions.is_empty()
        || input.instructions.len() > 65_536
        || input.instructions.contains('\0')
    {
        return Err(MachineError::invalid_argument(
            "agent_configuration",
            "instructions must be nonempty, NUL-free, and at most 65536 bytes",
        ));
    }
    if role_ref.is_empty()
        || role_ref.len() > 64
        || !role_ref.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(MachineError::invalid_argument(
            "role_ref",
            "role reference must match ^[a-z0-9][a-z0-9._-]{0,63}$",
        ));
    }
    if input
        .required_capabilities
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != input.required_capabilities.len()
    {
        return Err(MachineError::invalid_argument(
            "required_capabilities",
            "required capabilities must be unique",
        ));
    }
    let mut capabilities = input.required_capabilities;
    capabilities.sort();
    capabilities.dedup();
    if capabilities.len() > 64 {
        return Err(MachineError::invalid_argument(
            "required_capabilities",
            "at most 64 unique capabilities are allowed",
        ));
    }
    for capability in &capabilities {
        if capability.is_empty() || capability.len() > 128 || capability.contains('\0') {
            return Err(MachineError::invalid_argument(
                "required_capabilities",
                "capability name is outside the checked bound",
            ));
        }
        if !matches!(
            profile_capabilities.get(capability),
            Some(crate::profile::ProfileCapabilityState::Supported)
        ) {
            return Err(compatibility_rejected(
                &input.runtime_profile,
                "required_capability",
                json!("supported"),
                json!(capability),
                "required capability is not supported by the profile",
            ));
        }
    }
    let execution_lane = parse_execution_lane(&input.execution_lane)?;
    let instructions = InstructionSnapshot {
        schema: "dolgorae.instructions/v1".to_owned(),
        common_prefix_version: 1,
        mode_prefix_version: 1,
        purpose_prefix_version: 1,
        normalized_byte_length: input.instructions.len() as u64,
        normalized_sha256: sha256_hex(input.instructions.as_bytes()),
    };
    let agent_configuration = AgentConfigurationSnapshot {
        schema_version: 2,
        runtime_profile: input.runtime_profile,
        runtime_profile_snapshot_sha256: global_profile_binding.digest()?,
        model,
        default_effort: input.default_effort,
        purpose: Purpose {
            kind: purpose_kind,
            external_label: input.purpose_label,
        },
        required_capabilities: capabilities,
        role_reference: Some(role_ref.to_owned()),
        normalized_instructions: input.instructions,
        instructions,
        execution_lane,
        required_assurance: Assurance::BestEffortPersonalAlpha,
        native_subagent_policy: "enabled".to_owned(),
    };
    Ok(PreparedExternalSpecialist {
        view,
        agent_configuration,
        global_profile_binding,
    })
}

pub(crate) fn prepare_reviewer(
    workspace: Option<&Path>,
    profile_name: &str,
) -> Result<PreparedReviewer, MachineError> {
    let view = WorkspaceService::system()?.discover(workspace)?;
    let home = DolgoraeHome::system()?;
    let global_profile_binding =
        ResolvedGlobalProfile::resolve(&home, profile_name)?.prepare(&home)?;
    let state = crate::profile::ensure_global_server(&global_profile_binding)?;
    let configuration = &global_profile_binding
        .launch_snapshot
        .process_static_configuration;
    let model = configuration
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&state.default_model)
        .to_owned();
    let efforts = advertised_efforts(&state, profile_name, &model)?;
    let effort = reviewer_effort(
        profile_name,
        configuration.get("model_reasoning_effort"),
        &efforts,
    )?;
    let profile_snapshot = global_profile_binding.launch_snapshot.clone();
    let profile = run_profile_snapshot(&profile_snapshot)?;
    let mut plan = ReviewerRuntimePlan::resolve(
        &profile,
        ReviewerRuntimeRequest {
            runtime_profile: profile_name.to_owned(),
            model: model.clone(),
            effort: effort.clone(),
            required_capabilities: Vec::new(),
        },
    )?;
    plan.agent_configuration.schema_version = 2;
    plan.agent_configuration.runtime_profile_snapshot_sha256 = global_profile_binding.digest()?;
    let state_root = workspace_state_root(&view)?;
    Ok(PreparedReviewer {
        view,
        state_root,
        plan,
        model,
        effort,
        profile_snapshot,
        global_profile_binding,
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

pub(crate) fn interrupt_reviewer_run(
    state_root: &Path,
    run_id: Uuid,
    controller: &Path,
) -> Result<ControlResponseV1, MachineError> {
    let carrier = CredentialCarrier::open_path(controller)?;
    let binding = load_reconciled_controller_binding(state_root, run_id)?;
    authorize_controller(run_id, "run.interrupt", &binding, &carrier)?;
    let carrier = carrier.with_expected_generation(binding.identity.generation);
    // Internal cancellation has no caller-supplied revision. Worker identity,
    // controller authority, admission, and lifecycle checks still apply.
    crate::worker::call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        Some(carrier.raw_fd()),
        |expected| {
            checked_controller_request(
                ControlRequestV1::Interrupt {
                    expected,
                    caller: None,
                },
                Some(&carrier),
            )
        },
    )
    .map_err(|error| error.machine_error(run_id, state_root))
}

#[derive(Default)]
pub struct CoreSemanticService;

/// Transport-neutral allocation inputs; credential descriptors are already open.
pub struct StartRunInput {
    pub workspace: Option<PathBuf>,
    pub expected_workspace_id: Option<String>,
    pub profile: String,
    pub control_mode: ControlMode,
    pub execution_lane: ExecutionLane,
    pub assurance: Assurance,
    pub purpose: Purpose,
    pub parent_ref: Option<ParentReference>,
    pub idempotency_key: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub instructions: String,
    pub carrier: CredentialCarrier,
    pub required_capabilities: Vec<String>,
}

pub struct RunStartReceipt {
    pub snapshot: crate::snapshot::RunSnapshot,
    pub exact_replay: bool,
}

pub enum RunMutationOperation {
    Submit {
        request: TurnControlRequest,
        write: bool,
    },
    Interrupt,
    Pause {
        interrupt: bool,
    },
    Resume,
    Close {
        interrupt: bool,
    },
    Recover,
    Reconcile,
    AcquireWriter,
    ReleaseWriter,
    Respond {
        request_id: String,
        idempotency_key: String,
        response: Value,
    },
}

pub struct RunMutationInput {
    pub workspace: PathBuf,
    pub expected_workspace_id: String,
    pub run_id: Uuid,
    pub expected_state_revision: Option<u64>,
    pub carrier: CredentialCarrier,
    pub operation: RunMutationOperation,
}

pub struct RunMutationReceipt {
    pub snapshot: crate::snapshot::RunSnapshot,
    pub response: Option<ControlResponseV1>,
}

impl CoreSemanticService {
    pub fn list_runs(
        &self,
        workspace: &WorkspaceView,
        controller_id: Option<Uuid>,
    ) -> Result<
        Vec<(
            crate::snapshot::RunSnapshot,
            crate::snapshot::RunObservation,
        )>,
        MachineError,
    > {
        let root = workspace_state_root(workspace)?;
        let directory = root.join("runs");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(internal(error.to_string())),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| internal(error.to_string()))?;
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
            {
                ids.push(id);
            }
        }
        ids.sort();
        let mut result = Vec::new();
        for id in ids {
            let observation = crate::snapshot::RunSnapshot::observe(
                &root,
                id,
                observed_control_socket_epoch(&root, id),
            )?;
            if observation.0.manifest.workspace_id != workspace.workspace_id {
                return Err(internal("Run list crossed a workspace boundary"));
            }
            if controller_id
                .is_none_or(|controller| observation.0.controller.controller_id == controller)
            {
                result.push(observation);
            }
        }
        Ok(result)
    }

    pub fn mutate_run(&self, input: RunMutationInput) -> Result<RunMutationReceipt, MachineError> {
        let view = WorkspaceService::system()?.discover(Some(&input.workspace))?;
        require_workspace_identity(&view, Some(&input.expected_workspace_id))?;
        let state_root = workspace_state_root(&view)?;
        let run_id = input.run_id;
        let (verb, prepared, write) = match input.operation {
            RunMutationOperation::Submit { mut request, write } => {
                request.write = write;
                (
                    RunVerb::Submit,
                    Prepared::Turn {
                        request,
                        timeout_ms: None,
                    },
                    write,
                )
            }
            RunMutationOperation::Interrupt => (RunVerb::Interrupt, Prepared::Plain, false),
            RunMutationOperation::Pause { interrupt } => {
                (RunVerb::Pause, Prepared::Pause { interrupt }, false)
            }
            RunMutationOperation::Resume => (RunVerb::Resume, Prepared::Plain, false),
            RunMutationOperation::Close { interrupt } => {
                (RunVerb::Close, Prepared::Close { interrupt }, false)
            }
            RunMutationOperation::AcquireWriter => (RunVerb::AcquireWrite, Prepared::Plain, false),
            RunMutationOperation::ReleaseWriter => (RunVerb::ReleaseWrite, Prepared::Plain, false),
            RunMutationOperation::Respond {
                request_id,
                idempotency_key,
                response,
            } => (
                RunVerb::Respond,
                Prepared::Respond {
                    request_id: resolve_public_interaction_id(&state_root, run_id, &request_id)?,
                    idempotency_key,
                    response,
                },
                false,
            ),
            operation @ (RunMutationOperation::Recover | RunMutationOperation::Reconcile) => {
                let name = if matches!(operation, RunMutationOperation::Recover) {
                    "run.recover"
                } else {
                    "run.reconcile"
                };
                history_reconciliation_typed(
                    &view,
                    &state_root,
                    run_id,
                    &input.carrier,
                    name,
                    input.expected_state_revision,
                )?;
                return Ok(RunMutationReceipt {
                    snapshot: RunSources::load(
                        &state_root,
                        run_id,
                        observed_control_socket_epoch(&state_root, run_id),
                    )?,
                    response: None,
                });
            }
        };
        let (_, response) = run_control_typed(
            &view,
            &state_root,
            run_id,
            verb,
            Some(&input.carrier),
            prepared,
            write,
            input.expected_state_revision,
        )?;
        let control_socket_epoch = observed_control_socket_epoch(&state_root, run_id);
        let snapshot = match &response {
            Some(response) => RunSources::for_control_response(
                &state_root,
                run_id,
                control_socket_epoch,
                response,
            )?,
            None => RunSources::load(&state_root, run_id, control_socket_epoch)?,
        };
        Ok(RunMutationReceipt { snapshot, response })
    }

    pub fn start_run(&self, input: StartRunInput) -> Result<RunStartReceipt, MachineError> {
        let view =
            WorkspaceService::system()?.discover_for_run_start(input.workspace.as_deref())?;
        start_run_typed(input, view, None, None, None, None)
    }
}

impl SemanticService for CoreSemanticService {
    fn execute(&self, command: &SemanticCommand) -> Result<SemanticResult, MachineError> {
        match command {
            SemanticCommand::RuntimeCapabilities => Ok(SemanticResult::RuntimeCapabilities(
                Box::new(capabilities()),
            )),
            SemanticCommand::Initialize { path, mode } => WorkspaceService::system()?
                .initialize_with_preparation(path.as_deref(), *mode, || {
                    let home = crate::paths::DolgoraeHome::system()?;
                    crate::global_profile::initialize_generation(&home).map(|_| ())
                })
                .map(SemanticResult::Workspace),
            SemanticCommand::WorkspaceInspect { workspace } => WorkspaceService::system()?
                .discover(workspace.as_deref())
                .map(SemanticResult::Workspace),
            SemanticCommand::Run { verb, args } => match verb {
                RunVerb::Start => run_start(args).map(SemanticResult::Run),
                RunVerb::CreateWriteContinuation => {
                    run_create_write_continuation(args).map(SemanticResult::Run)
                }
                RunVerb::Fork => run_fork(args).map(SemanticResult::Run),
                RunVerb::Recover => run_recover(args).map(SemanticResult::Run),
                RunVerb::Reconcile => run_reconcile(args).map(SemanticResult::Run),
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
    run_start_with_context(args, None, None, None, None)
}

fn run_create_write_continuation(args: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let state_root = workspace_state_root(&view)?;
    let source_run_id = parse_uuid_flag(args, "--from")?;
    let source_turn_id = required(args, "--from-turn")?;
    let reason = match required(args, "--reason")?.as_str() {
        "shared-readonly-source" | "shared_readonly_source" => "shared_readonly_source",
        "access-transition-unavailable" | "access_transition_unavailable" => {
            "access_transition_unavailable"
        }
        "access-transition-unverified" | "access_transition_unverified" => {
            "access_transition_unverified"
        }
        _ => {
            return Err(MachineError::invalid_argument(
                "--reason",
                "reason must be shared-readonly-source, access-transition-unavailable, or access-transition-unverified",
            ));
        }
    };
    let runs = RunStore::new(SystemWorkspacePlatform, &state_root);
    let source = runs.load_manifest(source_run_id)?;
    let source_projection = runs.load_state_projection(source_run_id)?;
    if source_projection.active_turn_id.is_some()
        || !source_projection.pending_requests.is_empty()
        || source_projection.latest_turn_id.as_deref() != Some(source_turn_id.as_str())
        || source_projection.thread_id.is_none()
    {
        return Err(MachineError::new(
            "WRITE_CONTINUATION_SOURCE_NOT_TERMINAL",
            "the named source turn is not the current terminal boundary",
            false,
            json!({
                "source_run_id": source_run_id,
                "source_turn_id": source_turn_id,
                "current_turn_id": source_projection.latest_turn_id,
            }),
        ));
    }
    let reason_matches = continuation_reason_matches(
        source.execution_lane,
        source
            .profile_capability_snapshot
            .capabilities
            .get("access_policy_transition"),
        reason,
    );
    if !reason_matches {
        return Err(MachineError::new(
            "WRITE_CONTINUATION_LINEAGE_INVALID",
            "the creation reason does not match the recorded source lane and transition verdict",
            false,
            json!({"source_run_id": source_run_id, "reason": reason}),
        ));
    }

    let source_carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let source_binding = runs.load_controller_binding(source_run_id)?;
    authorize_controller(
        source_run_id,
        "run.create_write_continuation",
        &source_binding,
        &source_carrier,
    )?;
    let destination_carrier =
        carrier_from_options(args, "--new-controller-file", "--new-controller-fd")?;
    let destination_binding = binding_from_carrier(&destination_carrier, 1)?;
    let source_principal = source_binding
        .identity
        .subject_id
        .as_deref()
        .unwrap_or(&source_binding.identity.instance_id);
    let destination_principal = destination_binding
        .identity
        .subject_id
        .as_deref()
        .unwrap_or(&destination_binding.identity.instance_id);
    if source_binding.identity.controller_id == destination_binding.identity.controller_id
        || source_binding.identity.kind != destination_binding.identity.kind
        || source_principal != destination_principal
    {
        return Err(MachineError::new(
            "WRITE_CONTINUATION_CONTROLLER_INVALID",
            "the destination controller must be fresh and represent the same principal",
            false,
            json!({"source_run_id": source_run_id}),
        ));
    }

    let requested_assurance = optional(args, "--required-assurance")
        .map_or(Ok(source.requested_assurance), |value| {
            parse_assurance(&value)
        })?;
    if assurance_rank(requested_assurance) < assurance_rank(source.requested_assurance) {
        return Err(MachineError::new(
            "ASSURANCE_LEVEL_UNAVAILABLE",
            "a write continuation cannot lower the source assurance",
            false,
            json!({
                "requested": requested_assurance.as_str(),
                "source": source.requested_assurance.as_str(),
            }),
        ));
    }
    let mut capabilities = source.required_capabilities.clone();
    capabilities.extend(all(args, "--require-capability"));
    capabilities.sort();
    capabilities.dedup();

    let destination_instructions = read_optional_text_fd(args, "--instructions-fd", 65_536)?;
    let handoff_summary = read_optional_text_fd(args, "--handoff-summary-fd", 65_536)?;
    let mut artifact_refs = all(args, "--artifact-ref")
        .into_iter()
        .map(|value| {
            value.parse::<Uuid>().map_err(|_| {
                MachineError::invalid_argument(
                    "--artifact-ref",
                    "artifact reference must be a UUID",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    artifact_refs.sort();
    artifact_refs.dedup();
    if artifact_refs.len() > 64 {
        return Err(MachineError::invalid_argument(
            "--artifact-ref",
            "at most 64 artifact references are allowed",
        ));
    }
    let instructions = format!(
        "Dolgorae write continuation.\nControl mode: {}.\nPurpose: {}.\n{}{}",
        source.control_mode.as_str(),
        required(args, "--purpose")?,
        handoff_summary
            .as_deref()
            .map_or(String::new(), |summary| format!(
                "Handoff summary:\n{summary}\n"
            )),
        destination_instructions.unwrap_or_default(),
    );
    if instructions.len() > 65_536 {
        return Err(MachineError::invalid_argument(
            "--instructions-fd",
            "composed instructions exceed 65536 bytes",
        ));
    }
    let baseline_text = serde_json::to_string(&source.start_baseline)
        .map_err(|error| internal(error.to_string()))?;
    let baseline =
        canonicalize(&parse(&baseline_text).map_err(|error| internal(error.to_string()))?)
            .map_err(|error| internal(error.to_string()))?;
    let provenance = WriteContinuationProvenance {
        source_run_id,
        source_turn_id,
        source_thread_id: source_projection.thread_id.expect("checked above"),
        creation_reason: reason.to_owned(),
        source_controller_kind: source_binding.identity.kind.as_str().to_owned(),
        destination_controller_kind: destination_binding.identity.kind.as_str().to_owned(),
        handoff_summary_sha256: handoff_summary
            .as_deref()
            .map(|summary| sha256_hex(summary.as_bytes())),
        artifact_refs,
        workspace_baseline_sha256: sha256_hex(&baseline),
        created_at: SystemLedgerClock::default().timestamp(),
    };

    let workspace_path = source
        .canonical_workspace
        .to_path_buf()
        .map_err(|_| internal("source workspace path is not representable"))?;
    let mut start_args = vec![
        OsString::from("--workspace"),
        workspace_path.into_os_string(),
        OsString::from("--profile"),
        OsString::from(&source.profile.profile_name),
        OsString::from("--control-mode"),
        OsString::from(source.control_mode.as_str()),
        OsString::from("--execution-lane"),
        OsString::from("dedicated"),
        OsString::from("--required-assurance"),
        OsString::from(requested_assurance.as_str()),
        OsString::from("--model"),
        OsString::from(optional(args, "--model").unwrap_or_else(|| source.model.clone())),
        OsString::from("--effort"),
        OsString::from(
            optional(args, "--effort").unwrap_or_else(|| source.default_reasoning_effort.clone()),
        ),
        OsString::from("--purpose"),
        OsString::from(required(args, "--purpose")?),
        OsString::from("--instructions"),
        OsString::from(instructions),
        OsString::from("--idempotency-key"),
        OsString::from(required(args, "--idempotency-key")?),
        OsString::from("--controller-fd"),
        OsString::from(destination_carrier.raw_fd().to_string()),
    ];
    if let Some(label) = optional(args, "--purpose-label") {
        start_args.extend([OsString::from("--purpose-label"), OsString::from(label)]);
    }
    for capability in capabilities {
        start_args.extend([
            OsString::from("--require-capability"),
            OsString::from(capability),
        ]);
    }
    run_start_with_context(
        &start_args,
        None,
        Some(&ContinuationStartContext {
            provenance: &provenance,
        }),
        None,
        None,
    )
}

fn run_fork(args: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let state_root = workspace_state_root(&view)?;
    let source_run_id = parse_uuid_flag(args, "--from")?;
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let source = store.load_manifest(source_run_id)?;
    let projection = store.load_state_projection(source_run_id)?;
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let source_binding = store.load_controller_binding(source_run_id)?;
    authorize_controller(source_run_id, "run.fork", &source_binding, &carrier)?;
    if source.workspace_id != view.workspace_id {
        return Err(MachineError::new(
            "RUN_STATE_CONFLICT",
            "a fork cannot cross canonical workspaces",
            false,
            json!({"run_id": source_run_id, "operation": "run.fork"}),
        ));
    }

    let mut fresh = switch(args, "--fresh");
    let (source_thread_id, source_turn_id, boundary) = if fresh {
        if matches!(
            projection.lifecycle,
            RunLifecycle::Running | RunLifecycle::WaitingInteraction
        ) && (projection_identity_verdict(&state_root, source_run_id) != "Unverifiable"
            || !source_control_socket_unreachable(&state_root, source_run_id))
        {
            return Err(run_state_conflict_error(
                source_run_id,
                projection.lifecycle,
                "run.fork",
            ));
        }
        (None, None, None)
    } else {
        if !matches!(
            projection.lifecycle,
            RunLifecycle::Idle
                | RunLifecycle::Paused
                | RunLifecycle::Closed
                | RunLifecycle::OutcomeUnknown
        ) {
            return Err(run_state_conflict_error(
                source_run_id,
                projection.lifecycle,
                "run.fork",
            ));
        }
        prove_source_generation_absent_for_fork(&state_root, source_run_id)?;
        let (terminal, has_confirmed_turns) =
            latest_forkable_terminal(&state_root, source_run_id, &projection)?;
        if terminal.is_none()
            && !has_confirmed_turns
            && projection.lifecycle == RunLifecycle::OutcomeUnknown
        {
            fresh = true;
            (None, None, None)
        } else if let Some(terminal) = terminal {
            let thread_id = projection.thread_id.clone().ok_or_else(|| {
                MachineError::new(
                    "THREAD_NOT_FOUND",
                    "the source run has no confirmed Codex thread",
                    false,
                    json!({"run_id": source_run_id}),
                )
            })?;
            (
                Some(thread_id),
                Some(terminal.turn_id),
                Some(terminal.status),
            )
        } else {
            return Err(MachineError::new(
                "COMPATIBILITY_REJECTED",
                "confirmed history has no profile-approved fork boundary",
                false,
                json!({
                    "profile": source.profile.profile_name,
                    "compatibility_surface": "forkable_turn_statuses",
                    "required": ["completed"],
                    "observed": projection.latest_turn_id,
                }),
            ));
        }
    };
    let provenance = ForkProvenance {
        source_run_id,
        mode: if fresh { "fresh" } else { "history_copy" }.to_owned(),
        source_turn_id,
        source_thread_id,
        last_confirmed_boundary: boundary,
        observed_source_lifecycle: projection.lifecycle.as_str().to_owned(),
        unresolved_turn_id: if fresh {
            projection.active_turn_id.clone().or_else(|| {
                (projection.lifecycle == RunLifecycle::OutcomeUnknown)
                    .then(|| projection.latest_turn_id.clone())
                    .flatten()
            })
        } else {
            None
        },
    };

    let workspace_path = source
        .canonical_workspace
        .to_path_buf()
        .map_err(|_| internal("source workspace path is not representable"))?;
    let mut start_args = vec![
        OsString::from("--workspace"),
        workspace_path.into_os_string(),
        OsString::from("--profile"),
        OsString::from(&source.profile.profile_name),
        OsString::from("--control-mode"),
        OsString::from(source.control_mode.as_str()),
        OsString::from("--execution-lane"),
        OsString::from(source.execution_lane.as_str()),
        OsString::from("--required-assurance"),
        OsString::from(source.requested_assurance.as_str()),
        OsString::from("--model"),
        OsString::from(optional(args, "--model").unwrap_or_else(|| source.model.clone())),
        OsString::from("--effort"),
        OsString::from(&source.default_reasoning_effort),
        OsString::from("--purpose"),
        OsString::from(source.purpose.kind.as_str()),
        OsString::from("--instructions"),
        OsString::from(&source.agent_configuration.normalized_instructions),
        OsString::from("--idempotency-key"),
        OsString::from(bounded_key(args, "--idempotency-key", 256)?),
        OsString::from("--controller-fd"),
        OsString::from(carrier.raw_fd().to_string()),
    ];
    if let Some(label) = &source.purpose.external_label {
        start_args.extend([OsString::from("--purpose-label"), OsString::from(label)]);
    }
    if let Some(parent) = &source.parent_ref {
        start_args.extend([
            OsString::from("--parent-namespace"),
            OsString::from(&parent.namespace),
            OsString::from("--parent-kind"),
            OsString::from(&parent.kind),
            OsString::from("--parent-id"),
            OsString::from(&parent.id),
        ]);
    }
    for capability in &source.required_capabilities {
        start_args.extend([
            OsString::from("--require-capability"),
            OsString::from(capability),
        ]);
    }
    run_start_with_context(
        &start_args,
        None,
        None,
        Some(&ForkStartContext {
            provenance: &provenance,
        }),
        None,
    )
}

fn source_control_socket_unreachable(state_root: &Path, run_id: Uuid) -> bool {
    let Ok(path) =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
    else {
        return false;
    };
    let Ok(record) = crate::worker::read_runtime_record(&path, DarwinSystem.current_uid()) else {
        return false;
    };
    UnixStream::connect(&record.socket_path).is_err()
}

fn latest_forkable_terminal(
    state_root: &Path,
    run_id: Uuid,
    projection: &crate::projection::RunStateProjection,
) -> Result<(Option<crate::turn::TerminalTurn>, bool), MachineError> {
    if projection.ledger_head.sequence == 0 {
        return Ok((None, false));
    }
    let root = state_root.join("runs").join(run_id.to_string());
    let ledger =
        crate::ledger::ObservedLedger::open(&root, run_id, projection.ledger_head.sequence)
            .map_err(|error| {
                audit_integrity(run_id, projection.ledger_head.sequence, &error.to_string())
            })?;
    let terminals = ledger
        .payloads_of_kind(AuditKind::TurnTerminal)
        .map_err(|error| {
            audit_integrity(run_id, projection.ledger_head.sequence, &error.to_string())
        })?;
    let has_confirmed_turns = !terminals.is_empty()
        || !ledger
            .payloads_of_kind(AuditKind::TurnStarted)
            .map_err(|error| {
                audit_integrity(run_id, projection.ledger_head.sequence, &error.to_string())
            })?
            .is_empty();
    for payload in terminals.into_iter().rev() {
        let terminal: crate::turn::TerminalTurn =
            serde_json::from_value(payload).map_err(|error| {
                audit_integrity(run_id, projection.ledger_head.sequence, &error.to_string())
            })?;
        if crate::turn::forkable_status(&terminal.status) {
            return Ok((Some(terminal), true));
        }
    }
    Ok((None, has_confirmed_turns))
}

fn prove_source_generation_absent_for_fork(
    state_root: &Path,
    run_id: Uuid,
) -> Result<(), MachineError> {
    let uid = DarwinSystem.current_uid();
    let path = crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
        .map_err(|error| error.machine_error(run_id, state_root))?;
    if !path.exists() {
        return Ok(());
    }
    let record = crate::worker::read_runtime_record(&path, uid)
        .map_err(|error| error.machine_error(run_id, &path))?;
    crate::worker::prove_worker_generation_absent(&record)
        .map(|_| ())
        .map_err(|_| {
            let verdict = crate::worker::classify_worker_identity(&record);
            MachineError::new(
                "RECOVERY_REQUIRED",
                "the source generation is not proved absent for history-copying fork",
                false,
                json!({
                    "run_id": run_id,
                    "generation": record.identity.run_generation,
                    "identity_verdict": verdict.as_str(),
                    "required_action": "recover_source_generation",
                }),
            )
        })
}

fn run_recover(args: &[OsString]) -> Result<Value, MachineError> {
    run_history_reconciliation(args, "run.recover")
}

fn run_reconcile(args: &[OsString]) -> Result<Value, MachineError> {
    run_history_reconciliation(args, "run.reconcile")
}

fn run_history_reconciliation(
    args: &[OsString],
    operation: &'static str,
) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let state_root = workspace_state_root(&view)?;
    let run_id = positional_run_id(args)?;
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    history_reconciliation_typed(&view, &state_root, run_id, &carrier, operation, None)
}

fn history_reconciliation_typed(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    operation: &'static str,
    expected_state_revision: Option<u64>,
) -> Result<Value, MachineError> {
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    let projection = store.load_state_projection(run_id)?;
    let runtime_path =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    let control_recovery =
        crate::worker::read_runtime_record(&runtime_path, DarwinSystem.current_uid())
            .is_ok_and(|record| record.control_recovery_required);
    if !control_recovery
        && durable_pending_admission(state_root, run_id)?.is_none()
        && !matches!(
            projection.lifecycle,
            RunLifecycle::Running
                | RunLifecycle::WaitingInteraction
                | RunLifecycle::ReconciliationRequired
                | RunLifecycle::OutcomeUnknown
        )
    {
        return Err(run_state_conflict_error(
            run_id,
            projection.lifecycle,
            operation,
        ));
    }
    let binding = load_reconciled_controller_binding(state_root, run_id)?;
    authorize_controller(run_id, operation, &binding, carrier)?;
    let admission_id = match begin_prepared_mutation(
        view,
        state_root,
        run_id,
        RunVerb::Reconcile,
        carrier,
        &Prepared::Plain,
        expected_state_revision,
    )? {
        PreparedAdmission::Admitted(id) => id,
        PreparedAdmission::Replayed(_) => {
            return Err(internal(
                "reconciliation cannot replay an acceptance receipt",
            ));
        }
    };
    if control_recovery {
        crate::worker::recover_control_runtime(&runtime_path, DarwinSystem.current_uid())
            .map_err(|error| error.machine_error(run_id, &runtime_path))?;
        if matches!(
            projection.lifecycle,
            RunLifecycle::Idle
                | RunLifecycle::Paused
                | RunLifecycle::Closed
                | RunLifecycle::StartFailed
        ) {
            let writer = crate::writer::WriterStore::new(
                state_root,
                &view.workspace_id,
                DarwinSystem.current_uid(),
            );
            writer.observe_locked(|_| {
                let mut ledger = crate::ledger::Ledger::open(run_root(state_root, run_id), run_id)
                    .map_err(|error| internal(error.to_string()))?;
                complete_mutation_admission(&mut ledger, admission_id)
            })??;
            let sources = RunSources::load(state_root, run_id, 0)?;
            let terminal = durable_last_terminal(state_root, run_id, &sources)?;
            return Ok(sources.run_value("Absent", terminal));
        }
    }
    let control_socket_epoch = ensure_run_worker(view, state_root, run_id)?;
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    let response = crate::worker::call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        Some(carrier.raw_fd()),
        |expected| {
            let request = ControlRequestV1::Reconcile {
                expected,
                caller: None,
            };
            ControlRequestV1::AdmittedMutation {
                admission_id,
                request: Box::new(request),
            }
        },
    )
    .map_err(|error| error.machine_error(run_id, &runtime_record))?;
    match control_response_value(
        state_root,
        run_id,
        control_socket_epoch,
        RunVerb::Reconcile,
        response,
    )? {
        SemanticResult::Run(value) => Ok(value),
        _ => Err(internal("reconciliation returned a non-run result")),
    }
}

fn assurance_rank(assurance: Assurance) -> u8 {
    match assurance {
        Assurance::BestEffortPersonalAlpha => 0,
        Assurance::VerifiedThreadScopedControl => 1,
        Assurance::StrongProcessContainment => 2,
    }
}

fn read_optional_text_fd(
    args: &[OsString],
    flag: &str,
    maximum: u64,
) -> Result<Option<String>, MachineError> {
    let Some(raw) = optional(args, flag) else {
        return Ok(None);
    };
    let fd = raw
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| MachineError::invalid_argument(flag, "descriptor must be nonnegative"))?;
    let mut file = std::fs::File::open(format!("/dev/fd/{fd}"))
        .map_err(|_| MachineError::invalid_argument(flag, "descriptor is unreadable"))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument(flag, "descriptor is unreadable"))?;
    if bytes.len() as u64 > maximum {
        return Err(MachineError::invalid_argument(
            flag,
            "descriptor content is too large",
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| MachineError::invalid_argument(flag, "descriptor must contain UTF-8"))
}

pub(crate) struct ReviewerStartContext<'a> {
    pub reserved_run_id: Uuid,
    pub aggregate_binding: &'a AggregateBinding,
    pub plan: &'a ReviewerRuntimePlan,
    pub review_cwd: Option<&'a Path>,
    pub global_profile_binding: &'a GlobalProfileBinding,
}

pub(crate) fn start_reviewer_run(
    args: &[OsString],
    context: ReviewerStartContext<'_>,
) -> Result<Value, MachineError> {
    run_start_with_context(args, Some(&context), None, None, None)
}

pub(crate) struct ExternalSpecialistStartContext<'a> {
    pub reserved_run_id: Uuid,
    pub aggregate_binding: &'a AggregateBinding,
    pub agent_configuration: &'a AgentConfigurationSnapshot,
    pub launch_cwd: Option<&'a Path>,
    pub sandbox: &'a str,
    pub global_profile_binding: &'a GlobalProfileBinding,
}

pub(crate) fn start_external_specialist_run(
    args: &[OsString],
    context: ExternalSpecialistStartContext<'_>,
) -> Result<Value, MachineError> {
    run_start_with_context(args, None, None, None, Some(&context))
}

fn validate_external_specialist_manifest(
    manifest: &RunManifest,
    configuration: &AgentConfigurationSnapshot,
) -> Result<(), MachineError> {
    let binding = manifest.aggregate_binding.as_ref().ok_or_else(|| {
        MachineError::invalid_argument(
            "aggregate_binding",
            "an external Specialist requires an aggregate binding",
        )
    })?;
    if manifest.control_mode != ControlMode::ManagedAgent
        || binding.aggregate_kind != crate::domain::AggregateKind::ExternalSpecialistEngagement
        || binding.member_kind != crate::run::AggregateMemberKind::Specialist
        || manifest.profile.profile_name != configuration.runtime_profile
        || manifest.model != configuration.model
        || manifest.default_reasoning_effort != configuration.default_effort
        || manifest.execution_lane != configuration.execution_lane
        || manifest.requested_assurance != configuration.required_assurance
        || manifest.purpose != configuration.purpose
        || manifest.agent_configuration != *configuration
    {
        return Err(MachineError::invalid_argument(
            "agent_configuration",
            "external Specialist Run does not match its immutable configuration",
        ));
    }
    Ok(())
}

fn continuation_reason_matches(
    lane: ExecutionLane,
    verdict: Option<&crate::run::CapabilityState>,
    reason: &str,
) -> bool {
    match reason {
        "shared_readonly_source" => lane == ExecutionLane::SharedReadonly,
        "access_transition_unavailable" => {
            lane == ExecutionLane::Dedicated
                && verdict == Some(&crate::run::CapabilityState::Unavailable)
        }
        "access_transition_unverified" => {
            lane == ExecutionLane::Dedicated
                && verdict == Some(&crate::run::CapabilityState::Unverified)
        }
        _ => false,
    }
}

fn continuation_request_identity(
    provenance: &WriteContinuationProvenance,
) -> Result<String, MachineError> {
    let mut identity =
        serde_json::to_value(provenance).map_err(|error| internal(error.to_string()))?;
    identity
        .as_object_mut()
        .ok_or_else(|| internal("continuation provenance must be an object"))?
        .remove("created_at");
    serde_json::to_string(&identity).map_err(|error| internal(error.to_string()))
}

struct ContinuationStartContext<'a> {
    provenance: &'a WriteContinuationProvenance,
}

struct ForkStartContext<'a> {
    provenance: &'a ForkProvenance,
}

fn run_start_with_context(
    args: &[OsString],
    reviewer: Option<&ReviewerStartContext<'_>>,
    continuation: Option<&ContinuationStartContext<'_>>,
    fork: Option<&ForkStartContext<'_>>,
    external: Option<&ExternalSpecialistStartContext<'_>>,
) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    // Preserve the Machine CLI's workspace-first refusal before parsing the
    // operation-specific configuration and credential carrier.
    let view = WorkspaceService::system()?.discover_for_run_start(workspace.as_deref())?;
    let control_mode = parse_control_mode(&required(args, "--control-mode")?)?;
    let input = StartRunInput {
        workspace,
        expected_workspace_id: None,
        profile: required(args, "--profile")?,
        control_mode,
        execution_lane: parse_execution_lane(&required(args, "--execution-lane")?)?,
        assurance: parse_assurance(&required(args, "--required-assurance")?)?,
        purpose: Purpose {
            kind: parse_purpose(&required(args, "--purpose")?)?,
            external_label: bounded_option(args, "--purpose-label", 256)?,
        },
        parent_ref: parent_reference(args, control_mode)?,
        idempotency_key: bounded_key(args, "--idempotency-key", 256)?,
        model: optional(args, "--model"),
        effort: optional(args, "--effort"),
        instructions: instructions_from(args)?,
        carrier: carrier_from_options(args, "--controller-file", "--controller-fd")?,
        required_capabilities: required_capabilities(args),
    };
    let receipt = start_run_typed(input, view, reviewer, continuation, fork, external)?;
    let state_root = workspace_state_root_from_snapshot(&receipt.snapshot)?;
    let verdict = projection_identity_verdict(&state_root, receipt.snapshot.manifest.run_id);
    let terminal = durable_last_terminal(
        &state_root,
        receipt.snapshot.manifest.run_id,
        &receipt.snapshot,
    )?;
    Ok(receipt.snapshot.run_value(verdict, terminal))
}

fn workspace_state_root_from_snapshot(
    snapshot: &crate::snapshot::RunSnapshot,
) -> Result<PathBuf, MachineError> {
    Ok(DolgoraeHome::system()?.workspace_root(&snapshot.manifest.workspace_id))
}

fn start_run_typed(
    input: StartRunInput,
    view: WorkspaceView,
    reviewer: Option<&ReviewerStartContext<'_>>,
    continuation: Option<&ContinuationStartContext<'_>>,
    fork: Option<&ForkStartContext<'_>>,
    external: Option<&ExternalSpecialistStartContext<'_>>,
) -> Result<RunStartReceipt, MachineError> {
    let StartRunInput {
        workspace: _,
        expected_workspace_id,
        profile: profile_name,
        control_mode,
        execution_lane,
        assurance,
        purpose,
        parent_ref,
        idempotency_key,
        model,
        effort,
        instructions: instructions_text,
        carrier,
        mut required_capabilities,
    } = input;
    require_workspace_identity(&view, expected_workspace_id.as_deref())?;
    if idempotency_key.is_empty() || idempotency_key.len() > 256 {
        return Err(MachineError::invalid_argument(
            "idempotency_key",
            "must contain 1 to 256 bytes",
        ));
    }
    required_capabilities.sort();
    required_capabilities.dedup();
    let home = DolgoraeHome::system()?;
    let global_profile_binding = if let Some(context) = reviewer {
        context.global_profile_binding.clone()
    } else if let Some(context) = external {
        context.global_profile_binding.clone()
    } else {
        ResolvedGlobalProfile::resolve(&home, &profile_name)?.prepare(&home)?
    };
    if global_profile_binding.selected_name != profile_name {
        return Err(MachineError::invalid_argument(
            "--profile",
            "the selected global Profile differs from the prepared consumer binding",
        ));
    }
    let state = crate::profile::ensure_global_server(&global_profile_binding)?;
    let model = model.unwrap_or_else(|| state.default_model.clone());
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
    let effort = effort.unwrap_or_else(|| default_effort(&efforts));
    if !efforts.contains(&effort) {
        return Err(compatibility_rejected(
            &profile_name,
            "reasoning_effort",
            json!(efforts),
            json!(effort),
            "requested reasoning effort is not advertised for this model",
        ));
    }
    if instructions_text.is_empty() || instructions_text.len() > 65_536 {
        return Err(MachineError::invalid_argument(
            "--instructions",
            "a Run pins nonempty instructions of at most 65536 bytes",
        ));
    }
    let presented_controller = presented_controller_from_carrier(&carrier, 1)?;
    let binding = presented_controller.binding;
    let state_root = workspace_state_root(&view)?;
    let orchestration_policy = if let Some(launch) = presented_controller.orchestration_launch {
        if reviewer.is_some()
            || external.is_some()
            || continuation.is_some()
            || fork.is_some()
            || control_mode != ControlMode::DirectInteractive
            || parent_ref.is_some()
            || !matches!(
                binding.identity.kind,
                crate::domain::ControllerKind::HumanCli
                    | crate::domain::ControllerKind::InteractiveClient
            )
        {
            return Err(MachineError::invalid_argument(
                "orchestration_launch",
                "orchestration launch requires a parentless direct-interactive public Run",
            ));
        }
        let prior = StartReservationStore::new(SystemWorkspacePlatform, &state_root)
            .load_operation("run-start", "start_run", &idempotency_key)?;
        let prior_run_id = prior.as_ref().map(|reservation| reservation.run_id);
        let existing = if let Some(run_id) = prior_run_id {
            let store = crate::orchestration::OrchestrationStore::open(&state_root)?;
            match store.session(run_id) {
                Ok(session) => Some(session.specialist_policy),
                Err(error) if error.code == "RUN_NOT_FOUND" => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let policy = match existing {
            Some(policy) => policy,
            None => {
                let workspace_path = view.canonical_path.to_path_buf()?;
                crate::specialist_policy::SpecialistPolicyRegistry::discover(Some(&workspace_path))?
                    .resolve_for_session(&launch.specialist_policy_name)?
            }
        };
        if policy.policy_name != launch.specialist_policy_name {
            return Err(crate::run::idempotency_conflict(
                prior_run_id.unwrap_or_else(Uuid::nil),
                &policy.policy_name,
                &launch.specialist_policy_name,
            ));
        }
        Some(policy)
    } else {
        None
    };

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

    let profile = run_profile_snapshot(&global_profile_binding.launch_snapshot)?;
    // docs/specs/README.md: "Run allocation reserves its key before publishing a Run."
    // The normalization is decided here, before any Run identity exists, so a
    // retried allocation resolves to the same digest and therefore the same
    // Run rather than to a second one.
    let mut normalized_identity_sha256 = start_normalized_digest(
        &view,
        &profile,
        &global_profile_binding,
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
    if let Some(policy) = &orchestration_policy {
        normalized_identity_sha256 = sha256_hex(
            format!(
                "orchestrated-session-v1\0{normalized_identity_sha256}\0{}",
                policy.digest()?
            )
            .as_bytes(),
        );
    }
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
    if let Some(context) = external {
        normalized_identity_sha256 = sha256_hex(
            format!(
                "external-specialist-v1\0{normalized_identity_sha256}\0{}\0{}",
                serde_json::to_string(context.aggregate_binding)
                    .map_err(|error| internal(error.to_string()))?,
                agent_configuration_digest(context.agent_configuration).map_err(internal)?
            )
            .as_bytes(),
        );
    }
    if let Some(context) = continuation {
        normalized_identity_sha256 = sha256_hex(
            format!(
                "write-continuation-v1\0{normalized_identity_sha256}\0{}",
                continuation_request_identity(context.provenance)?
            )
            .as_bytes(),
        );
    }
    if let Some(context) = fork {
        normalized_identity_sha256 = sha256_hex(
            format!(
                "fork-v1\0{normalized_identity_sha256}\0{}",
                serde_json::to_string(context.provenance)
                    .map_err(|error| internal(error.to_string()))?
            )
            .as_bytes(),
        );
    }
    let reservations = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    let (reservation_directory, reservation_operation) = if fork.is_some() {
        ("run-fork", "fork_run")
    } else {
        ("run-start", "start_run")
    };
    let _allocation = reservations.lock_allocation(reservation_directory, &idempotency_key)?;
    let prior_reservation = reservations.load_operation(
        reservation_directory,
        reservation_operation,
        &idempotency_key,
    )?;
    let reservation = match prior_reservation {
        Some(held) => held,
        None => reservations.reserve(&StartReservation {
            schema_version: 1,
            operation: reservation_operation.to_owned(),
            idempotency_key: idempotency_key.clone(),
            normalized_identity_sha256: normalized_identity_sha256.clone(),
            run_id: reviewer.map_or_else(
                || external.map_or_else(Uuid::now_v7, |context| context.reserved_run_id),
                |context| context.reserved_run_id,
            ),
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
    if let Some(context) = external
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
    let mut orchestration = if let Some(policy) = &orchestration_policy {
        let mut orchestration = crate::orchestration::OrchestrationStore::open(&state_root)?;
        orchestration.prepare_session(
            &view.workspace_id,
            run_id,
            &idempotency_key,
            &normalized_identity_sha256,
            policy,
        )?;
        Some(orchestration)
    } else {
        None
    };
    // The identical retry returns the original Run.  Reaching a published Run
    // through its own reservation is exactly the response-loss reconciliation
    // docs/specs/README.md describes, so nothing is allocated, published, or started again
    // — and because nothing here reached that Run's worker, the verdict is
    // derived from what durable state proves rather than asserted as `Match`.
    if let Ok(manifest) = store.load_manifest(run_id) {
        if let Some(orchestration) = orchestration.as_mut() {
            let session = orchestration.session(run_id)?;
            let binding_matches = manifest.aggregate_binding.as_ref().is_some_and(|binding| {
                binding.aggregate_kind == AggregateKind::OrchestratedSession
                    && binding.aggregate_id == run_id
                    && binding.member_kind == crate::run::AggregateMemberKind::Primary
                    && binding.operation_id == session.bootstrap_operation_id
                    && binding.policy_sha256.as_deref()
                        == Some(session.specialist_policy_sha256.as_str())
            });
            if !binding_matches {
                return Err(MachineError::new(
                    "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                    "published Primary Run disagrees with its Orchestrated Session",
                    false,
                    json!({"run_id":run_id,"session_id":run_id}),
                ));
            }
            if matches!(session.status.as_str(), "creating" | "recovering") {
                orchestration.finish_primary_publication(run_id, Ok(()))?;
            }
        }
        return Ok(RunStartReceipt {
            snapshot: RunSources::load(
                &state_root,
                run_id,
                observed_control_socket_epoch(&state_root, run_id),
            )?,
            exact_replay: true,
        });
    }

    let _process_startup = ProcessRunStartup::acquire(&state_root, run_id)?;
    crate::profile::admit_global_run(&global_profile_binding, &state, &view.workspace_id, run_id)?;
    let mut worker_started = false;
    let admitted_result = (|| -> Result<Value, MachineError> {
        let start_baseline = WorkspaceService::system()?.capture_run_baseline(&view)?;
        let mut manifest = build_manifest(
            run_id,
            &view,
            &state,
            profile,
            global_profile_binding.clone(),
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
        if let Some(context) = external {
            manifest.agent_configuration = context.agent_configuration.clone();
            manifest.aggregate_binding = Some(context.aggregate_binding.clone());
            validate_external_specialist_manifest(&manifest, context.agent_configuration)?;
        }
        if let (Some(policy), Some(orchestration)) =
            (orchestration_policy.as_ref(), orchestration.as_mut())
        {
            let session = orchestration.session(run_id)?;
            manifest.aggregate_binding = Some(AggregateBinding {
                aggregate_kind: AggregateKind::OrchestratedSession,
                aggregate_id: run_id,
                member_kind: crate::run::AggregateMemberKind::Primary,
                operation_id: session.bootstrap_operation_id,
                policy_sha256: Some(policy.digest()?),
                role_reference: None,
                role_snapshot_sha256: None,
                agent_configuration_sha256: None,
            });
            orchestration.mark_primary_intent(run_id)?;
        }
        if let Some(context) = continuation {
            manifest.write_continuation_provenance = Some(context.provenance.clone());
        }
        if let Some(context) = fork {
            manifest.fork_provenance = Some(context.provenance.clone());
        }
        let directory = store.publish(&manifest)?;

        crate::writer::WriterStore::new(
            &state_root,
            &manifest.workspace_id,
            DarwinSystem.current_uid(),
        )
        .observe_locked(|writer| -> Result<(), MachineError> {
            let mut ledger = ConformantLedger::open_for_bootstrap(&directory.root, run_id)
                .map_err(|error| internal(error.to_string()))?;
            ledger.with_event_context(
                bootstrap_event_context(&manifest, writer),
                |ledger| -> Result<(), MachineError> {
                    ledger
                        .bootstrap(&BootstrapRequest {
                            timestamp: SystemLedgerClock::default().timestamp(),
                            workspace_id: view.workspace_id.clone(),
                            intent: IdempotencyIntent {
                                schema_version: 1,
                                operation: if fork.is_some() {
                                    IdempotencyOperation::ForkRun
                                } else {
                                    IdempotencyOperation::StartRun
                                },
                                idempotency_key,
                                normalized_identity_sha256,
                                run_id,
                            },
                            record_kind: BootstrapRecordKind::RunCreated,
                            initial_access: Access::Read.as_str().to_owned(),
                            default_effort: Some(effort.clone()),
                        })
                        .map_err(|error| internal(error.to_string()))?;
                    if reviewer.is_none() {
                        ledger
                            .mark_threadless_ready(&SystemLedgerClock::default().timestamp())
                            .map_err(|error| internal(error.to_string()))?;
                    }
                    Ok(())
                },
            )
        })??;

        // Membership is part of publishing the Run's connection authority, not a
        // later best-effort side effect.  The profile lifecycle must be able to
        // refuse stop/restart before the worker can attach to this server.
        crate::profile::reattach_global_run(
            &global_profile_binding,
            &state,
            &view.workspace_id,
            run_id,
            crate::global_runtime::GlobalMembershipFacts {
                controller_id: Some(manifest.controller.identity.controller_id),
                worker_generation: reviewer.is_some().then_some(1),
                thread_id: manifest.thread_id.clone(),
                connection_id: None,
                lifecycle: if reviewer.is_none() {
                    "threadless_ready"
                } else {
                    "worker_starting"
                }
                .to_owned(),
                writer: manifest.initial_access.as_str() == "write",
                observed_epoch: Some(state.server_epoch),
                runtime_locator: Some(directory.root.to_string_lossy().into_owned()),
            },
        )?;

        if let Some(orchestration) = orchestration.as_mut() {
            orchestration.finish_primary_publication(run_id, Ok(()))?;
        }

        // Public Run allocation is deliberately threadless and process-free. The
        // first turn (or an explicit recovery/resume operation) wins the startup
        // election and constructs the connection from the immutable manifest.
        // Internal reviewer allocation retains its already-selected isolated cwd
        // and starts immediately because that path is not part of the public Run
        // manifest.
        if reviewer.is_none() {
            return run_object(&state_root, run_id, 0, "Absent");
        }

        let session = WorkerSessionBootstrap {
            app_server_socket: PathBuf::from(&state.socket_path),
            canonical_codex_home: state.snapshot.canonical_codex_home.clone(),
            server_key: state.snapshot.server_key.clone(),
            server_epoch: state.server_epoch,
            controller_id: manifest.controller.identity.controller_id,
            control_mode: manifest.control_mode.as_str().to_owned(),
            fixed_model: model.clone(),
            default_effort: effort.clone(),
            supported_efforts: efforts,
            cwd: match reviewer
                .and_then(|context| context.review_cwd)
                .or_else(|| external.and_then(|context| context.launch_cwd))
            {
                Some(path) => path.to_path_buf(),
                None => view
                    .canonical_path
                    .to_path_buf()
                    .map_err(|_| internal("workspace path is not representable"))?,
            },
            developer_instructions: instructions_text,
            sandbox: external
                .map_or("read-only", |context| context.sandbox)
                .to_owned(),
            approval_policy: "never".to_owned(),
            safety_policy: reviewer.map_or(crate::turn::SessionSafetyPolicy::Standard, |context| {
                context.plan.safety_policy
            }),
            artifact_root: directory.root.join("artifacts"),
            attach: SessionAttach::Start,
            transport_timeout_seconds: 900,
            dedicated_server: None,
        };
        let control_socket_epoch = 1;
        let startup_range = fenced_worker_startup_range(
            &global_profile_binding,
            &state,
            &view.workspace_id,
            &state_root,
            run_id,
            1,
        )?;
        start_worker(
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
            startup_range,
        )?;
        worker_started = true;
        // The Run's own record is the answer, not a summary of what start did:
        // SPEC-006 has every lifecycle command return the resulting `run`.
        run_object(&state_root, run_id, control_socket_epoch, "Match")
    })();
    match admitted_result {
        Ok(_) => Ok(RunStartReceipt {
            snapshot: RunSources::load(
                &state_root,
                run_id,
                observed_control_socket_epoch(&state_root, run_id),
            )?,
            exact_replay: false,
        }),
        Err(error) => {
            if let Some(orchestration) = orchestration.as_mut() {
                let publication = if store.load_manifest(run_id).is_ok() {
                    Err(crate::orchestration::AdapterFailure::Unknown)
                } else {
                    Err(crate::orchestration::AdapterFailure::Rejected(
                        error.code.clone(),
                    ))
                };
                let _ = orchestration.finish_primary_publication(run_id, publication);
            }
            if !worker_started {
                let _ = GlobalMembershipStore::new(
                    &home,
                    &global_profile_binding.selected_name,
                    &global_profile_binding.server_key,
                )
                .and_then(|store| {
                    store.record(&view.workspace_id, run_id, MembershipDisposition::Released)
                });
            }
            Err(error)
        }
    }
}

/// Drive one Run verb through the worker's control socket.
fn run_control(verb: RunVerb, args: &[OsString]) -> Result<SemanticResult, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let run_id = positional_run_id(args)?;
    let state_root = workspace_state_root(&view)?;
    let carrier = if verb.mutates() {
        Some(carrier_from_options(
            args,
            "--controller-file",
            "--controller-fd",
        )?)
    } else {
        None
    };
    let uid = DarwinSystem.current_uid();
    let prepared = match verb {
        RunVerb::Start
        | RunVerb::CreateWriteContinuation
        | RunVerb::Fork
        | RunVerb::Recover
        | RunVerb::Reconcile => {
            return Err(internal("run allocation is composed separately"));
        }
        RunVerb::Status | RunVerb::Interrupt | RunVerb::Resume => Prepared::Plain,
        RunVerb::Pending => Prepared::Plain,
        RunVerb::Pause => Prepared::Pause {
            interrupt: switch(args, "--interrupt"),
        },
        RunVerb::AcquireWrite | RunVerb::ReleaseWrite => Prepared::Plain,
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
            let manifest =
                RunStore::new(SystemWorkspacePlatform, &state_root).load_manifest(run_id)?;
            if manifest.execution_lane == ExecutionLane::SharedReadonly {
                return Err(write_forbidden(&state_root, run_id));
            }
            let request = turn_request(args)?;
            let timeout_ms = caller_timeout_ms(args)?;
            Prepared::Turn {
                request,
                timeout_ms,
            }
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
            request_id: resolve_public_interaction_id(
                &state_root,
                run_id,
                &required(args, "--request-id")?,
            )?,
            idempotency_key: required(args, "--idempotency-key")?,
            response: response_body(args)?,
        },
    };
    run_control_typed(
        &view,
        &state_root,
        run_id,
        verb,
        carrier.as_ref(),
        prepared,
        switch(args, "--write"),
        None,
    )
    .map(|(result, _)| result)
}

fn checked_controller_request(
    request: ControlRequestV1,
    carrier: Option<&CredentialCarrier>,
) -> ControlRequestV1 {
    match carrier.and_then(CredentialCarrier::expected_generation) {
        Some(expected_controller_generation) => ControlRequestV1::ControllerChecked {
            expected_controller_generation,
            request: Box::new(request),
        },
        None => request,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_control_typed(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    verb: RunVerb,
    carrier: Option<&CredentialCarrier>,
    mut prepared: Prepared,
    write: bool,
    expected_state_revision: Option<u64>,
) -> Result<(SemanticResult, Option<ControlResponseV1>), MachineError> {
    // Authority is checked against the Run's own published binding before the
    // control socket is touched, so an unauthorised caller never occupies a
    // worker thread.  This is only an early rejection: the same already-open
    // descriptor travels to the worker with `SCM_RIGHTS`, and the worker's
    // revalidation under the Run mutation lock is the authoritative one.
    if let Some(carrier) = carrier {
        let binding = load_reconciled_controller_binding(state_root, run_id)?;
        authorize_controller(run_id, verb.operation_name(), &binding, carrier)?;
    }
    if let Prepared::Turn { request, .. } = &mut prepared {
        crate::worker::capture_turn_request_identity(state_root, run_id, request)?;
    }
    if verb == RunVerb::Submit
        && let Prepared::Turn { request, .. } = &prepared
        && let Some(response) = crate::worker::durable_submit_replay(state_root, run_id, request)?
    {
        let value = control_response_value(
            state_root,
            run_id,
            observed_control_socket_epoch(state_root, run_id),
            verb,
            response.clone(),
        )?;
        return Ok((value, Some(response)));
    }
    let expected_state_revision = if verb.mutates() && verb != RunVerb::Respond {
        Some(expected_state_revision.unwrap_or(durable_head(state_root, run_id)?))
    } else {
        None
    };
    if verb == RunVerb::AcquireWrite {
        let carrier = carrier.as_ref().expect("mutating verb has a carrier");
        acquire_writer_authorized(
            view,
            state_root,
            run_id,
            carrier,
            false,
            None,
            expected_state_revision,
            None,
        )?;
        return run_object(
            state_root,
            run_id,
            observed_control_socket_epoch(state_root, run_id),
            projection_identity_verdict(state_root, run_id),
        )
        .map(|value| (SemanticResult::Run(value), None));
    }
    if verb == RunVerb::ReleaseWrite {
        let carrier = carrier.as_ref().expect("mutating verb has a carrier");
        release_writer_authorized(
            view,
            state_root,
            run_id,
            carrier,
            None,
            expected_state_revision,
        )?;
        return run_object(
            state_root,
            run_id,
            observed_control_socket_epoch(state_root, run_id),
            projection_identity_verdict(state_root, run_id),
        )
        .map(|value| (SemanticResult::Run(value), None));
    }
    if verb == RunVerb::Pending {
        return interaction_pending_at(state_root, run_id)
            .map(|value| (SemanticResult::Run(value), None));
    }
    if verb == RunVerb::Resume {
        let projection =
            RunStore::new(SystemWorkspacePlatform, state_root).load_state_projection(run_id)?;
        if !resume_lifecycle_allowed(projection.lifecycle) {
            return Err(run_state_conflict_error(
                run_id,
                projection.lifecycle,
                "run.resume",
            ));
        }
    }
    let uid = DarwinSystem.current_uid();
    // Every option is parsed before the socket is touched, so a malformed
    // request never reaches the worker and a worker thread is never held open
    // waiting for a caller that will fail anyway.
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    if matches!(verb, RunVerb::Pause | RunVerb::Close)
        && let Some(result) = offline_lifecycle_transition(
            state_root,
            run_id,
            verb,
            &runtime_record,
            carrier,
            expected_state_revision,
        )?
    {
        if verb == RunVerb::Close {
            let manifest =
                RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run_id)?;
            let _ = release_global_run_membership(&manifest, run_id);
        }
        return Ok((result, None));
    }
    if write
        && matches!(verb, RunVerb::Send | RunVerb::Submit)
        && RunStore::new(SystemWorkspacePlatform, state_root)
            .load_manifest(run_id)?
            .execution_lane
            == ExecutionLane::SharedReadonly
    {
        return Err(write_forbidden(state_root, run_id));
    }
    let admission_id = if matches!(
        verb,
        RunVerb::Send | RunVerb::Submit | RunVerb::Resume | RunVerb::Pause | RunVerb::Close
    ) {
        match begin_prepared_mutation(
            view,
            state_root,
            run_id,
            verb,
            carrier.expect("mutation carrier"),
            &prepared,
            expected_state_revision,
        )? {
            PreparedAdmission::Admitted(id) => Some(id),
            PreparedAdmission::Replayed(response) => {
                let response = *response;
                let value = control_response_value(
                    state_root,
                    run_id,
                    observed_control_socket_epoch(state_root, run_id),
                    verb,
                    response.clone(),
                )?;
                return Ok((value, Some(response)));
            }
        }
    } else {
        None
    };
    if write && matches!(verb, RunVerb::Send | RunVerb::Submit) {
        acquire_writer_authorized(
            view,
            state_root,
            run_id,
            carrier.expect("mutation carrier"),
            true,
            None,
            None,
            admission_id,
        )?;
    }
    if matches!(verb, RunVerb::Send | RunVerb::Submit | RunVerb::Resume) {
        ensure_run_worker(view, state_root, run_id)?;
    }
    let response = crate::worker::call_run_worker(
        state_root,
        run_id,
        uid,
        carrier.map(CredentialCarrier::raw_fd),
        |expected| {
            let request = prepared.into_request(verb, expected);
            let request = if let Some(admission_id) = admission_id {
                ControlRequestV1::AdmittedMutation {
                    admission_id,
                    request: Box::new(request),
                }
            } else {
                match expected_state_revision {
                    Some(expected_state_revision) if verb.mutates() => {
                        ControlRequestV1::CheckedMutation {
                            expected_state_revision,
                            request: Box::new(request),
                        }
                    }
                    _ => request,
                }
            };
            checked_controller_request(request, carrier)
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
            let sources = RunSources::load(state_root, run_id, 1)?;
            let last_terminal = durable_last_terminal(state_root, run_id, &sources)?;
            return Ok((
                SemanticResult::Run(sources.run_value(verdict, last_terminal)),
                None,
            ));
        }
        Err(error) => return Err(error.machine_error(run_id, &runtime_record)),
    };
    let response = if matches!(verb, RunVerb::Pause | RunVerb::Close)
        && let ControlResponseV1::Interrupted { turn_id, .. } = &response
    {
        let waited = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
            ControlRequestV1::Wait {
                expected,
                caller: None,
                turn_id: turn_id.clone(),
                timeout_ms: Some(5_000),
            }
        })
        .map_err(|error| error.machine_error(run_id, &runtime_record))?;
        if matches!(waited, ControlResponseV1::Terminal { .. }) {
            crate::worker::call_run_worker(
                state_root,
                run_id,
                uid,
                carrier.map(CredentialCarrier::raw_fd),
                |expected| {
                    let request = match verb {
                        RunVerb::Pause => ControlRequestV1::Pause {
                            expected,
                            caller: None,
                            interrupt: false,
                        },
                        RunVerb::Close => ControlRequestV1::Close {
                            expected,
                            caller: None,
                            interrupt: false,
                        },
                        _ => unreachable!("lifecycle retry is pause or close"),
                    };
                    ControlRequestV1::AdmittedMutation {
                        admission_id: admission_id.expect("lifecycle admission"),
                        request: Box::new(request),
                    }
                },
            )
            .map_err(|error| error.machine_error(run_id, &runtime_record))?
        } else {
            crate::worker::call_run_worker(
                state_root,
                run_id,
                uid,
                carrier.map(CredentialCarrier::raw_fd),
                |expected| ControlRequestV1::AdmittedMutation {
                    admission_id: admission_id.expect("lifecycle admission"),
                    request: Box::new(ControlRequestV1::SettleLifecycleTimeout {
                        expected,
                        caller: None,
                        close: verb == RunVerb::Close,
                    }),
                },
            )
            .map_err(|error| error.machine_error(run_id, &runtime_record))?
        }
    } else {
        response
    };
    let lifecycle_stopped = match &response {
        ControlResponseV1::Closed { .. } => true,
        ControlResponseV1::Status { lifecycle, .. } => lifecycle == "paused",
        _ => false,
    };
    let closed = matches!(response, ControlResponseV1::Closed { .. });
    let mut control_socket_epoch = crate::worker::read_runtime_record(&runtime_record, uid)
        .map_or(1, |record| record.control_socket_epoch);
    if lifecycle_stopped {
        stop_lifecycle_generation(state_root, run_id, &runtime_record)?;
        control_socket_epoch = 0;
    }
    let value = control_response_value(
        state_root,
        run_id,
        control_socket_epoch,
        verb,
        response.clone(),
    )?;
    if closed {
        let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run_id)?;
        // The Run is already durably closed. A concurrently vanished profile
        // lifetime cannot turn that completed mutation into a retryable close
        // failure (which would invite replay of an effect that already
        // happened). A live lifetime still receives the membership release;
        // an absent/replaced one has no server left for the member to gate.
        let _ = release_global_run_membership(&manifest, run_id);
    }
    Ok((value, Some(response)))
}

fn stop_lifecycle_generation(
    state_root: &Path,
    run_id: Uuid,
    runtime_record: &Path,
) -> Result<(), MachineError> {
    let uid = DarwinSystem.current_uid();
    let record = crate::worker::read_runtime_record(runtime_record, uid)
        .map_err(|error| error.machine_error(run_id, runtime_record))?;
    let response = crate::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
        ControlRequestV1::Shutdown { expected }
    })
    .map_err(|error| error.machine_error(run_id, runtime_record))?;
    if !matches!(response, ControlResponseV1::Shutdown { .. }) {
        return Err(internal(
            "lifecycle shutdown returned an unexpected response",
        ));
    }
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        let _ = DarwinSystem.reap_child_nonblocking(record.identity.pid);
        if crate::worker::prove_worker_generation_absent(&record).is_ok() {
            crate::worker::remove_verified_absent_runtime(runtime_record, uid)
                .map_err(|error| error.machine_error(run_id, runtime_record))?;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(MachineError::new(
                "RECOVERY_REQUIRED",
                "the stopped run generation did not reach verified absence",
                false,
                json!({
                    "run_id": run_id,
                    "generation": record.identity.run_generation,
                    "identity_verdict": crate::worker::classify_worker_identity(&record).as_str(),
                    "reason": "generation_cleanup_unverified",
                }),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Quiesce one member selected by a durable Profile stop fence, then append
/// the Run-local operator-override observation before the Profile Server may
/// be signalled. The Profile fence prevents a new worker from completing
/// startup; the Run startup range closes the remaining in-flight election.
#[cfg(target_os = "macos")]
pub fn quiesce_profile_member(
    home_root: &Path,
    member: &crate::global_runtime::GlobalMembershipRecord,
    profile: &str,
    server_key: &str,
    server_epoch: u64,
    operation_id: Uuid,
) -> Result<String, MachineError> {
    let state_root = home_root.join("workspaces").join(&member.workspace_id);
    let run_root = state_root.join("runs").join(member.run_id.to_string());
    let locator_matches = member
        .runtime_locator
        .as_deref()
        .and_then(|locator| std::fs::canonicalize(locator).ok())
        .zip(std::fs::canonicalize(&run_root).ok())
        .is_some_and(|(locator, expected)| locator == expected);
    if !locator_matches {
        return Err(MachineError::new(
            "PROFILE_MEMBERSHIP_INCOMPLETE",
            "global Profile membership cannot be proven",
            false,
            json!({
                "profile": profile,
                "server_key": server_key,
                "reason": "member runtime locator does not match its durable identity",
            }),
        ));
    }
    let manifest =
        RunStore::new(SystemWorkspacePlatform, &state_root).load_manifest(member.run_id)?;
    let binding = manifest.global_profile_binding.as_ref().ok_or_else(|| {
        MachineError::new(
            "PROFILE_MEMBERSHIP_INCOMPLETE",
            "global Profile membership cannot be proven",
            false,
            json!({"profile": profile, "server_key": server_key, "reason": "member profile binding is missing"}),
        )
    })?;
    if manifest.workspace_id != member.workspace_id || binding.server_key != server_key {
        return Err(MachineError::new(
            "PROFILE_MEMBERSHIP_INCOMPLETE",
            "global Profile membership cannot be proven",
            false,
            json!({"profile": profile, "server_key": server_key, "reason": "member manifest does not match the stop scope"}),
        ));
    }

    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let before = store.load_state_projection(member.run_id)?;
    let runtime_root = crate::worker::runtime_root(&state_root);
    let runtime_record = crate::worker::runtime_record_path(&runtime_root, member.run_id)
        .map_err(|error| error.machine_error(member.run_id, &state_root))?;
    if runtime_record.exists() {
        let record =
            crate::worker::read_runtime_record(&runtime_record, DarwinSystem.current_uid())
                .map_err(|error| error.machine_error(member.run_id, &runtime_record))?;
        match crate::worker::classify_worker_identity(&record) {
            crate::worker::ProcessIdentityVerdict::Match => {
                stop_lifecycle_generation(&state_root, member.run_id, &runtime_record)?;
            }
            crate::worker::ProcessIdentityVerdict::Absent => {
                crate::worker::remove_verified_absent_runtime(
                    &runtime_record,
                    DarwinSystem.current_uid(),
                )
                .map_err(|error| error.machine_error(member.run_id, &runtime_record))?;
            }
            verdict => {
                return Err(MachineError::new(
                    "RECOVERY_REQUIRED",
                    "the member worker cannot be safely interrupted",
                    false,
                    json!({
                        "run_id": member.run_id,
                        "generation": record.identity.run_generation,
                        "identity_verdict": verdict.as_str(),
                        "reason": "worker_identity_does_not_authorize_interrupt",
                    }),
                ));
            }
        }
    }

    let lock = RunStartupLock::open(&state_root, member.run_id)?;
    lock.acquire()?;
    let result = (|| {
        if runtime_record.exists() {
            let record =
                crate::worker::read_runtime_record(&runtime_record, DarwinSystem.current_uid())
                    .map_err(|error| error.machine_error(member.run_id, &runtime_record))?;
            match crate::worker::classify_worker_identity(&record) {
                crate::worker::ProcessIdentityVerdict::Match => {
                    return Err(crate::controller::run_busy(
                        member.run_id,
                        "startup",
                        "a member worker appeared after the Profile stop fence",
                    ));
                }
                crate::worker::ProcessIdentityVerdict::Absent => {
                    crate::worker::remove_verified_absent_runtime(
                        &runtime_record,
                        DarwinSystem.current_uid(),
                    )
                    .map_err(|error| error.machine_error(member.run_id, &runtime_record))?;
                }
                verdict => {
                    return Err(MachineError::new(
                        "RECOVERY_REQUIRED",
                        "the member worker that appeared after the Profile stop fence cannot be safely replaced",
                        false,
                        json!({
                            "run_id": member.run_id,
                            "generation": record.identity.run_generation,
                            "identity_verdict": verdict.as_str(),
                            "reason": "worker_identity_does_not_authorize_replacement",
                        }),
                    ));
                }
            }
        }
        crate::writer::WriterStore::new(
            &state_root,
            &manifest.workspace_id,
            DarwinSystem.current_uid(),
        )
        .observe_locked(|writer| -> Result<String, MachineError> {
            let mut ledger = ConformantLedger::open(&run_root, member.run_id)
                .map_err(|error| internal(error.to_string()))?;
            let context = crate::event::EventAppendContext {
                workspace_id: manifest.workspace_id.clone(),
                server_key: server_key.to_owned(),
                server_epoch,
                writer_state_revision: writer.authority_revision,
            };
            ledger.with_event_context(context, |ledger| -> Result<String, MachineError> {
                let operation_id_text = operation_id.to_string();
                let observations = ledger
                    .inner()
                    .payloads_of_kind(AuditKind::ProfileObserved)
                    .map_err(|error| internal(error.to_string()))?;
                if let Some(outcome) =
                    recorded_profile_observation_outcome(&observations, &operation_id_text)?
                {
                    return Ok(outcome);
                }
                let observed_turn = ledger
                    .inner()
                    .durable_records()
                    .map_err(|error| internal(error.to_string()))?
                    .iter()
                    .filter(|record| {
                        record.sequence() > before.ledger_head.sequence
                            && record.kind() == AuditKind::TurnStarted
                    })
                    .filter_map(|record| {
                        let crate::jcs::LosslessJson::Object(entries) = record.payload() else {
                            return None;
                        };
                        entries.iter().find_map(|(name, value)| {
                            (name == "turn_id")
                                .then_some(value)
                                .and_then(|value| match value {
                                    crate::jcs::LosslessJson::String(value) => Some(value.clone()),
                                    _ => None,
                                })
                        })
                    })
                    .next_back()
                    .or_else(|| before.active_turn_id.clone());
                let timestamp = SystemLedgerClock::default().timestamp();
                let after = ledger
                    .verify()
                    .map_err(|error| internal(error.to_string()))?;
                let (outcome, mark_unknown) =
                    profile_quiesce_outcome(observed_turn.is_some(), after.lifecycle);
                if mark_unknown {
                    ledger
                        .mark_profile_interrupt_outcome_unknown(&timestamp, operation_id)
                        .map_err(|error| internal(error.to_string()))?;
                }
                let payload = json!({
                    "observation": "operator_override",
                    "operation_id": operation_id,
                    "profile": profile,
                    "server_key": server_key,
                    "server_epoch": server_epoch,
                    "turn_id": observed_turn,
                    "outcome": outcome,
                });
                ledger
                    .append_profile_observation(&timestamp, operation_id, &payload)
                    .map_err(|error| internal(error.to_string()))?;
                Ok(outcome.to_owned())
            })
        })?
    })();
    lock.release();
    result
}

#[cfg(target_os = "macos")]
fn recorded_profile_observation_outcome(
    observations: &[Value],
    operation_id: &str,
) -> Result<Option<String>, MachineError> {
    let Some(existing) = observations
        .iter()
        .find(|value| value.get("operation_id").and_then(Value::as_str) == Some(operation_id))
    else {
        return Ok(None);
    };
    existing
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|value| {
            matches!(
                *value,
                "terminal_observed" | "no_active_turn" | "outcome_unknown"
            )
        })
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| internal("operator override outcome is invalid"))
}

/// Classify the durable state on both sides of worker shutdown. The caller
/// records `outcome_unknown` when a worker vanished without advancing the
/// ledger, before it appends the operator-override observation.
#[cfg(target_os = "macos")]
fn profile_quiesce_outcome(had_active_turn: bool, after: RunLifecycle) -> (&'static str, bool) {
    if after == RunLifecycle::OutcomeUnknown {
        ("outcome_unknown", false)
    } else if matches!(
        after,
        RunLifecycle::Running
            | RunLifecycle::WaitingInteraction
            | RunLifecycle::ReconciliationRequired
    ) {
        ("outcome_unknown", true)
    } else if had_active_turn {
        ("terminal_observed", false)
    } else {
        ("no_active_turn", false)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn quiesce_profile_member(
    _home_root: &Path,
    _member: &crate::global_runtime::GlobalMembershipRecord,
    _profile: &str,
    _server_key: &str,
    _server_epoch: u64,
    _operation_id: Uuid,
) -> Result<String, MachineError> {
    Err(MachineError::new(
        "INTERNAL_ERROR",
        "global Profile member quiescence requires macOS",
        false,
        json!({"invariant": "global Profile member quiescence requires macOS"}),
    ))
}

// Before a dedicated runtime exists, events retain the real Profile observation
// accepted into this Run's capability snapshot; this is bootstrap provenance,
// not a claim that the Run has materialized a dedicated generation.
fn bootstrap_event_context(
    manifest: &RunManifest,
    writer: &crate::writer::WriterRecord,
) -> crate::event::EventAppendContext {
    crate::event::EventAppendContext {
        workspace_id: manifest.workspace_id.clone(),
        server_key: manifest.profile_capability_snapshot.server_key.clone(),
        server_epoch: manifest.profile_capability_snapshot.server_epoch,
        writer_state_revision: writer.authority_revision,
    }
}

/// Complete threadless lifecycle changes or close an already stopped paused Run.
/// The startup range excludes a concurrent first-turn worker election; after
/// taking it we re-read both runtime and projection, so this path cannot race
/// a worker into existence or mistake a crashed, history-bearing generation
/// for a process-free run.
fn offline_lifecycle_transition(
    state_root: &Path,
    run_id: Uuid,
    verb: RunVerb,
    runtime_record: &Path,
    carrier: Option<&CredentialCarrier>,
    expected_state_revision: Option<u64>,
) -> Result<Option<SemanticResult>, MachineError> {
    if !matches!(std::fs::symlink_metadata(runtime_record), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    let lock = RunStartupLock::open(state_root, run_id)?;
    lock.acquire()?;
    let result = (|| {
        if !matches!(std::fs::symlink_metadata(runtime_record), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        let store = RunStore::new(SystemWorkspacePlatform, state_root);
        let projection = store.load_state_projection(run_id)?;
        let stopped_paused = verb == RunVerb::Close && projection.lifecycle == RunLifecycle::Paused;
        if (projection.thread_id.is_some() && !stopped_paused)
            || !matches!(
                projection.lifecycle,
                RunLifecycle::Idle | RunLifecycle::Paused
            )
        {
            return Ok(None);
        }
        lock.lock
            .require_unowned_worker_range()
            .map_err(|error| worker_startup_range_error(error, run_id, lock.lock.path()))?;
        if let Some(carrier) = carrier {
            require_mutation_revision(
                state_root,
                run_id,
                carrier,
                verb.operation_name(),
                expected_state_revision,
            )?;
        }
        if verb == RunVerb::Pause && projection.lifecycle == RunLifecycle::Paused {
            return run_object(state_root, run_id, 0, "Absent")
                .map(SemanticResult::Run)
                .map(Some);
        }
        let run_root = state_root.join("runs").join(run_id.to_string());
        let manifest = store.load_manifest(run_id)?;
        let writer_store = crate::writer::WriterStore::new(
            state_root,
            &manifest.workspace_id,
            DarwinSystem.current_uid(),
        );
        writer_store.transact(|writer| -> Result<(), MachineError> {
            if let Some(carrier) = carrier {
                require_mutation_revision(
                    state_root,
                    run_id,
                    carrier,
                    verb.operation_name(),
                    expected_state_revision,
                )?;
            }
            if stopped_paused
                && writer
                    .holder
                    .as_ref()
                    .is_some_and(|holder| holder.run_id == run_id)
                && writer.state != crate::writer::WriterAuthorityState::Active
            {
                return Err(run_state_conflict_error(
                    run_id,
                    projection.lifecycle,
                    verb.operation_name(),
                ));
            }
            let mut ledger = ConformantLedger::open(&run_root, run_id)
                .map_err(|error| internal(error.to_string()))?;
            let mut event_context = bootstrap_event_context(&manifest, writer);
            if projection.thread_id.is_some() {
                let observed = crate::ledger::ObservedLedger::open(
                    &run_root,
                    run_id,
                    projection.ledger_head.sequence,
                )
                .map_err(|error| internal(error.to_string()))?;
                let event = observed
                    .records()
                    .iter()
                    .rev()
                    .find_map(crate::audit::AuditRecord::client_projection)
                    .ok_or_else(|| internal("stopped Run has no durable lane provenance"))?;
                event_context.server_key = event.record.server_key.clone();
                event_context.server_epoch = event.record.server_epoch;
            }
            ledger.with_event_context(event_context, |ledger| -> Result<(), MachineError> {
                let timestamp = SystemLedgerClock::default().timestamp();
                match verb {
                    RunVerb::Pause => ledger.pause_idle(&timestamp),
                    RunVerb::Close => ledger.seal_closed(
                        &timestamp,
                        if stopped_paused {
                            "paused_generation_had_verified_absent_runtime"
                        } else {
                            "threadless_run_had_no_worker_connection_or_owned_runtime"
                        },
                    ),
                    _ => unreachable!("only pause and close use threadless lifecycle"),
                }
                .map_err(|error| internal(error.to_string()))?;
                Ok(())
            })?;
            if stopped_paused
                && writer
                    .holder
                    .as_ref()
                    .is_some_and(|holder| holder.run_id == run_id)
            {
                let transaction_id = Uuid::now_v7();
                writer.prepare_release(run_id, transaction_id)?;
                writer.commit_release(transaction_id)?;
            }
            Ok(())
        })?;
        run_object(state_root, run_id, 0, "Absent")
            .map(SemanticResult::Run)
            .map(Some)
    })();
    lock.release();
    result
}

pub(crate) fn acquire_external_writer(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    carrier: &CredentialCarrier,
) -> Result<(), MachineError> {
    acquire_writer_authorized(
        view,
        state_root,
        run_id,
        carrier,
        true,
        Some(engagement_id),
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn acquire_writer_authorized(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    allow_threadless: bool,
    external_engagement: Option<Uuid>,
    expected_state_revision: Option<u64>,
    admission_id: Option<Uuid>,
) -> Result<(), MachineError> {
    let uid = DarwinSystem.current_uid();
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    let manifest = store.load_manifest(run_id)?;
    let projection = store.load_state_projection(run_id)?;
    if manifest.execution_lane != ExecutionLane::Dedicated {
        return Err(write_forbidden(state_root, run_id));
    }
    if !allow_threadless && projection.thread_id.is_none() {
        return Err(MachineError::new(
            "THREADLESS_REQUIRES_WRITE_TURN",
            "a threadless run must acquire writer authority through its first write turn",
            false,
            json!({"run_id": run_id, "required_action": "submit_turn_write"}),
        ));
    }
    if !writer_acquire_lifecycle_allowed(projection.lifecycle) {
        return Err(run_state_conflict_error(
            run_id,
            projection.lifecycle,
            "run.acquire_write",
        ));
    }
    let binding = store.load_controller_binding(run_id)?;
    if external_engagement.is_none() {
        authorize_controller(run_id, "run.acquire_write", &binding, carrier)?;
    }
    let writer = crate::writer::WriterStore::new(state_root, &view.workspace_id, uid);
    let transaction_id = Uuid::now_v7();
    let holder = crate::writer::WriterStore::holder(
        run_id,
        manifest.profile.profile_name.clone(),
        &binding,
        projection.run_generation.max(1),
        manifest.profile_capability_snapshot.server_key.clone(),
        manifest.profile_capability_snapshot.server_epoch,
        projection.thread_id.clone(),
        projection.lifecycle,
    );
    let (_, (replayed, next_generation)) = writer.transact(|record| {
        if let Some(admission_id) = admission_id {
            require_writer_admission(state_root, run_id, carrier, admission_id)?;
        } else if external_engagement.is_none() {
            require_mutation_revision(
                state_root,
                run_id,
                carrier,
                "run.acquire_write",
                expected_state_revision,
            )?;
        }
        record.prepare_acquire(run_id, holder.clone(), transaction_id)
    })?;
    if replayed {
        return Ok(());
    }
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    if !runtime_record.exists()
        && let Err(error) = ensure_run_worker(view, state_root, run_id)
    {
        writer.transact(|record| {
            record.cancel_acquire(transaction_id);
            Ok(())
        })?;
        return Err(error);
    }
    let worker_record = match crate::worker::read_runtime_record(&runtime_record, uid) {
        Ok(record) => record,
        Err(error) => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(error.machine_error(run_id, &runtime_record));
        }
    };
    if let Err(error) = writer.transact(|record| {
        record.bind_reserved_generation(
            transaction_id,
            worker_record.identity.run_generation,
            worker_record
                .app_server_epoch
                .unwrap_or(manifest.profile_capability_snapshot.server_epoch),
        )
    }) {
        writer.transact(|record| {
            if record.transaction_id == Some(transaction_id) {
                record.block_unknown(transaction_id);
            }
            Ok(())
        })?;
        return Err(error);
    }
    if prove_writer_pre_effect_absence(
        &writer,
        transaction_id,
        WriterPreEffectTransition::Acquire,
        || crate::worker::prove_worker_workload_absent(&worker_record),
    )?
    .is_err()
    {
        return Err(background_execution_unverified(
            run_id,
            &projection,
            &worker_record,
            "the recorded worker group did not produce five complete empty workload samples",
            "retry_census",
        ));
    }
    let response = crate::worker::call_run_worker(
        state_root,
        run_id,
        uid,
        Some(carrier.raw_fd()),
        |expected| {
            let request = external_engagement.map_or(
                ControlRequestV1::SetWriterAccess {
                    expected: expected.clone(),
                    caller: None,
                    write: true,
                    writer_generation: next_generation,
                    transaction_id,
                },
                |engagement_id| ControlRequestV1::ExternalSetWriterAccess {
                    expected,
                    caller: None,
                    engagement_id,
                    write: true,
                    writer_generation: next_generation,
                    transaction_id,
                },
            );
            match admission_id {
                Some(admission_id) => ControlRequestV1::AdmittedMutation {
                    admission_id,
                    request: Box::new(request),
                },
                None => request,
            }
        },
    );
    match response {
        Ok(ControlResponseV1::WriterAccessChanged {
            write: true,
            writer_generation,
            ..
        }) if writer_generation == next_generation => {}
        Ok(ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        }) => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(MachineError::new(code, message, retryable, details));
        }
        Err(error) => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(error.machine_error(run_id, &runtime_record));
        }
        _ => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(internal(
                "worker returned an invalid writer transition response",
            ));
        }
    }
    writer.transact(|record| record.commit_acquire(transaction_id, next_generation))?;
    Ok(())
}

const fn resume_lifecycle_allowed(lifecycle: RunLifecycle) -> bool {
    matches!(lifecycle, RunLifecycle::Paused)
}

const fn writer_acquire_lifecycle_allowed(lifecycle: RunLifecycle) -> bool {
    matches!(lifecycle, RunLifecycle::Idle | RunLifecycle::Paused)
}

fn ensure_worker_spawn_lifecycle(
    run_id: Uuid,
    lifecycle: RunLifecycle,
) -> Result<(), MachineError> {
    match lifecycle {
        RunLifecycle::Closed => Err(run_state_conflict_error(
            run_id,
            lifecycle,
            "run.ensure_worker",
        )),
        RunLifecycle::Starting
        | RunLifecycle::Idle
        | RunLifecycle::Running
        | RunLifecycle::WaitingInteraction
        | RunLifecycle::ReconciliationRequired
        | RunLifecycle::Paused
        | RunLifecycle::StartFailed
        | RunLifecycle::OutcomeUnknown => Ok(()),
    }
}

const fn external_writer_release_can_bypass_lifecycle(
    lifecycle: RunLifecycle,
    has_external_engagement: bool,
) -> bool {
    match lifecycle {
        RunLifecycle::Closed => has_external_engagement,
        RunLifecycle::Starting
        | RunLifecycle::Idle
        | RunLifecycle::Running
        | RunLifecycle::WaitingInteraction
        | RunLifecycle::ReconciliationRequired
        | RunLifecycle::Paused
        | RunLifecycle::StartFailed
        | RunLifecycle::OutcomeUnknown => false,
    }
}

pub(crate) fn release_external_writer(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    carrier: &CredentialCarrier,
) -> Result<(), MachineError> {
    release_writer_authorized(view, state_root, run_id, carrier, Some(engagement_id), None)
}

fn release_writer_authorized(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    external_engagement: Option<Uuid>,
    expected_state_revision: Option<u64>,
) -> Result<(), MachineError> {
    let uid = DarwinSystem.current_uid();
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    let projection = store.load_state_projection(run_id)?;
    let externally_closed = external_writer_release_can_bypass_lifecycle(
        projection.lifecycle,
        external_engagement.is_some(),
    );
    if !matches!(
        projection.lifecycle,
        RunLifecycle::Idle | RunLifecycle::Paused
    ) && !externally_closed
    {
        return Err(run_state_conflict_error(
            run_id,
            projection.lifecycle,
            "run.release_write",
        ));
    }
    let binding = store.load_controller_binding(run_id)?;
    if external_engagement.is_none() {
        authorize_controller(run_id, "run.release_write", &binding, carrier)?;
    }
    let writer = crate::writer::WriterStore::new(state_root, &view.workspace_id, uid);
    let transaction_id = Uuid::now_v7();
    let (_, generation) = writer.transact(|record| {
        if external_engagement.is_none() {
            require_mutation_revision(
                state_root,
                run_id,
                carrier,
                "run.release_write",
                expected_state_revision,
            )?;
        }
        record.prepare_release(run_id, transaction_id)
    })?;
    let Some(generation) = generation else {
        return Ok(());
    };
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    if commit_external_closed_writer_release_if_runtime_absent(
        &writer,
        transaction_id,
        &runtime_record,
        externally_closed,
    )? {
        return Ok(());
    }
    let worker_record = match crate::worker::read_runtime_record(&runtime_record, uid) {
        Ok(record) => record,
        Err(error) => {
            writer.transact(|record| {
                record.rollback_release(transaction_id);
                Ok(())
            })?;
            return Err(error.machine_error(run_id, &runtime_record));
        }
    };
    if prove_writer_pre_effect_absence(
        &writer,
        transaction_id,
        WriterPreEffectTransition::Release,
        || crate::worker::prove_worker_workload_absent(&worker_record),
    )?
    .is_err()
    {
        return Err(background_execution_unverified(
            run_id,
            &projection,
            &worker_record,
            "the recorded worker group did not produce five complete empty workload samples",
            "retry_census",
        ));
    }
    let response = crate::worker::call_run_worker(
        state_root,
        run_id,
        uid,
        Some(carrier.raw_fd()),
        |expected| {
            external_engagement.map_or(
                ControlRequestV1::SetWriterAccess {
                    expected: expected.clone(),
                    caller: None,
                    write: false,
                    writer_generation: generation,
                    transaction_id,
                },
                |engagement_id| ControlRequestV1::ExternalSetWriterAccess {
                    expected,
                    caller: None,
                    engagement_id,
                    write: false,
                    writer_generation: generation,
                    transaction_id,
                },
            )
        },
    );
    match response {
        Ok(ControlResponseV1::WriterAccessChanged {
            write: false,
            writer_generation,
            ..
        }) if writer_generation == generation => {}
        Ok(ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        }) => {
            writer.transact(|record| {
                record.rollback_release(transaction_id);
                Ok(())
            })?;
            return Err(MachineError::new(code, message, retryable, details));
        }
        Err(error) => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(error.machine_error(run_id, &runtime_record));
        }
        _ => {
            writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            })?;
            return Err(internal(
                "worker returned an invalid writer transition response",
            ));
        }
    }
    writer.transact(|record| record.commit_release(transaction_id))?;
    Ok(())
}

fn run_state_conflict_error(run_id: Uuid, state: RunLifecycle, operation: &str) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        format!(
            "{operation} is not allowed while the run is {}",
            state.as_str()
        ),
        false,
        json!({"run_id": run_id, "state": state.as_str(), "operation": operation}),
    )
}

fn background_execution_unverified(
    run_id: Uuid,
    projection: &crate::projection::RunStateProjection,
    worker: &crate::worker::WorkerRuntimeRecord,
    reason: &str,
    required_action: &str,
) -> MachineError {
    MachineError::new(
        "BACKGROUND_EXECUTION_UNVERIFIED",
        "dedicated workload absence could not be proved",
        false,
        json!({
            "run_id": run_id,
            "thread_id": projection.thread_id.clone().unwrap_or_else(|| "unbound".to_owned()),
            "server_epoch": worker.control_socket_epoch.max(1),
            "lane_id": run_id,
            "process_generation": projection.run_generation.max(1),
            "census_revision": worker.control_socket_epoch,
            "reason": reason,
            "required_action": required_action,
        }),
    )
}

#[derive(Clone, Copy)]
enum WriterPreEffectTransition {
    Acquire,
    Release,
}

fn prove_writer_pre_effect_absence<F>(
    writer: &crate::writer::WriterStore,
    transaction_id: Uuid,
    transition: WriterPreEffectTransition,
    prove: F,
) -> Result<
    Result<crate::worker::BackgroundAbsenceEvidence, crate::worker::WorkerProtocolError>,
    MachineError,
>
where
    F: FnOnce() -> Result<
        crate::worker::BackgroundAbsenceEvidence,
        crate::worker::WorkerProtocolError,
    >,
{
    let proof = prove();
    if proof.is_err() {
        let rollback = writer.transact(|record| {
            match transition {
                WriterPreEffectTransition::Acquire => record.cancel_acquire(transaction_id),
                WriterPreEffectTransition::Release => record.rollback_release(transaction_id),
            }
            Ok(())
        });
        if let Err(error) = rollback {
            // A failed rollback makes the persisted transition uncertain. Try
            // to fence it explicitly rather than leaving a reusable-looking
            // reservation behind; if storage is unavailable, the original
            // persistence error remains the most precise result.
            let _ = writer.transact(|record| {
                record.block_unknown(transaction_id);
                Ok(())
            });
            return Err(error);
        }
    }
    Ok(proof)
}

fn commit_external_closed_writer_release_if_runtime_absent(
    writer: &crate::writer::WriterStore,
    transaction_id: Uuid,
    runtime_record: &Path,
    externally_closed: bool,
) -> Result<bool, MachineError> {
    if !externally_closed {
        return Ok(false);
    }
    // This check-to-commit sequence is safe only while every worker creation
    // path refuses a durable Closed lifecycle. `ensure_run_worker` enforces
    // that invariant immediately before spawning a missing generation.
    match std::fs::symlink_metadata(runtime_record) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writer.transact(|record| record.commit_release(transaction_id))?;
            Ok(true)
        }
        Ok(_) | Err(_) => Ok(false),
    }
}

fn parse_uuid_flag(args: &[OsString], flag: &str) -> Result<Uuid, MachineError> {
    required(args, flag)?
        .parse()
        .map_err(|_| MachineError::invalid_argument(flag, "value must be a UUID"))
}

fn parse_u64_flag(args: &[OsString], flag: &str) -> Result<u64, MachineError> {
    required(args, flag)?
        .parse()
        .map_err(|_| MachineError::invalid_argument(flag, "value must be a positive integer"))
}

fn reset_writer(
    view: &WorkspaceView,
    state_root: &Path,
    args: &[OsString],
) -> Result<Value, MachineError> {
    if required(args, "--confirm-workspace-id")? != view.workspace_id {
        return Err(MachineError::invalid_argument(
            "--confirm-workspace-id",
            "confirmation does not match the canonical workspace",
        ));
    }
    if !switch(args, "--require-worker-absence") {
        return Err(MachineError::invalid_argument(
            "--require-worker-absence",
            "explicit worker-absence proof is required",
        ));
    }
    let carrier = carrier_from_options(args, "--operator-file", "--operator-fd")?;
    let authorization =
        crate::controller::OperatorStore::new(crate::controller::default_operator_root()?)
            .authorize(&carrier)?;
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid());
    let current = writer.load()?;
    if let Some(holder) = &current.holder
        && let Err(error) =
            prove_recorded_writer_absent(state_root, holder, DarwinSystem.current_uid())
    {
        authorization.release();
        return Err(error);
    }
    let (record, ()) = writer.transact(|record| {
        if !matches!(record.state, crate::writer::WriterAuthorityState::BlockedUnknown | crate::writer::WriterAuthorityState::None) {
            return Err(MachineError::new(
                "RECOVERY_REQUIRED",
                "writer reset is limited to blocked_unknown authority",
                false,
                json!({"reason": "authority_not_blocked", "required_action": "reconcile_dedicated_lane"}),
            ));
        }
        let generation = record.writer_generation;
        let writer_lock_device = record.writer_lock_device;
        let writer_lock_inode = record.writer_lock_inode;
        let handoff_lock_device = record.handoff_lock_device;
        let handoff_lock_inode = record.handoff_lock_inode;
        *record = crate::writer::WriterRecord::empty(&view.workspace_id);
        record.writer_generation = generation;
        record.writer_lock_device = writer_lock_device;
        record.writer_lock_inode = writer_lock_inode;
        record.handoff_lock_device = handoff_lock_device;
        record.handoff_lock_inode = handoff_lock_inode;
        Ok(())
    })?;
    authorization.release();
    let _ = record;
    writer.status_value()
}

fn prove_recorded_writer_absent(
    state_root: &Path,
    holder: &crate::writer::WriterHolder,
    uid: u32,
) -> Result<(), MachineError> {
    let path =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), holder.run_id)
            .map_err(|_| recorded_writer_absence_unverified(holder, "unverifiable"))?;
    let record = crate::worker::read_runtime_record(&path, uid)
        .map_err(|_| recorded_writer_absence_unverified(holder, "unverifiable"))?;
    crate::worker::prove_worker_generation_absent(&record).map_err(|_| {
        recorded_writer_absence_unverified(
            holder,
            crate::worker::classify_worker_identity(&record).as_str(),
        )
    })?;
    Ok(())
}

fn recorded_writer_absence_unverified(
    holder: &crate::writer::WriterHolder,
    identity_verdict: &str,
) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "the recorded writer worker and process group have not been proved absent",
        false,
        json!({
            "run_id": holder.run_id,
            "generation": holder.worker_generation,
            "identity_verdict": identity_verdict,
            "reason": "worker_or_group_absence_unverified",
        }),
    )
}

fn prepare_writer_handoff(
    view: &WorkspaceView,
    state_root: &Path,
    args: &[OsString],
) -> Result<Value, MachineError> {
    let source = parse_uuid_flag(args, "--from")?;
    let destination = parse_uuid_flag(args, "--to")?;
    if source == destination {
        return Err(MachineError::invalid_argument(
            "--to",
            "handoff destination must differ",
        ));
    }
    let expected = parse_u64_flag(args, "--expected-generation")?;
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let runs = RunStore::new(SystemWorkspacePlatform, state_root);
    let source_binding = runs.load_controller_binding(source)?;
    let destination_binding = runs.load_controller_binding(destination)?;
    authorize_controller(
        source,
        "workspace.writer.handoff_prepare",
        &source_binding,
        &carrier,
    )?;
    authorize_controller(
        destination,
        "workspace.writer.handoff_prepare",
        &destination_binding,
        &carrier,
    )?;
    if source_binding.identity.controller_id != destination_binding.identity.controller_id {
        return Err(MachineError::new(
            "CROSS_CONTROLLER_RELEASE_REQUIRED",
            "writer handoff requires one controller to own both runs",
            false,
            json!({"source_run_id": source, "destination_run_id": destination}),
        ));
    }
    let source_manifest = runs.load_manifest(source)?;
    let destination_manifest = runs.load_manifest(destination)?;
    ensure_writer_handoff_lanes(
        source,
        source_manifest.execution_lane,
        destination,
        destination_manifest.execution_lane,
    )?;
    for run_id in [source, destination] {
        let projection = runs.load_state_projection(run_id)?;
        if projection.lifecycle != RunLifecycle::Idle || !projection.pending_requests.is_empty() {
            return Err(MachineError::new(
                "WRITER_HANDOFF_NOT_ALLOWED",
                "writer handoff requires idle runs without pending interactions",
                false,
                json!({"source_run_id": source, "destination_run_id": destination, "blockers": [format!("run_{run_id}_not_idle")]}),
            ));
        }
    }
    let handoff_id = Uuid::now_v7();
    let expires_at_unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| internal("system clock precedes Unix epoch"))?
        .as_secs()
        .saturating_add(300);
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid());
    let handoff = crate::writer::WriterHandoff {
        handoff_id,
        source_run_id: source,
        destination_run_id: destination,
        expected_writer_generation: expected,
        expires_at_unix_seconds,
        controller_id: source_binding.identity.controller_id,
        controller_generation: source_binding.identity.generation,
        source_retirement_started: false,
        source_retired: false,
    };
    writer.transact_handoff(|record| record.prepare_handoff(handoff.clone()))?;
    Ok(handoff_value(&handoff, "prepared"))
}

fn commit_writer_handoff(
    view: &WorkspaceView,
    state_root: &Path,
    args: &[OsString],
) -> Result<Value, MachineError> {
    let handoff_id = parse_uuid_flag(args, "--handoff-id")?;
    let expected = parse_u64_flag(args, "--expected-generation")?;
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid());
    let current = writer.load()?;
    let handoff = current.handoff.clone().filter(|value| value.handoff_id == handoff_id)
        .ok_or_else(|| MachineError::new(
            "WRITER_HANDOFF_NOT_ALLOWED",
            "writer handoff is not prepared",
            false,
            json!({"source_run_id": Value::Null, "destination_run_id": Value::Null, "blockers": ["handoff_not_prepared"]}),
        ))?;
    if current.writer_generation != expected || handoff.expected_writer_generation != expected {
        return Err(MachineError::new(
            "STALE_WRITER_GENERATION",
            "writer generation changed before handoff commit",
            false,
            json!({"expected_generation": expected, "actual_generation": current.writer_generation}),
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| internal("system clock precedes Unix epoch"))?
        .as_secs();
    if now >= handoff.expires_at_unix_seconds {
        return Err(MachineError::new(
            "WRITER_HANDOFF_NOT_ALLOWED",
            "writer handoff has expired",
            false,
            json!({"source_run_id": handoff.source_run_id, "destination_run_id": handoff.destination_run_id, "blockers": ["expired"]}),
        ));
    }
    let runs = RunStore::new(SystemWorkspacePlatform, state_root);
    let source_binding = runs.load_controller_binding(handoff.source_run_id)?;
    let destination_binding = runs.load_controller_binding(handoff.destination_run_id)?;
    authorize_controller(
        handoff.source_run_id,
        "workspace.writer.handoff_commit",
        &source_binding,
        &carrier,
    )?;
    authorize_controller(
        handoff.destination_run_id,
        "workspace.writer.handoff_commit",
        &destination_binding,
        &carrier,
    )?;
    let source_manifest = runs.load_manifest(handoff.source_run_id)?;
    let destination_manifest = runs.load_manifest(handoff.destination_run_id)?;
    ensure_writer_handoff_lanes(
        handoff.source_run_id,
        source_manifest.execution_lane,
        handoff.destination_run_id,
        destination_manifest.execution_lane,
    )?;
    let source_projection = runs.load_state_projection(handoff.source_run_id)?;
    let source_runtime_path = crate::worker::runtime_record_path(
        &crate::worker::runtime_root(state_root),
        handoff.source_run_id,
    )
    .map_err(|error| error.machine_error(handoff.source_run_id, state_root))?;
    let source_worker =
        crate::worker::read_runtime_record(&source_runtime_path, DarwinSystem.current_uid())
            .map_err(|error| error.machine_error(handoff.source_run_id, &source_runtime_path))?;
    if crate::worker::prove_worker_workload_absent(&source_worker).is_err() {
        writer.transact_handoff(|record| {
            if record.transaction_id == Some(handoff_id) {
                record.state = crate::writer::WriterAuthorityState::BlockedUnknown;
                record.handoff = None;
                record.recovery_action = Some("reconcile_dedicated_lane".to_owned());
            }
            Ok(())
        })?;
        return Err(background_execution_unverified(
            handoff.source_run_id,
            &source_projection,
            &source_worker,
            "source lane workload absence was not proved before handoff",
            "reconcile_dedicated_lane",
        ));
    }
    writer.transact_handoff(|record| record.begin_handoff_source_retirement(handoff_id))?;
    if let Err(error) = transition_worker_access(
        state_root,
        handoff.source_run_id,
        &carrier,
        false,
        expected,
        handoff_id,
    ) {
        return match error {
            WorkerAccessFailure::Refused(error) => {
                writer.transact_handoff(|record| {
                    record.abort_handoff_source_retirement(handoff_id)
                })?;
                Err(error)
            }
            WorkerAccessFailure::Unknown(error) => {
                writer.transact_handoff(|record| {
                    if record.transaction_id == Some(handoff_id) {
                        record.state = crate::writer::WriterAuthorityState::BlockedUnknown;
                        record.handoff = None;
                        record.recovery_action = Some("reverify_writer_policy".to_owned());
                    }
                    Ok(())
                })?;
                Err(error)
            }
        };
    }
    writer.transact_handoff(|record| record.retire_handoff_source(handoff_id))?;
    let projection = runs.load_state_projection(handoff.destination_run_id)?;
    let holder = crate::writer::WriterStore::holder(
        handoff.destination_run_id,
        destination_manifest.profile.profile_name.clone(),
        &destination_binding,
        projection.run_generation.max(1),
        destination_manifest
            .profile_capability_snapshot
            .server_key
            .clone(),
        destination_manifest
            .profile_capability_snapshot
            .server_epoch,
        projection.thread_id,
        projection.lifecycle,
    );
    let (_, destination_generation) = writer.transact_handoff(|record| {
        record.reserve_handoff_destination(handoff_id, expected, holder.clone())
    })?;
    if let Err(error) = transition_worker_access(
        state_root,
        handoff.destination_run_id,
        &carrier,
        true,
        destination_generation,
        handoff_id,
    ) {
        return match error {
            WorkerAccessFailure::Refused(error) => {
                writer.transact_handoff(|record| {
                    if record.transaction_id == Some(handoff_id) {
                        record.cancel_acquire(handoff_id);
                    }
                    Ok(())
                })?;
                Err(error)
            }
            WorkerAccessFailure::Unknown(error) => {
                writer.transact_handoff(|record| {
                    if record.transaction_id == Some(handoff_id) {
                        record.block_unknown(handoff_id);
                    }
                    Ok(())
                })?;
                Err(error)
            }
        };
    }
    let destination_runtime = crate::worker::runtime_record_path(
        &crate::worker::runtime_root(state_root),
        handoff.destination_run_id,
    )
    .map_err(|error| error.machine_error(handoff.destination_run_id, state_root))?;
    let destination_worker =
        crate::worker::read_runtime_record(&destination_runtime, DarwinSystem.current_uid())
            .map_err(|error| {
                error.machine_error(handoff.destination_run_id, &destination_runtime)
            })?;
    writer.transact_handoff(|record| {
        record.bind_reserved_generation(
            handoff_id,
            destination_worker.identity.run_generation,
            destination_worker
                .app_server_epoch
                .ok_or_else(|| internal("destination server epoch is missing"))?,
        )?;
        record.commit_acquire(handoff_id, destination_generation)
    })?;
    Ok(handoff_value(&handoff, "committed"))
}

fn ensure_writer_handoff_lanes(
    source_run_id: Uuid,
    source_lane: ExecutionLane,
    destination_run_id: Uuid,
    destination_lane: ExecutionLane,
) -> Result<(), MachineError> {
    let mut blockers = Vec::new();
    if source_lane != ExecutionLane::Dedicated {
        blockers.push("source_run_not_dedicated");
    }
    if destination_lane != ExecutionLane::Dedicated {
        blockers.push("destination_run_not_dedicated");
    }
    if blockers.is_empty() {
        return Ok(());
    }
    Err(MachineError::new(
        "WRITER_HANDOFF_NOT_ALLOWED",
        "writer handoff requires dedicated source and destination runs",
        false,
        json!({
            "source_run_id": source_run_id,
            "destination_run_id": destination_run_id,
            "blockers": blockers,
        }),
    ))
}

fn cancel_writer_handoff(
    view: &WorkspaceView,
    state_root: &Path,
    args: &[OsString],
) -> Result<Value, MachineError> {
    let handoff_id = parse_uuid_flag(args, "--handoff-id")?;
    let carrier = carrier_from_options(args, "--controller-file", "--controller-fd")?;
    let runs = RunStore::new(SystemWorkspacePlatform, state_root);
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid());
    let current = writer.load()?;
    let handoff = current.handoff.or(current.cancelled_handoff).filter(|value| value.handoff_id == handoff_id && value.expected_writer_generation == current.writer_generation)
        .ok_or_else(|| MachineError::new(
            "WRITER_HANDOFF_NOT_ALLOWED",
            "writer handoff is not prepared",
            false,
            json!({"source_run_id": Value::Null, "destination_run_id": Value::Null, "blockers": ["handoff_not_prepared"]}),
        ))?;
    let binding = runs.load_controller_binding(handoff.source_run_id)?;
    if binding.identity.controller_id != handoff.controller_id
        || binding.identity.generation != handoff.controller_generation
    {
        return Err(MachineError::new(
            "WRITER_HANDOFF_NOT_ALLOWED",
            "handoff controller generation changed",
            false,
            json!({"source_run_id":handoff.source_run_id, "destination_run_id":handoff.destination_run_id,
            "blockers":["controller_generation_changed"]}),
        ));
    }
    authorize_controller(
        handoff.source_run_id,
        "workspace.writer.handoff_cancel",
        &binding,
        &carrier,
    )?;
    writer.transact_handoff(|record| record.cancel_handoff(handoff_id))?;
    Ok(handoff_value(&handoff, "cancelled"))
}

fn transition_worker_access(
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    write: bool,
    generation: u64,
    transaction_id: Uuid,
) -> Result<(), WorkerAccessFailure> {
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| {
                WorkerAccessFailure::Unknown(error.machine_error(run_id, state_root))
            })?;
    match crate::worker::call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        Some(carrier.raw_fd()),
        |expected| ControlRequestV1::SetWriterAccess {
            expected,
            caller: None,
            write,
            writer_generation: generation,
            transaction_id,
        },
    ) {
        Ok(ControlResponseV1::WriterAccessChanged {
            write: observed,
            writer_generation,
            ..
        }) if observed == write && writer_generation == generation => Ok(()),
        Ok(ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        }) => Err(WorkerAccessFailure::Refused(MachineError::new(
            code, message, retryable, details,
        ))),
        Ok(_) => Err(WorkerAccessFailure::Unknown(internal(
            "worker returned an invalid writer transition response",
        ))),
        Err(error) => Err(WorkerAccessFailure::Unknown(
            error.machine_error(run_id, &runtime_record),
        )),
    }
}

enum WorkerAccessFailure {
    Refused(MachineError),
    Unknown(MachineError),
}

fn handoff_value(handoff: &crate::writer::WriterHandoff, status: &str) -> Value {
    json!({
        "handoff_id": handoff.handoff_id,
        "status": status,
        "source_run_id": handoff.source_run_id,
        "destination_run_id": handoff.destination_run_id,
        "expected_writer_generation": handoff.expected_writer_generation,
        "expires_at": crate::writer::timestamp_from_unix(handoff.expires_at_unix_seconds),
        "blockers": [],
    })
}

fn ensure_global_run_membership(
    home: &DolgoraeHome,
    workspace_id: &str,
    binding: &GlobalProfileBinding,
    run_id: Uuid,
    state: &crate::profile::ServerState,
) -> Result<(), MachineError> {
    let store = GlobalMembershipStore::new(home, &binding.selected_name, &binding.server_key)?;
    let key = format!("{workspace_id}:{run_id}");
    let membership = store.load()?;
    let existing = membership.members.get(&key);
    if existing.is_none_or(|member| {
        member.disposition != MembershipDisposition::Active
            || member.observed_epoch != Some(state.server_epoch)
    }) {
        crate::profile::reattach_global_run(
            binding,
            state,
            workspace_id,
            run_id,
            crate::global_runtime::GlobalMembershipFacts {
                controller_id: existing.and_then(|member| member.controller_id),
                worker_generation: existing.and_then(|member| member.worker_generation),
                thread_id: existing.and_then(|member| member.thread_id.clone()),
                connection_id: existing.and_then(|member| member.connection_id),
                lifecycle: "run_reattached".to_owned(),
                writer: false,
                observed_epoch: Some(state.server_epoch),
                runtime_locator: existing
                    .and_then(|member| member.runtime_locator.clone())
                    .or_else(|| {
                        Some(
                            home.root()
                                .join("workspaces")
                                .join(workspace_id)
                                .join("runs")
                                .join(run_id.to_string())
                                .to_string_lossy()
                                .into_owned(),
                        )
                    }),
            },
        )?;
    }
    Ok(())
}

fn release_global_run_membership(manifest: &RunManifest, run_id: Uuid) -> Result<(), MachineError> {
    let binding = manifest.global_profile_binding.as_ref().ok_or_else(|| {
        MachineError::new(
            "LEGACY_STATE_UNSUPPORTED",
            "legacy Run membership cannot be recovered after the global Profile cutover",
            false,
            json!({"run_id": run_id}),
        )
    })?;
    GlobalMembershipStore::new(
        &DolgoraeHome::system()?,
        &binding.selected_name,
        &binding.server_key,
    )?
    .record(
        &manifest.workspace_id,
        run_id,
        MembershipDisposition::Released,
    )
    .map(|_| ())
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
            | crate::worker::WorkerProtocolError::ControlRecoveryRequired { .. }
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
    match std::fs::symlink_metadata(runtime_record) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "Absent",
        Err(_) => return "Unverifiable",
        Ok(_) => {}
    }
    match crate::worker::read_runtime_record(runtime_record, DarwinSystem.current_uid()) {
        Ok(record) => crate::worker::classify_worker_identity(&record).as_str(),
        Err(_) => "Unverifiable",
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
    let ledger = crate::ledger::ObservedLedger::open_run(state_root, run_id, head)?;
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
    Ok(parse_event_cursor(&value).map_or_else(
        || RequestedCursor::Refused(canonical.to_owned()),
        RequestedCursor::Exact,
    ))
}

/// A control request with everything but the worker's identity already decided.
#[derive(Clone)]
enum Prepared {
    Plain,
    Pause {
        interrupt: bool,
    },
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
        idempotency_key: String,
        response: Value,
    },
}

impl Prepared {
    fn into_request(
        self,
        verb: RunVerb,
        expected: crate::worker::WorkerIdentity,
    ) -> ControlRequestV1 {
        match self {
            Prepared::Plain if verb == RunVerb::Status => ControlRequestV1::Status { expected },
            Prepared::Plain if verb == RunVerb::Resume => ControlRequestV1::Resume {
                expected,
                caller: None,
            },
            Prepared::Plain if matches!(verb, RunVerb::Reconcile | RunVerb::Recover) => {
                ControlRequestV1::Reconcile {
                    expected,
                    caller: None,
                }
            }
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
            Prepared::Pause { interrupt } => ControlRequestV1::Pause {
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
                idempotency_key,
                response,
            } => ControlRequestV1::Respond {
                expected,
                caller: None,
                request_id,
                idempotency_key,
                response,
            },
        }
    }

    fn normalized_request(&self, verb: RunVerb) -> Value {
        match self {
            Self::Plain => {
                json!({"operation": match verb {RunVerb::Resume => "resume", RunVerb::Reconcile | RunVerb::Recover => "reconcile", _ => "interrupt"}})
            }
            Self::Pause { interrupt } => json!({"operation":"pause","interrupt":interrupt}),
            Self::Close { interrupt } => json!({"operation":"close","interrupt":interrupt}),
            Self::Turn {
                request,
                timeout_ms,
            } if verb == RunVerb::Send => {
                json!({"operation":"send","request":request,"timeout_ms":timeout_ms})
            }
            Self::Turn { request, .. } => json!({"operation":"submit","request":request}),
            Self::Respond {
                request_id,
                idempotency_key,
                response,
            } => {
                json!({"operation":"respond","request_id":request_id,"idempotency_key":idempotency_key,"response":response})
            }
            Self::Wait {
                turn_id,
                timeout_ms,
            } => json!({"operation":"wait","turn_id":turn_id,"timeout_ms":timeout_ms}),
        }
    }
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
            let sources =
                RunSources::for_control_response(state_root, run_id, control_socket_epoch, &other)?;
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
    let ledger = crate::ledger::ObservedLedger::open_run(state_root, run_id, head)?;
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
type RunSources = crate::snapshot::RunSnapshot;

impl RunSources {
    /// The event cursor an observer may resume from.
    fn event_cursor(&self) -> String {
        self.projection
            .last_event_cursor
            .clone()
            .unwrap_or_else(|| "0".to_owned())
    }

    fn server_epoch(&self) -> u64 {
        self.app_server_epoch
            .unwrap_or(self.manifest.profile_capability_snapshot.server_epoch)
    }

    /// The Run's recorded default reasoning effort.
    fn default_effort(&self) -> String {
        self.projection
            .default_effort
            .clone()
            .unwrap_or_else(|| self.manifest.default_reasoning_effort.clone())
    }

    fn run_value(&self, identity_verdict: &str, last_terminal: Value) -> Value {
        let manifest = &self.manifest;
        let projection = &self.projection;
        let lifecycle = projection.lifecycle.as_str();
        let dedicated = manifest.execution_lane == ExecutionLane::Dedicated;
        let owns_writer = dedicated
            && self
                .writer
                .holder
                .as_ref()
                .is_some_and(|holder| holder.run_id == manifest.run_id);
        let writer_state = if owns_writer {
            self.writer.state.as_str()
        } else {
            "none"
        };
        let threadless = projection.thread_id.is_none();
        let state_variant = if !dedicated {
            "shared_readonly"
        } else if threadless {
            "dedicated_unstarted"
        } else {
            match writer_state {
                "active" => "dedicated_writer_active",
                "reserved" => "dedicated_writer_reserved",
                "handoff_prepared" | "releasing" => "dedicated_releasing",
                "blocked_unknown" => "dedicated_blocked_unknown",
                _ if projection.lifecycle == RunLifecycle::Paused => "dedicated_paused",
                _ => "dedicated_reader",
            }
        };
        let access = if !dedicated || writer_state == "none" {
            "read"
        } else {
            match writer_state {
                "active" => "write",
                "blocked_unknown" => "unknown",
                _ => "transitioning",
            }
        };
        let writer_generation = (owns_writer && self.writer.writer_generation > 0)
            .then_some(self.writer.writer_generation);
        let physical_server_epoch = (!threadless).then_some(self.server_epoch());
        let observed_process_count = self.runtime_record.as_ref().and_then(|record| {
            DarwinSystem
                .process_group_pids(record.identity.process_group_id)
                .ok()
                .and_then(|members| u64::try_from(members.len()).ok())
        });
        let lane_state = if dedicated && threadless {
            "absent"
        } else if !dedicated {
            "unverified"
        } else {
            match identity_verdict {
                "Match" => "ready",
                "Absent" => "absent",
                _ => "unverified",
            }
        };
        let lane_kind = manifest.execution_lane.as_str();
        let lane_id = dedicated.then_some(manifest.run_id);
        let lineage = manifest.write_continuation_provenance.as_ref().map_or_else(
            || {
                manifest
                    .fork_provenance
                    .as_ref()
                    .map_or(Value::Null, |value| json!(value))
            },
            |value| json!(value),
        );
        let mut recovery = recovery_value(manifest, lifecycle);
        if self
            .runtime_record
            .as_ref()
            .is_some_and(|record| record.control_recovery_required)
        {
            recovery["status"] = json!("recovery_required");
            recovery["reason"] = json!("control_socket_recovery_failed");
            recovery["safe_actions"] =
                json!(["run.status", "run.events", "run.recover", "run.reconcile"]);
            recovery["unsafe_actions"] =
                json!(["run.send", "run.submit", "run.respond", "run.interrupt"]);
        }
        json!({
            "workspace_id": manifest.workspace_id,
            "run_id": manifest.run_id,
            "state": lifecycle,
            "state_variant": state_variant,
            "control_mode": manifest.control_mode.as_str(),
            "purpose": manifest.purpose,
            "execution_lane": manifest.execution_lane.as_str(),
            "effective_policy": {
                "access": access,
                "verification": "verified",
                "policy_epoch": if dedicated { self.writer.authority_revision } else { 0 },
                "thread_generation": u64::from(projection.thread_id.is_some()),
                "server_epoch": if dedicated { json!(physical_server_epoch) } else { json!(self.server_epoch()) },
                "writer_generation": writer_generation,
            },
            "writer_authority": {
                "state": writer_state,
                "writer_generation": if owns_writer { self.writer.writer_generation } else { 0 },
                "transaction_id": if owns_writer { json!(self.writer.transaction_id) } else { Value::Null },
                "reconciliation_action": if owns_writer { json!(self.writer.recovery_action) } else { Value::Null },
            },
            "server_lane": {
                "kind": lane_kind,
                "lane_id": lane_id,
                "process_generation": if dedicated && !threadless { json!(projection.run_generation.max(1)) } else { Value::Null },
                "server_epoch": if dedicated { json!(physical_server_epoch) } else { json!(self.server_epoch()) },
                "state": lane_state,
                "socket_identity_sha256": Value::Null,
            },
            "workload_background_state": {
                "state": if dedicated { "unverified" } else { "not_applicable" },
                "mechanism": if dedicated { "dedicated_lane_process_census" } else { "shared_profile_aggregate" },
                "census_revision": if dedicated && observed_process_count.is_some() { json!(self.writer.authority_revision) } else { Value::Null },
                "observed_process_count": observed_process_count,
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
            "lineage": lineage,
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
            "recovery": recovery,
            // Observed evidence or nothing: the durable projection records
            // which Turn was last active, not its response, usage, or measured
            // changes, so a verb that did not read a terminal publishes null
            // rather than a fabricated `turn`.
            "last_terminal": last_terminal,
        })
    }
}

pub(crate) fn writer_run_status(state_root: &Path, run_id: Uuid) -> Result<Value, MachineError> {
    let sources = RunSources::load(state_root, run_id, 0)?;
    let verdict = sources.runtime_record.as_ref().map_or("Absent", |record| {
        match crate::worker::classify_worker_identity(record) {
            crate::worker::ProcessIdentityVerdict::Match => "Match",
            crate::worker::ProcessIdentityVerdict::Absent => "Absent",
            crate::worker::ProcessIdentityVerdict::Mismatch => "Mismatch",
            crate::worker::ProcessIdentityVerdict::Unverifiable => "Unverifiable",
        }
    });
    Ok(sources.run_value(verdict, Value::Null))
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
        ControlResponseV1::Accepted { accepted }
        | ControlResponseV1::AcceptedReceipt { accepted, .. } => (
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
        normalized_request_sha256: None,
        write: switch(args, "--write"),
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
        std::io::Read::take(
            std::io::stdin(),
            (MAX_RESPONSE_BYTES as u64).saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument("stdin", "response read failed"))?;
        reject_oversized_response(args, &bytes)?;
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
    std::io::Read::take(&mut file, (MAX_RESPONSE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument("--response-fd", "response read failed"))?;
    reject_oversized_response(args, &bytes)?;
    decode_response(&bytes, "--response-fd")
}

fn reject_oversized_response(args: &[OsString], bytes: &[u8]) -> Result<(), MachineError> {
    if bytes.len() <= MAX_RESPONSE_BYTES {
        return Ok(());
    }
    Err(crate::interaction::response_too_large(
        positional_run_id(args)?,
        &required(args, "--request-id")?,
        bytes.len(),
        "interaction response exceeds the 1 MiB input limit",
    ))
}

/// One bounded body, canonicalized before it is allowed to travel further.
pub(crate) fn decode_response(bytes: &[u8], argument: &str) -> Result<Value, MachineError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not UTF-8"))?;
    let parsed = parse(text)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))?;
    let canonical = canonicalize(&parsed)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))?;
    serde_json::from_slice(&canonical)
        .map_err(|_| MachineError::invalid_argument(argument, "response is not JSON"))
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

fn reviewer_effort(
    profile: &str,
    configured: Option<&Value>,
    efforts: &[String],
) -> Result<String, MachineError> {
    match configured {
        None | Some(Value::Null) => Ok(default_effort(efforts)),
        Some(Value::String(effort)) if efforts.contains(effort) => Ok(effort.clone()),
        Some(value) => Err(compatibility_rejected(
            profile,
            "reasoning_effort",
            json!(efforts),
            value.clone(),
            "configured Reviewer effort is not advertised by the selected model",
        )),
    }
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
    global_profile_binding: &GlobalProfileBinding,
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
        "global_profile_binding_sha256": global_profile_binding.digest()?,
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
    global_profile_binding: GlobalProfileBinding,
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
        schema_version: 2,
        runtime_profile: profile.profile_name.clone(),
        runtime_profile_snapshot_sha256: global_profile_binding.digest()?,
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
        schema_version: 2,
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
        write_continuation_provenance: None,
        aggregate_binding: None,
        audit: AuditPolicy::default(),
        compatibility: CompatibilityVerdict::Accepted,
        profile,
        global_profile_binding: Some(global_profile_binding),
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
    startup_range: crate::worker::StartupLockFile,
) -> Result<crate::worker::StartedRun, MachineError> {
    let record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), start.run_id)
            .map_err(|error| error.machine_error(start.run_id, state_root))?;
    crate::worker::start_run_worker_with_startup_range(state_root, start, startup_range)
        .map_err(|error| error.machine_error(start.run_id, &record))
}

#[cfg(target_os = "macos")]
fn fenced_worker_startup_range(
    binding: &GlobalProfileBinding,
    state: &ServerState,
    workspace_id: &str,
    state_root: &Path,
    run_id: Uuid,
    run_generation: u64,
) -> Result<crate::worker::StartupLockFile, MachineError> {
    let uid = DarwinSystem.current_uid();
    let runtime = crate::worker::runtime_root(state_root);
    crate::worker::prepare_runtime_root(&runtime, uid)
        .map_err(|error| error.machine_error(run_id, &runtime))?;
    let startup_path = crate::worker::startup_lock_path(&runtime, run_id);
    let startup_range = crate::worker::StartupLockFile::open(&startup_path, uid)
        .map_err(|error| error.machine_error(run_id, &startup_path))?;
    startup_range
        .hold_startup_range(crate::worker::STARTUP_BOUND_TIMEOUT)
        .map_err(|error| worker_startup_range_error(error, run_id, &startup_path))?;
    let owner = crate::worker::current_startup_owner(workspace_id, run_id, run_generation)
        .and_then(|owner| startup_range.write_owner(&owner));
    if let Err(error) = owner {
        let _ = startup_range.release_startup_range();
        return Err(worker_startup_range_error(error, run_id, &startup_path));
    }
    if let Err(error) =
        crate::profile::fence_global_run_worker_start(binding, state, workspace_id, run_id)
    {
        let _ = startup_range.release_startup_range();
        return Err(error);
    }
    Ok(startup_range)
}

#[cfg(target_os = "macos")]
fn worker_startup_range_error(
    error: crate::worker::WorkerProtocolError,
    run_id: Uuid,
    startup_path: &Path,
) -> MachineError {
    match error {
        crate::worker::WorkerProtocolError::StartupBusy => crate::controller::run_busy(
            run_id,
            "startup",
            "another operation holds this run's startup lock",
        ),
        error => error.machine_error(run_id, startup_path),
    }
}

#[cfg(not(target_os = "macos"))]
fn fenced_worker_startup_range(
    _binding: &GlobalProfileBinding,
    _state: &ServerState,
    _workspace_id: &str,
    _state_root: &Path,
    run_id: Uuid,
    _run_generation: u64,
) -> Result<crate::worker::StartupLockFile, MachineError> {
    Err(MachineError::new(
        "INTERNAL_ERROR",
        "per-Run workers are supported on macOS only",
        false,
        json!({"invariant": format!("worker start is unavailable for {run_id}")}),
    ))
}

#[cfg(target_os = "macos")]
fn ensure_run_worker(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
) -> Result<u64, MachineError> {
    let mut startup = Some(RunStartupLock::open(state_root, run_id)?);
    startup.as_ref().expect("startup lock").acquire()?;
    let result = ensure_run_worker_locked(view, state_root, run_id, &mut startup);
    if let Some(startup) = startup {
        let release = startup
            .lock
            .release_startup_range()
            .map_err(|error| worker_startup_range_error(error, run_id, startup.lock.path()));
        if result.is_ok() {
            release?;
        }
    }
    result
}

#[cfg(target_os = "macos")]
fn ensure_run_worker_locked(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    startup: &mut Option<RunStartupLock>,
) -> Result<u64, MachineError> {
    let uid = DarwinSystem.current_uid();
    let runtime_record =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    if runtime_record.exists() {
        let record = crate::worker::read_runtime_record(&runtime_record, uid)
            .map_err(|error| error.machine_error(run_id, &runtime_record))?;
        if record.control_recovery_required {
            return Err(
                crate::worker::WorkerProtocolError::ControlRecoveryRequired {
                    generation: record.identity.run_generation,
                }
                .machine_error(run_id, &runtime_record),
            );
        }
        match crate::worker::classify_worker_identity(&record) {
            crate::worker::ProcessIdentityVerdict::Match => {
                return Ok(record.control_socket_epoch);
            }
            crate::worker::ProcessIdentityVerdict::Absent => {
                crate::worker::remove_verified_absent_runtime(&runtime_record, uid)
                    .map_err(|error| error.machine_error(run_id, &runtime_record))?;
            }
            verdict => {
                return Err(MachineError::new(
                    "RECOVERY_REQUIRED",
                    "the prior worker generation cannot be safely replaced",
                    false,
                    json!({
                        "run_id": run_id,
                        "generation": record.identity.run_generation,
                        "identity_verdict": verdict.as_str(),
                        "reason": "worker_identity_does_not_authorize_replacement",
                    }),
                ));
            }
        }
    }

    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    let manifest = store.load_manifest(run_id)?;
    let projection = store.load_state_projection(run_id)?;
    ensure_worker_spawn_lifecycle(run_id, projection.lifecycle)?;
    let run_generation = projection.run_generation.saturating_add(1).max(1);
    let startup_file = &startup.as_ref().expect("startup lock").lock;
    crate::worker::current_startup_owner(&view.workspace_id, run_id, run_generation)
        .and_then(|owner| startup_file.write_owner(&owner))
        .map_err(|error| worker_startup_range_error(error, run_id, startup_file.path()))?;
    let global_binding = manifest.global_profile_binding.as_ref().ok_or_else(|| {
        MachineError::new(
            "LEGACY_STATE_UNSUPPORTED",
            "legacy Run recovery is unavailable after the global Profile cutover",
            false,
            json!({"run_id": run_id}),
        )
    })?;
    global_binding.validate_for_recovery()?;
    let state = crate::profile::ensure_global_server(global_binding)?;
    ensure_global_run_membership(
        &DolgoraeHome::system()?,
        &view.workspace_id,
        global_binding,
        run_id,
        &state,
    )?;
    if state.snapshot.server_key != manifest.profile.initial_server_key {
        return Err(MachineError::new(
            "PROFILE_SERVER_EPOCH_MISMATCH",
            "the run's pinned profile contract is not the active contract",
            false,
            json!({
                "run_id": run_id,
                "expected_server_key": manifest.profile.initial_server_key,
                "expected_server_epoch": manifest.profile_capability_snapshot.server_epoch,
                "actual_server_key": state.snapshot.server_key,
                "actual_server_epoch": state.server_epoch,
            }),
        ));
    }
    let efforts = advertised_efforts(&state, &manifest.profile.profile_name, &manifest.model)?;
    let writer = crate::writer::WriterStore::new(state_root, &manifest.workspace_id, uid).load()?;
    let owns_write = matches!(
        writer.state,
        crate::writer::WriterAuthorityState::Reserved | crate::writer::WriterAuthorityState::Active
    ) && writer
        .holder
        .as_ref()
        .is_some_and(|holder| holder.run_id == run_id);
    let control_socket_epoch = observed_control_socket_epoch(state_root, run_id)
        .saturating_add(1)
        .max(1);
    let workspace_path = view
        .canonical_path
        .to_path_buf()
        .map_err(|_| internal("workspace path is not representable"))?;
    let dedicated_server = if manifest.execution_lane == ExecutionLane::Dedicated {
        let socket_path = crate::worker::dedicated_server_socket_path(uid, run_id, run_generation)
            .map_err(|error| error.machine_error(run_id, state_root))?;
        let server_epoch = crate::writer::WriterStore::new(state_root, &manifest.workspace_id, uid)
            .allocate_server_epoch(state.server_epoch)?;
        Some(DedicatedServerBootstrap {
            socket_path,
            argv: manifest.profile.normalized_argv.clone(),
            cwd: PathBuf::from(&manifest.profile.derived_launch_cwd),
            environment: manifest.profile.sanitized_environment.clone(),
            log_path: state_root
                .join("runs")
                .join(run_id.to_string())
                .join(format!("dedicated-server-{run_generation}.log")),
            executable_device: manifest.profile.executable_identity.device,
            executable_inode: manifest.profile.executable_identity.inode,
            executable_sha256: manifest.profile.executable_identity.sha256.clone(),
            server_epoch,
        })
    } else {
        None
    };
    let app_server_socket = dedicated_server.as_ref().map_or_else(
        || PathBuf::from(&state.socket_path),
        |server| server.socket_path.clone(),
    );
    let server_epoch = dedicated_server
        .as_ref()
        .map_or(state.server_epoch, |server| server.server_epoch);
    let mut session_cwd = workspace_path;
    let mut isolated_write = false;
    if let Some(binding) = manifest.aggregate_binding.as_ref()
        && binding.aggregate_kind == AggregateKind::ExternalSpecialistEngagement
    {
        let engagement =
            EngagementStore::open(&EngagementStore::workspace_database_path(state_root))?;
        let (_, access, member_state) =
            engagement.external_member_configuration(binding.aggregate_id, run_id)?;
        if !matches!(
            member_state.as_str(),
            "provisioning" | "active" | "degraded"
        ) {
            return Err(MachineError::new(
                "ENGAGEMENT_STATE_CONFLICT",
                "external Specialist membership is not recoverable",
                false,
                json!({"run_id":run_id,"membership_state":member_state}),
            ));
        }
        if access == "isolated_write" {
            session_cwd = isolated_specialist_root(state_root, binding.aggregate_id, run_id);
            if verify_secure_directory(&session_cwd, uid).is_err() {
                return Err(MachineError::new(
                    "RECOVERY_REQUIRED",
                    "the isolated Specialist worktree is unavailable",
                    false,
                    json!({"run_id":run_id,"required_action":"restore_isolated_worktree"}),
                ));
            }
            isolated_write = true;
        }
    }
    let session = WorkerSessionBootstrap {
        app_server_socket,
        canonical_codex_home: state.snapshot.canonical_codex_home.clone(),
        server_key: state.snapshot.server_key.clone(),
        server_epoch,
        controller_id: manifest.controller.identity.controller_id,
        control_mode: manifest.control_mode.as_str().to_owned(),
        fixed_model: manifest.model.clone(),
        default_effort: projection
            .default_effort
            .clone()
            .unwrap_or_else(|| manifest.default_reasoning_effort.clone()),
        supported_efforts: efforts,
        cwd: session_cwd,
        developer_instructions: manifest.agent_configuration.normalized_instructions.clone(),
        sandbox: if owns_write || isolated_write {
            "workspace-write"
        } else {
            "read-only"
        }
        .to_owned(),
        approval_policy: if owns_write || isolated_write {
            "on-request"
        } else {
            "never"
        }
        .to_owned(),
        safety_policy: crate::turn::SessionSafetyPolicy::Standard,
        artifact_root: run_root(state_root, run_id).join("artifacts"),
        attach: match &manifest.fork_provenance {
            Some(provenance) if provenance.mode == "history_copy" => SessionAttach::Fork {
                source_thread_id: provenance
                    .source_thread_id
                    .clone()
                    .ok_or_else(|| internal("history-copy fork lacks source thread"))?,
                last_turn_id: provenance
                    .source_turn_id
                    .clone()
                    .ok_or_else(|| internal("history-copy fork lacks source turn"))?,
            },
            _ => projection
                .thread_id
                .clone()
                .map_or(SessionAttach::Start, |thread_id| SessionAttach::Resume {
                    thread_id,
                }),
        },
        transport_timeout_seconds: 900,
        dedicated_server,
    };
    crate::profile::fence_global_run_worker_start(
        global_binding,
        &state,
        &view.workspace_id,
        run_id,
    )?;
    let RunStartupLock {
        lock: startup_range,
        _process_guard,
        ..
    } = startup.take().expect("startup lock transfers once");
    let start = start_worker(
        state_root,
        &crate::worker::RunWorkerStart {
            workspace_id: manifest.workspace_id,
            run_id,
            run_generation,
            ledger_root: state_root.join("runs").join(run_id.to_string()),
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: crate::worker::WORKER_PROTOCOL_VERSION,
            control_socket_epoch,
            profile: manifest.profile.profile_name,
            session: Some(session),
        },
        startup_range,
    );
    if let Err(error) = start {
        if manifest.execution_lane == ExecutionLane::Dedicated {
            return Err(MachineError::new(
                "DEDICATED_SERVER_START_FAILED",
                "the run-owned dedicated app-server generation did not become ready",
                true,
                json!({
                    "run_id": run_id,
                    "execution_lane": "dedicated",
                    "reason": error.message,
                    "required_action": "retry_dedicated_start",
                }),
            ));
        }
        return Err(error);
    }
    Ok(control_socket_epoch)
}

pub(crate) fn ensure_external_specialist_worker(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
) -> Result<u64, MachineError> {
    ensure_run_worker(view, state_root, run_id)
}

#[cfg(not(target_os = "macos"))]
fn ensure_run_worker(
    _view: &WorkspaceView,
    _state_root: &Path,
    run_id: Uuid,
) -> Result<u64, MachineError> {
    Err(MachineError::new(
        "INTERNAL_ERROR",
        "per-Run workers are supported on macOS only",
        false,
        json!({"invariant": format!("worker start is unavailable for {run_id}")}),
    ))
}

#[cfg(not(target_os = "macos"))]
fn start_worker(
    _state_root: &Path,
    _start: &crate::worker::RunWorkerStart,
    _startup_range: crate::worker::StartupLockFile,
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
    Ok(DolgoraeHome::system()?.workspace_root(&view.workspace_id))
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
        "--offset",
        "--length",
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

fn positional_interaction_id(args: &[OsString]) -> Result<Uuid, MachineError> {
    let Some(value) = positionals(args).get(1).copied() else {
        return Err(MachineError::invalid_argument(
            "request-id",
            "interaction get requires both run and request ids",
        ));
    };
    value
        .to_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| value.get_version_num() == 7)
        .ok_or_else(|| MachineError::invalid_argument("request-id", "request id must be a UUIDv7"))
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

static PROCESS_RUN_STARTUPS: Mutex<BTreeSet<(PathBuf, Uuid)>> = Mutex::new(BTreeSet::new());

/// POSIX locks do not serialize threads; sibling opens/closes also drop them.
/// Keep only active claims and reject contention before opening another fd.
struct ProcessRunStartup {
    key: (PathBuf, Uuid),
}

impl ProcessRunStartup {
    fn acquire(state_root: &Path, run_id: Uuid) -> Result<Self, MachineError> {
        let key = (state_root.to_owned(), run_id);
        let mut active = PROCESS_RUN_STARTUPS
            .lock()
            .map_err(|_| internal("process startup serializer is poisoned"))?;
        if !active.insert(key.clone()) {
            return Err(crate::controller::run_busy(
                run_id,
                "startup",
                "another operation in this process is preparing the Run",
            ));
        }
        Ok(Self { key })
    }
}

impl Drop for ProcessRunStartup {
    fn drop(&mut self) {
        if let Ok(mut active) = PROCESS_RUN_STARTUPS.lock() {
            active.remove(&self.key);
        }
    }
}

/// The Run startup/mutation lock as a file range, for the resetting CLI.
///
/// One descriptor is opened for the whole reset and kept alive across PREPARE,
/// APPLY, and COMMIT: POSIX record locks are per process, so closing any
/// descriptor for this file would drop every range this process holds on it,
/// including the one the next phase depends on.
struct RunStartupLock {
    // The fd must close before releasing the process claim.
    lock: crate::worker::StartupLockFile,
    run_id: Uuid,
    _process_guard: ProcessRunStartup,
}

impl RunStartupLock {
    fn open(state_root: &Path, run_id: Uuid) -> Result<Self, MachineError> {
        let process_guard = ProcessRunStartup::acquire(state_root, run_id)?;
        let uid = DarwinSystem.current_uid();
        let runtime = crate::worker::runtime_root(state_root);
        crate::worker::prepare_runtime_root(&runtime, uid)
            .map_err(|error| worker_startup_range_error(error, run_id, &runtime))?;
        let path = crate::worker::startup_lock_path(&runtime, run_id);
        let lock = crate::worker::StartupLockFile::open(&path, uid)
            .map_err(|error| worker_startup_range_error(error, run_id, &path))?;
        Ok(Self {
            lock,
            run_id,
            _process_guard: process_guard,
        })
    }
}

impl RunMutationLock for RunStartupLock {
    fn acquire(&self) -> Result<(), MachineError> {
        // The normative ten-second startup contention budget: a reset waits
        // for the range exactly as long as a start attempt does, and reports
        // the same registered busy answer when it loses.
        self.lock
            .hold_startup_range(crate::worker::STARTUP_BOUND_TIMEOUT)
            .map_err(|error| worker_startup_range_error(error, self.run_id, self.lock.path()))
    }

    fn release(&self) {
        let _ = self.lock.release_startup_range();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn checked_interaction_rejects_an_oversized_payload() {
        let run_id = Uuid::now_v7();
        let request_id = Uuid::now_v7();
        let interaction = json!({
            "schema_version": 1,
            "request_id": request_id,
            "run_id": run_id,
            "controller_id": Uuid::now_v7(),
            "control_mode": "managed_agent",
            "thread_id": "thread",
            "turn_id": "turn",
            "item_id": null,
            "run_generation": 1,
            "server_epoch": 1,
            "kind": "user_input",
            "status": "pending",
            "payload": {"question": "x".repeat(crate::interaction::MAX_PAYLOAD_BYTES)},
            "available_decisions": [],
            "response_schema": "dolgorae.interaction.user-input/v1",
            "opened_at": "2026-09-09T00:00:00.000000Z",
            "resolved_at": null,
            "resolution": null
        });
        let error = checked_interaction(&[(interaction, 1)], run_id, request_id).unwrap_err();
        assert_eq!(error.code, "INTERACTION_PAYLOAD_TOO_LARGE");
        assert_eq!(error.details["run_id"], json!(run_id));
        assert_eq!(error.details["request_id"], json!(request_id));
    }

    #[test]
    fn checked_interaction_rejects_invalid_typed_payload_and_unsafe_generations() {
        let run_id = Uuid::now_v7();
        let request_id = Uuid::now_v7();
        let mut interaction = json!({
            "schema_version": 1,
            "request_id": request_id,
            "run_id": run_id,
            "controller_id": Uuid::now_v7(),
            "control_mode": "managed_agent",
            "thread_id": "thread",
            "turn_id": "turn",
            "item_id": null,
            "run_generation": 1,
            "server_epoch": 1,
            "kind": "command_execution_approval",
            "status": "pending",
            "payload": {
                "title": "x".repeat(513),
                "message": "message",
                "command": ["true"],
                "cwd": "/tmp",
                "reason": null
            },
            "available_decisions": ["accept_once", "decline", "cancel"],
            "response_schema": "dolgorae.interaction.command-approval/v1",
            "opened_at": "2026-09-09T00:00:00.000000Z",
            "resolved_at": null,
            "resolution": null
        });
        let error =
            checked_interaction(&[(interaction.clone(), 1)], run_id, request_id).unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(error.details["invariant"], "interaction UTF-8 byte bound");

        interaction["payload"]["title"] = json!("title");
        for field in ["run_generation", "server_epoch"] {
            interaction[field] = json!(crate::domain::MAX_JCS_SAFE_INTEGER + 1);
            let error =
                checked_interaction(&[(interaction.clone(), 1)], run_id, request_id).unwrap_err();
            assert_eq!(error.code, "INTERNAL_ERROR");
            assert_eq!(
                error.details["invariant"],
                "normalized interaction identity and state"
            );
            interaction[field] = json!(1);
        }
    }

    #[test]
    fn reviewer_honors_explicit_effort_without_substitution() {
        let efforts = vec!["medium".to_owned(), "low".to_owned()];
        assert_eq!(
            reviewer_effort("reviewer", Some(&json!("low")), &efforts).unwrap(),
            "low"
        );
        assert_eq!(
            reviewer_effort("reviewer", None, &efforts).unwrap(),
            "medium"
        );
        assert_eq!(
            reviewer_effort("reviewer", Some(&Value::Null), &efforts).unwrap(),
            "medium"
        );
        assert_eq!(
            reviewer_effort("reviewer", Some(&json!("high")), &efforts)
                .unwrap_err()
                .code,
            "COMPATIBILITY_REJECTED"
        );
    }

    #[test]
    fn continuation_requires_the_named_recorded_verdict_and_stable_request_identity() {
        use crate::run::CapabilityState;
        for (reason, expected) in [
            (
                "access_transition_unavailable",
                CapabilityState::Unavailable,
            ),
            ("access_transition_unverified", CapabilityState::Unverified),
        ] {
            assert!(!continuation_reason_matches(
                ExecutionLane::Dedicated,
                None,
                reason
            ));
            assert!(!continuation_reason_matches(
                ExecutionLane::Dedicated,
                Some(&CapabilityState::Supported),
                reason
            ));
            assert!(continuation_reason_matches(
                ExecutionLane::Dedicated,
                Some(&expected),
                reason
            ));
            assert!(!continuation_reason_matches(
                ExecutionLane::SharedReadonly,
                Some(&expected),
                reason
            ));
        }
        assert!(continuation_reason_matches(
            ExecutionLane::SharedReadonly,
            None,
            "shared_readonly_source"
        ));
        let mut provenance = WriteContinuationProvenance {
            source_run_id: Uuid::now_v7(),
            source_turn_id: "turn".to_owned(),
            source_thread_id: "thread".to_owned(),
            creation_reason: "shared_readonly_source".to_owned(),
            source_controller_kind: "human_cli".to_owned(),
            destination_controller_kind: "human_cli".to_owned(),
            handoff_summary_sha256: None,
            artifact_refs: vec![],
            workspace_baseline_sha256: "a".repeat(64),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let expected = continuation_request_identity(&provenance).unwrap();
        provenance.created_at = "2026-01-02T00:00:00Z".to_owned();
        assert_eq!(
            continuation_request_identity(&provenance).unwrap(),
            expected
        );
        provenance.source_turn_id = "different".to_owned();
        assert_ne!(
            continuation_request_identity(&provenance).unwrap(),
            expected
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_quiesce_classifies_every_durable_worker_outcome() {
        assert_eq!(
            profile_quiesce_outcome(true, RunLifecycle::Idle),
            ("terminal_observed", false)
        );
        assert_eq!(
            profile_quiesce_outcome(true, RunLifecycle::Running),
            ("outcome_unknown", true)
        );
        assert_eq!(
            profile_quiesce_outcome(true, RunLifecycle::OutcomeUnknown),
            ("outcome_unknown", false)
        );
        assert_eq!(
            profile_quiesce_outcome(false, RunLifecycle::Idle),
            ("no_active_turn", false)
        );
        assert_eq!(
            profile_quiesce_outcome(false, RunLifecycle::OutcomeUnknown),
            ("outcome_unknown", false)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn profile_quiesce_resumes_the_matching_ledger_observation() {
        let operation_id = Uuid::now_v7().to_string();
        let observations = vec![
            json!({"operation_id": Uuid::now_v7(), "outcome": "terminal_observed"}),
            json!({"operation_id": operation_id.clone(), "outcome": "outcome_unknown"}),
        ];
        assert_eq!(
            recorded_profile_observation_outcome(&observations, &operation_id).unwrap(),
            Some("outcome_unknown".to_owned())
        );
        assert_eq!(
            recorded_profile_observation_outcome(&observations, &Uuid::now_v7().to_string())
                .unwrap(),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn worker_startup_fence_preserves_busy_and_identity_collision_errors() {
        let run_id = Uuid::now_v7();
        let path = Path::new("/tmp/startup.lock");
        let busy = worker_startup_range_error(
            crate::worker::WorkerProtocolError::StartupBusy,
            run_id,
            path,
        );
        assert_eq!(busy.code, "RUN_BUSY");
        assert!(busy.retryable);

        let collision = worker_startup_range_error(
            crate::worker::WorkerProtocolError::SocketIdentityMismatch,
            run_id,
            path,
        );
        assert_eq!(collision.code, "RUNTIME_PATH_COLLISION");
        assert!(!collision.retryable);
    }

    fn writer_test_store(label: &str) -> (PathBuf, crate::writer::WriterStore) {
        let root = std::env::temp_dir().join(format!("dlg-writer-{label}-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.join("runtime")).unwrap();
        std::fs::set_permissions(root.join("runtime"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::create_dir(root.join("runtime/locks")).unwrap();
        std::fs::set_permissions(
            root.join("runtime/locks"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let workspace_id = "9".repeat(64);
        let uid = DarwinSystem.current_uid();
        crate::writer::WriterStore::initialize_layout(&root, &workspace_id, uid).unwrap();
        let store = crate::writer::WriterStore::new(&root, workspace_id, uid);
        (root, store)
    }

    fn writer_test_holder(run_id: Uuid) -> crate::writer::WriterHolder {
        crate::writer::WriterHolder {
            run_id,
            profile: "writer".to_owned(),
            controller_id: Uuid::now_v7(),
            controller_generation: 1,
            run_generation: 1,
            worker_generation: 1,
            profile_server_key: "0".repeat(64),
            profile_server_epoch: 1,
            thread_id: Some("thread".to_owned()),
            lifecycle: RunLifecycle::Idle,
            active_turn_id: None,
            pending_interaction_count: 0,
            last_event_cursor: None,
        }
    }

    fn activate_test_writer(store: &crate::writer::WriterStore, run_id: Uuid) {
        let transaction_id = Uuid::now_v7();
        store
            .transact(|record| {
                let (_, generation) =
                    record.prepare_acquire(run_id, writer_test_holder(run_id), transaction_id)?;
                record.commit_acquire(transaction_id, generation)
            })
            .unwrap();
    }

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
        assert_eq!(value["features"]["persistent_runs"], true);
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

    /// SPEC-014: a shared Run "MUST NOT acquire workspace writer authority or
    /// be promoted in place."  Both sides are checked because handoff retires
    /// one writer before granting authority to the other Run.
    #[test]
    fn writer_handoff_requires_two_dedicated_runs() {
        let source = Uuid::now_v7();
        let destination = Uuid::now_v7();
        assert!(
            ensure_writer_handoff_lanes(
                source,
                ExecutionLane::Dedicated,
                destination,
                ExecutionLane::Dedicated,
            )
            .is_ok()
        );

        for (source_lane, destination_lane, expected_blockers) in [
            (
                ExecutionLane::SharedReadonly,
                ExecutionLane::Dedicated,
                vec!["source_run_not_dedicated"],
            ),
            (
                ExecutionLane::Dedicated,
                ExecutionLane::SharedReadonly,
                vec!["destination_run_not_dedicated"],
            ),
            (
                ExecutionLane::SharedReadonly,
                ExecutionLane::SharedReadonly,
                vec!["source_run_not_dedicated", "destination_run_not_dedicated"],
            ),
        ] {
            let error =
                ensure_writer_handoff_lanes(source, source_lane, destination, destination_lane)
                    .unwrap_err();
            assert_eq!(error.code, "WRITER_HANDOFF_NOT_ALLOWED");
            assert_eq!(error.details["source_run_id"], source.to_string());
            assert_eq!(error.details["destination_run_id"], destination.to_string());
            assert_eq!(error.details["blockers"], json!(expected_blockers));
        }
    }

    /// SPEC-007: missing runtime identity is not worker-absence evidence. A
    /// reset must retain `blocked_unknown` until it can run the complete
    /// recorded-generation identity and process census proof.
    #[test]
    fn writer_reset_rejects_missing_recorded_worker_evidence() {
        let run_id = Uuid::now_v7();
        let holder = crate::writer::WriterHolder {
            run_id,
            profile: "writer".to_owned(),
            controller_id: Uuid::now_v7(),
            controller_generation: 1,
            run_generation: 1,
            worker_generation: 1,
            profile_server_key: "0".repeat(64),
            profile_server_epoch: 1,
            thread_id: None,
            lifecycle: RunLifecycle::OutcomeUnknown,
            active_turn_id: None,
            pending_interaction_count: 0,
            last_event_cursor: None,
        };
        let state_root = std::env::temp_dir().join(format!("dlg-reset-missing-{run_id}"));

        let error = prove_recorded_writer_absent(&state_root, &holder, DarwinSystem.current_uid())
            .unwrap_err();

        assert_eq!(error.code, "RECOVERY_REQUIRED");
        assert!(!error.retryable);
        assert_eq!(error.details["run_id"], run_id.to_string());
        assert_eq!(error.details["generation"], 1);
        assert_eq!(error.details["identity_verdict"], "unverifiable");
        assert_eq!(
            error.details["reason"],
            "worker_or_group_absence_unverified"
        );
    }

    #[test]
    fn pre_effect_census_failure_restores_writer_states_for_explicit_retry() {
        let (acquire_root, acquire_store) = writer_test_store("acquire-rollback");
        let acquire_run = Uuid::now_v7();
        let acquire_transaction = Uuid::now_v7();
        acquire_store
            .transact(|record| {
                record
                    .prepare_acquire(
                        acquire_run,
                        writer_test_holder(acquire_run),
                        acquire_transaction,
                    )
                    .map(|_| ())
            })
            .unwrap();
        let proof = prove_writer_pre_effect_absence(
            &acquire_store,
            acquire_transaction,
            WriterPreEffectTransition::Acquire,
            || Err(crate::worker::WorkerProtocolError::InvalidIdentity),
        )
        .unwrap();
        assert!(proof.is_err());
        assert_eq!(
            acquire_store.load().unwrap().state,
            crate::writer::WriterAuthorityState::None
        );
        acquire_store
            .transact(|record| {
                record
                    .prepare_acquire(acquire_run, writer_test_holder(acquire_run), Uuid::now_v7())
                    .map(|_| ())
            })
            .unwrap();
        std::fs::remove_dir_all(acquire_root).unwrap();

        let (release_root, release_store) = writer_test_store("release-rollback");
        let release_run = Uuid::now_v7();
        activate_test_writer(&release_store, release_run);
        let release_transaction = Uuid::now_v7();
        release_store
            .transact(|record| {
                record
                    .prepare_release(release_run, release_transaction)
                    .map(|_| ())
            })
            .unwrap();
        let proof = prove_writer_pre_effect_absence(
            &release_store,
            release_transaction,
            WriterPreEffectTransition::Release,
            || Err(crate::worker::WorkerProtocolError::InvalidIdentity),
        )
        .unwrap();
        assert!(proof.is_err());
        assert_eq!(
            release_store.load().unwrap().state,
            crate::writer::WriterAuthorityState::Active
        );
        release_store
            .transact(|record| {
                record
                    .prepare_release(release_run, Uuid::now_v7())
                    .map(|_| ())
            })
            .unwrap();
        std::fs::remove_dir_all(release_root).unwrap();
    }

    #[test]
    fn only_external_closed_run_with_absent_runtime_can_release_directly() {
        let (closed_root, closed_store) = writer_test_store("closed-release");
        let closed_run = Uuid::now_v7();
        activate_test_writer(&closed_store, closed_run);
        let closed_transaction = Uuid::now_v7();
        closed_store
            .transact(|record| {
                record
                    .prepare_release(closed_run, closed_transaction)
                    .map(|_| ())
            })
            .unwrap();
        assert!(
            commit_external_closed_writer_release_if_runtime_absent(
                &closed_store,
                closed_transaction,
                &closed_root.join("missing-runtime-record"),
                true,
            )
            .unwrap()
        );
        assert_eq!(
            closed_store.load().unwrap().state,
            crate::writer::WriterAuthorityState::None
        );
        std::fs::remove_dir_all(closed_root).unwrap();

        let (ordinary_root, ordinary_store) = writer_test_store("ordinary-release");
        let ordinary_run = Uuid::now_v7();
        activate_test_writer(&ordinary_store, ordinary_run);
        let ordinary_transaction = Uuid::now_v7();
        ordinary_store
            .transact(|record| {
                record
                    .prepare_release(ordinary_run, ordinary_transaction)
                    .map(|_| ())
            })
            .unwrap();
        assert!(
            !commit_external_closed_writer_release_if_runtime_absent(
                &ordinary_store,
                ordinary_transaction,
                &ordinary_root.join("missing-runtime-record"),
                false,
            )
            .unwrap()
        );
        assert_eq!(
            ordinary_store.load().unwrap().state,
            crate::writer::WriterAuthorityState::Releasing
        );
        std::fs::remove_dir_all(ordinary_root).unwrap();

        let (present_root, present_store) = writer_test_store("present-release");
        let present_run = Uuid::now_v7();
        activate_test_writer(&present_store, present_run);
        let present_transaction = Uuid::now_v7();
        present_store
            .transact(|record| {
                record
                    .prepare_release(present_run, present_transaction)
                    .map(|_| ())
            })
            .unwrap();
        let present_record = present_root.join("present-runtime-record");
        std::fs::write(&present_record, b"occupied").unwrap();
        assert!(
            !commit_external_closed_writer_release_if_runtime_absent(
                &present_store,
                present_transaction,
                &present_record,
                true,
            )
            .unwrap()
        );
        assert_eq!(
            present_store.load().unwrap().state,
            crate::writer::WriterAuthorityState::Releasing
        );
        std::fs::remove_dir_all(present_root).unwrap();
    }

    #[test]
    fn resume_and_writer_acquire_refuse_uncertain_or_terminal_lifecycles() {
        assert!(resume_lifecycle_allowed(RunLifecycle::Paused));
        assert!(!resume_lifecycle_allowed(RunLifecycle::Idle));

        for lifecycle in [RunLifecycle::Idle, RunLifecycle::Paused] {
            assert!(writer_acquire_lifecycle_allowed(lifecycle));
        }
        for lifecycle in [
            RunLifecycle::Starting,
            RunLifecycle::Running,
            RunLifecycle::WaitingInteraction,
            RunLifecycle::ReconciliationRequired,
            RunLifecycle::Closed,
            RunLifecycle::StartFailed,
            RunLifecycle::OutcomeUnknown,
        ] {
            assert!(!writer_acquire_lifecycle_allowed(lifecycle));
        }
    }

    #[test]
    fn closed_run_is_refused_at_the_worker_spawn_boundary() {
        let run_id = Uuid::now_v7();
        let error = ensure_worker_spawn_lifecycle(run_id, RunLifecycle::Closed).unwrap_err();
        assert_eq!(error.code, "RUN_STATE_CONFLICT");
        assert_eq!(error.details["run_id"], run_id.to_string());
        assert_eq!(error.details["state"], "closed");
        assert_eq!(error.details["operation"], "run.ensure_worker");

        assert!(external_writer_release_can_bypass_lifecycle(
            RunLifecycle::Closed,
            true
        ));
        assert!(!external_writer_release_can_bypass_lifecycle(
            RunLifecycle::Closed,
            false
        ));
    }
}

fn require_workspace_identity(
    view: &WorkspaceView,
    expected: Option<&str>,
) -> Result<(), MachineError> {
    if expected.is_some_and(|expected| expected != view.workspace_id) {
        return Err(MachineError::invalid_argument(
            "workspace_id",
            "workspace identity no longer matches the request",
        ));
    }
    Ok(())
}

/// Called only while the writer authority or threadless startup boundary is held.
/// The writer boundary is shared with every worker durable mutation append.
fn require_mutation_revision(
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    operation: &str,
    expected_state_revision: Option<u64>,
) -> Result<u64, MachineError> {
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    let binding = store.load_controller_binding(run_id)?;
    authorize_controller(run_id, operation, &binding, carrier)?;
    let projection = store.load_state_projection(run_id)?;
    let actual = projection.ledger_head.sequence;
    if durable_pending_admission(state_root, run_id)?.is_some() {
        return Err(run_state_conflict_error(
            run_id,
            projection.lifecycle,
            operation,
        ));
    }
    if expected_state_revision.is_some_and(|expected| expected != actual) {
        return Err(run_state_conflict_error(
            run_id,
            projection.lifecycle,
            operation,
        ));
    }
    Ok(actual)
}

enum PreparedAdmission {
    Admitted(Uuid),
    Replayed(Box<ControlResponseV1>),
}

fn begin_prepared_mutation(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    verb: RunVerb,
    carrier: &CredentialCarrier,
    prepared: &Prepared,
    expected_state_revision: Option<u64>,
) -> Result<PreparedAdmission, MachineError> {
    let uid = DarwinSystem.current_uid();
    let runtime_path =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
            .map_err(|error| error.machine_error(run_id, state_root))?;
    let expected = expected_state_revision.unwrap_or(durable_head(state_root, run_id)?);
    if crate::worker::read_runtime_record(&runtime_path, uid).is_ok_and(|record| {
        crate::worker::classify_worker_identity(&record)
            == crate::worker::ProcessIdentityVerdict::Match
    }) {
        let response = crate::worker::call_run_worker(
            state_root,
            run_id,
            uid,
            Some(carrier.raw_fd()),
            |identity| {
                checked_controller_request(
                    ControlRequestV1::BeginPreparedMutation {
                        expected_state_revision: expected,
                        request: Box::new(prepared.clone().into_request(verb, identity)),
                    },
                    Some(carrier),
                )
            },
        )
        .map_err(|error| error.machine_error(run_id, &runtime_path))?;
        return match response {
            ControlResponseV1::MutationAdmitted { admission_id } => {
                Ok(PreparedAdmission::Admitted(admission_id))
            }
            ControlResponseV1::Failed {
                code,
                message,
                retryable,
                details,
            } => Err(MachineError::new(code, message, retryable, details)),
            ControlResponseV1::Rejected { code } => Err(rejected_error(&code)),
            response => Ok(PreparedAdmission::Replayed(Box::new(response))),
        };
    }
    let lock = RunStartupLock::open(state_root, run_id)?;
    lock.acquire()?;
    let result = (|| {
        if runtime_path.exists() {
            let record = crate::worker::read_runtime_record(&runtime_path, uid)
                .map_err(|error| error.machine_error(run_id, &runtime_path))?;
            if crate::worker::classify_worker_identity(&record)
                != crate::worker::ProcessIdentityVerdict::Absent
            {
                return Err(crate::controller::run_busy(
                    run_id,
                    verb.operation_name(),
                    "the prior worker must be absent before offline admission",
                ));
            }
        }
        crate::writer::WriterStore::new(state_root, &view.workspace_id, uid).observe_locked(
            |_| {
                let binding = load_reconciled_controller_binding(state_root, run_id)?;
                authorize_controller(run_id, verb.operation_name(), &binding, carrier)?;
                let mut ledger = crate::ledger::Ledger::open(run_root(state_root, run_id), run_id)
                    .map_err(|error| internal(error.to_string()))?;
                let request = prepared.normalized_request(verb);
                let bytes = canonicalize(
                    &parse(&request.to_string()).map_err(|error| internal(error.to_string()))?,
                )
                .map_err(|error| internal(error.to_string()))?;
                admit_mutation_digest(
                    &mut ledger,
                    binding.identity.controller_id,
                    expected,
                    sha256_hex(&bytes),
                    verb.operation_name(),
                    matches!(prepared, Prepared::Turn { request, .. } if request.write),
                )
                .map(|admission| PreparedAdmission::Admitted(admission.admission_id))
            },
        )?
    })();
    lock.release();
    result
}

fn require_writer_admission(
    state_root: &Path,
    run_id: Uuid,
    carrier: &CredentialCarrier,
    admission_id: Uuid,
) -> Result<(), MachineError> {
    let binding = load_reconciled_controller_binding(state_root, run_id)?;
    authorize_controller(run_id, "run.acquire_write", &binding, carrier)?;
    let head = durable_head(state_root, run_id)?;
    let ledger = crate::ledger::ObservedLedger::open(&run_root(state_root, run_id), run_id, head)
        .map_err(|error| internal(error.to_string()))?;
    let admitted = ledger
        .payloads_of_kind(AuditKind::MutationAdmitted)
        .map_err(|error| internal(error.to_string()))?;
    let completed = ledger
        .payloads_of_kind(AuditKind::MutationCompleted)
        .map_err(|error| internal(error.to_string()))?;
    let matching = writer_admission_matches(
        &admitted,
        &completed,
        admission_id,
        binding.identity.controller_id,
    );
    if !matching {
        let state =
            RunStore::new(SystemWorkspacePlatform, state_root).load_state_projection(run_id)?;
        return Err(run_state_conflict_error(
            run_id,
            state.lifecycle,
            "run.acquire_write",
        ));
    }
    Ok(())
}

fn writer_admission_matches(
    admitted: &[Value],
    completed: &[Value],
    admission_id: Uuid,
    controller_id: Uuid,
) -> bool {
    admitted.last().is_some_and(|value| {
        value.get("admission_id") == Some(&json!(admission_id))
            && value.get("controller_id") == Some(&json!(controller_id))
            && value.get("allow_writer_acquire") == Some(&Value::Bool(true))
            && matches!(
                value.get("operation").and_then(Value::as_str),
                Some("run.submit" | "run.send" | "run.acquire_write")
            )
    }) && !completed
        .iter()
        .any(|value| value.get("admission_id") == Some(&json!(admission_id)))
}

fn durable_pending_admission(
    state_root: &Path,
    run_id: Uuid,
) -> Result<Option<MutationAdmission>, MachineError> {
    let head = durable_head(state_root, run_id)?;
    let ledger = crate::ledger::ObservedLedger::open(&run_root(state_root, run_id), run_id, head)
        .map_err(|error| internal(error.to_string()))?;
    let admitted = ledger
        .payloads_of_kind(AuditKind::MutationAdmitted)
        .map_err(|error| internal(error.to_string()))?;
    let Some(last) = admitted.last() else {
        return Ok(None);
    };
    let admission: MutationAdmission =
        serde_json::from_value(last.clone()).map_err(|error| internal(error.to_string()))?;
    let completed = ledger
        .payloads_of_kind(AuditKind::MutationCompleted)
        .map_err(|error| internal(error.to_string()))?;
    if completed
        .iter()
        .any(|value| value.get("admission_id") == Some(&json!(admission.admission_id)))
    {
        return Ok(None);
    }
    Ok(Some(admission))
}

#[cfg(test)]
mod mutation_admission_tests {
    use super::*;
    use crate::mutation_admission::{MutationRequest, active_mutation_admission};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writer_admission_requires_live_writer_capable_operation_from_current_controller() {
        let admission_id = Uuid::now_v7();
        let controller_id = Uuid::now_v7();
        let valid = json!({
            "admission_id": admission_id,
            "controller_id": controller_id,
            "allow_writer_acquire": true,
            "operation": "run.submit",
        });
        assert!(writer_admission_matches(
            std::slice::from_ref(&valid),
            &[],
            admission_id,
            controller_id,
        ));

        for invalid in [
            {
                let mut value = valid.clone();
                value["allow_writer_acquire"] = Value::Bool(false);
                value
            },
            {
                let mut value = valid.clone();
                value["operation"] = json!("run.resume");
                value
            },
            {
                let mut value = valid.clone();
                value["controller_id"] = json!(Uuid::now_v7());
                value
            },
        ] {
            assert!(!writer_admission_matches(
                &[invalid],
                &[],
                admission_id,
                controller_id,
            ));
        }
        assert!(!writer_admission_matches(
            &[valid],
            &[json!({"admission_id": admission_id})],
            admission_id,
            controller_id,
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reconciliation_preparation_matches_its_executed_request() {
        let identity = crate::worker::current_startup_owner(&"a".repeat(64), Uuid::now_v7(), 1)
            .unwrap()
            .identity;
        for verb in [RunVerb::Recover, RunVerb::Reconcile] {
            let request = Prepared::Plain.into_request(verb, identity.clone());
            assert!(matches!(request, ControlRequestV1::Reconcile { .. }));
            let normalized = Prepared::Plain.normalized_request(verb);
            let bytes = canonicalize(&parse(&normalized.to_string()).unwrap()).unwrap();
            assert_eq!(
                request
                    .mutation_request_identity()
                    .unwrap()
                    .request_sha256(),
                sha256_hex(&bytes)
            );
        }
    }

    #[test]
    fn durable_admission_replays_only_original_owner_request_and_revision() {
        let root = std::env::temp_dir().join(format!("dolgorae-admission-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.join("recovery")).unwrap();
        std::fs::set_permissions(
            root.join("recovery"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(root.join("audit.jsonl"), b"").unwrap();
        std::fs::set_permissions(
            root.join("audit.jsonl"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let run_id = Uuid::now_v7();
        let controller = Uuid::now_v7();
        let mut ledger = crate::ledger::Ledger::open(&root, run_id).unwrap();
        let digest = "a".repeat(64);
        let first = admit_mutation_digest(
            &mut ledger,
            controller,
            0,
            digest.clone(),
            "run.resume",
            false,
        )
        .unwrap();
        assert_eq!(ledger.head().unwrap().sequence, 1);
        assert!(!first.allow_writer_acquire);
        assert_eq!(
            admit_mutation_digest(
                &mut ledger,
                controller,
                0,
                digest.clone(),
                "run.resume",
                true
            )
            .unwrap_err()
            .code,
            "RUN_STATE_CONFLICT"
        );
        let replay = admit_mutation_digest(
            &mut ledger,
            controller,
            0,
            digest.clone(),
            "run.resume",
            false,
        )
        .unwrap();
        assert_eq!(first.admission_id, replay.admission_id);
        assert_eq!(ledger.head().unwrap().sequence, 1);
        for (owner, expected, request) in [
            (Uuid::now_v7(), 0, digest.clone()),
            (controller, 1, digest.clone()),
            (controller, 0, "b".repeat(64)),
        ] {
            assert_eq!(
                admit_mutation_digest(&mut ledger, owner, expected, request, "run.resume", false)
                    .unwrap_err()
                    .code,
                "RUN_STATE_CONFLICT"
            );
        }
        assert_eq!(
            complete_mutation_admission(&mut ledger, Uuid::now_v7())
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        drop(ledger);
        let mut ledger = crate::ledger::Ledger::open(&root, run_id).unwrap();
        assert_eq!(
            active_mutation_admission(&ledger)
                .unwrap()
                .unwrap()
                .admission_id,
            first.admission_id
        );
        complete_mutation_admission(&mut ledger, first.admission_id).unwrap();
        assert!(active_mutation_admission(&ledger).unwrap().is_none());
        assert_eq!(
            admit_mutation_digest(
                &mut ledger,
                controller,
                0,
                digest.clone(),
                "run.resume",
                false
            )
            .unwrap_err()
            .code,
            "RUN_STATE_CONFLICT"
        );
        let head = ledger.head().unwrap().sequence;
        let next =
            admit_mutation_digest(&mut ledger, controller, head, digest, "run.resume", false)
                .unwrap();
        assert_ne!(first.admission_id, next.admission_id);
        let head = ledger.head().unwrap().sequence;
        assert_eq!(
            admit_mutation_digest(
                &mut ledger,
                controller,
                head - 1,
                "c".repeat(64),
                "run.reconcile",
                false
            )
            .unwrap_err()
            .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(
            active_mutation_admission(&ledger)
                .unwrap()
                .unwrap()
                .admission_id,
            next.admission_id
        );
        let reconciliation = admit_mutation_digest(
            &mut ledger,
            controller,
            head,
            "c".repeat(64),
            "run.reconcile",
            false,
        )
        .unwrap();
        assert_ne!(reconciliation.admission_id, next.admission_id);
        assert_eq!(
            complete_mutation_admission(&mut ledger, next.admission_id)
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(
            active_mutation_admission(&ledger)
                .unwrap()
                .unwrap()
                .admission_id,
            reconciliation.admission_id
        );
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }
}
