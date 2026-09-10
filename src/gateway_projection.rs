//! Pure conversion of captured semantic facts into the checked public protocol.

use crate::domain;
use crate::machine::MachineError;
use crate::protocol::public_v1 as pb;
use crate::snapshot::RunSnapshot;
use crate::writer::WriterAuthorityState;
use base64::Engine;

/// Facts captured by the observation owner alongside the durable snapshot.
/// Conversion never probes a process or guesses an unavailable generation.
#[derive(Clone, Debug)]
pub struct ProjectionFacts {
    pub effective_policy: domain::EffectivePolicy,
    pub lane_state: pb::ServerLaneState,
    pub process_generation: Option<u64>,
    pub socket_identity_sha256: Option<String>,
    pub background: Option<BackgroundFacts>,
    pub active_turn_status: Option<pb::TurnStatus>,
    pub last_final_response: Option<crate::turn::FinalResponse>,
}

#[derive(Clone, Debug)]
pub struct BackgroundFacts {
    pub state: pb::BackgroundExecutionState,
    pub mechanism: pb::BackgroundExecutionMechanism,
    pub census_revision: u64,
    pub observed_process_count: u32,
    pub quiescent_since: Option<prost_types::Timestamp>,
    pub consecutive_empty_samples: u32,
}

fn missing(fact: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "public projection lacks a representable authoritative fact",
        false,
        serde_json::json!({"invariant": fact}),
    )
}

macro_rules! map_enum {
    ($name:ident, $source:ty, $target:ident, [$($variant:ident),+ $(,)?]) => {
        pub fn $name(value: $source) -> pb::$target {
            match value { $(<$source>::$variant => pb::$target::$variant),+ }
        }
    };
}

map_enum!(
    lifecycle,
    domain::RunLifecycle,
    RunLifecycle,
    [
        Starting,
        Idle,
        Running,
        WaitingInteraction,
        ReconciliationRequired,
        Paused,
        Closed,
        StartFailed,
        OutcomeUnknown
    ]
);
map_enum!(
    control_mode,
    domain::ControlMode,
    ControlMode,
    [DirectInteractive, ManagedAgent]
);
map_enum!(
    execution_lane,
    domain::ExecutionLane,
    ExecutionLane,
    [SharedReadonly, Dedicated]
);
map_enum!(
    assurance,
    domain::Assurance,
    AssuranceLevel,
    [
        BestEffortPersonalAlpha,
        VerifiedThreadScopedControl,
        StrongProcessContainment
    ]
);
map_enum!(
    access,
    domain::Access,
    EffectiveAccess,
    [Read, Write, Transitioning, Unsupported, Unknown]
);
map_enum!(
    verification,
    domain::PolicyVerification,
    PolicyVerification,
    [Verified, Unverified, Failed]
);
map_enum!(
    controller_kind,
    domain::ControllerKind,
    ControllerKind,
    [
        HumanCli,
        InteractiveClient,
        WorkflowOrchestrator,
        Automation,
        Other
    ]
);
map_enum!(
    purpose,
    domain::PurposeKind,
    PurposeKind,
    [
        Interactive,
        Planning,
        Implementation,
        Review,
        Research,
        Discussion,
        WorkflowStage,
        Other
    ]
);
map_enum!(
    writer_authority,
    WriterAuthorityState,
    WriterAuthorityState,
    [
        None,
        Reserved,
        Active,
        HandoffPrepared,
        Releasing,
        BlockedUnknown
    ]
);

pub fn stamp(value: &crate::snapshot::ProjectionStamp) -> pb::ProjectionStamp {
    pb::ProjectionStamp {
        captured_head_cursor: value.captured_head_cursor.clone(),
        run_state_revision: value.run_state_revision,
        writer_state_revision: value.writer_state_revision,
        interaction_state_revision: value.interaction_state_revision,
    }
}

pub fn controller(value: &domain::ControllerIdentity) -> pb::ControllerProjection {
    pb::ControllerProjection {
        controller_id: value.controller_id.to_string(),
        generation: value.generation,
        kind: controller_kind(value.kind) as i32,
        instance_id: value.instance_id.clone(),
        subject_id: value.subject_id.clone(),
    }
}

pub fn path(value: &crate::workspace::LosslessPath) -> Result<pb::PathProjection, MachineError> {
    use crate::workspace::LosslessPath;
    let value = match value {
        LosslessPath::Utf8(value) => pb::path_projection::Value::Utf8Path(value.clone()),
        LosslessPath::Bytes { bytes } => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(bytes)
                .map_err(|_| missing("canonical path bytes"))?;
            if base64::engine::general_purpose::STANDARD.encode(&decoded) != *bytes {
                return Err(missing("canonical path bytes"));
            }
            pb::path_projection::Value::OpaquePath(decoded)
        }
    };
    Ok(pb::PathProjection { value: Some(value) })
}

pub fn reconciliation_action(
    value: Option<&str>,
) -> Result<pb::ReconciliationAction, MachineError> {
    Ok(match value {
        None => pb::ReconciliationAction::None,
        Some("retry_census") => pb::ReconciliationAction::RetryCensus,
        Some("reconcile_dedicated_lane") => pb::ReconciliationAction::ReconcileDedicatedLane,
        Some("operator_repair") => pb::ReconciliationAction::OperatorRepair,
        Some("reverify_writer_policy" | "reverify_source_writer_policy") => {
            pb::ReconciliationAction::ReverifyWriterPolicy
        }
        Some(_) => return Err(missing("typed writer reconciliation action")),
    })
}

pub fn configuration(snapshot: &RunSnapshot) -> pb::RunConfigurationProjection {
    let manifest = &snapshot.manifest;
    let mut required_capabilities = manifest.required_capabilities.clone();
    required_capabilities.sort();
    required_capabilities.dedup();
    pb::RunConfigurationProjection {
        profile_name: manifest.profile.profile_name.clone(),
        purpose: purpose(manifest.purpose.kind) as i32,
        purpose_label: manifest.purpose.external_label.clone(),
        model_id: manifest.model.clone(),
        default_effort: snapshot
            .projection
            .default_effort
            .clone()
            .unwrap_or_else(|| manifest.default_reasoning_effort.clone()),
        required_capabilities,
        parent: manifest
            .parent_ref
            .as_ref()
            .map(|value| pb::ParentRefProjection {
                namespace: value.namespace.clone(),
                kind: value.kind.clone(),
                id: value.id.clone(),
            }),
        instruction_contract: Some(pb::InstructionContractProjection {
            schema_id: manifest.instructions.schema.clone(),
            common_prefix_version: manifest.instructions.common_prefix_version,
            mode_prefix_version: manifest.instructions.mode_prefix_version,
            purpose_prefix_version: manifest.instructions.purpose_prefix_version,
        }),
        controller_instructions_byte_length: manifest.instructions.normalized_byte_length,
        controller_instructions_sha256: manifest.instructions.normalized_sha256.clone(),
    }
}

pub fn lineage(
    value: &crate::run::WriteContinuationProvenance,
) -> Result<pb::LineageProjection, MachineError> {
    fn kind(value: &str) -> Result<i32, MachineError> {
        let value: domain::ControllerKind =
            serde_json::from_value(serde_json::Value::String(value.to_owned()))
                .map_err(|_| missing("lineage controller kind"))?;
        Ok(controller_kind(value) as i32)
    }
    Ok(pb::LineageProjection {
        source_run_id: value.source_run_id.to_string(),
        source_turn_id: value.source_turn_id.clone(),
        source_thread_id: value.source_thread_id.clone(),
        creation_reason: match value.creation_reason.as_str() {
            "shared_readonly_source" => pb::WriteContinuationReason::SharedReadonlySource,
            "access_transition_unavailable" => {
                pb::WriteContinuationReason::AccessTransitionUnavailable
            }
            "access_transition_unverified" => {
                pb::WriteContinuationReason::AccessTransitionUnverified
            }
            _ => return Err(missing("lineage creation reason")),
        } as i32,
        source_controller_kind: kind(&value.source_controller_kind)?,
        destination_controller_kind: kind(&value.destination_controller_kind)?,
        continuation_id: None,
        handoff_summary_sha256: value.handoff_summary_sha256.clone(),
        artifact_refs: value
            .artifact_refs
            .iter()
            .map(ToString::to_string)
            .collect(),
        workspace_baseline_sha256: value.workspace_baseline_sha256.clone(),
        created_at: Some(
            value
                .created_at
                .parse()
                .map_err(|_| missing("lineage creation timestamp"))?,
        ),
        immutable: true,
    })
}

pub fn final_response(value: &crate::turn::FinalResponse) -> pb::FinalResponse {
    use crate::turn::FinalResponse;
    let value = match value {
        FinalResponse::Inline { text } => pb::final_response::Value::InlineUtf8(text.clone()),
        FinalResponse::Artifact {
            artifact_id,
            byte_length,
            sha256,
            ..
        } => pb::final_response::Value::Artifact(pb::ArtifactRef {
            artifact_id: artifact_id.to_string(),
            kind: pb::ArtifactKind::FinalResponse as i32,
            visibility: pb::ArtifactVisibility::Observer as i32,
            media_type: "text/markdown".to_owned(),
            byte_length: *byte_length,
            sha256: sha256.clone(),
        }),
        FinalResponse::Unavailable {
            byte_length,
            sha256,
            reason,
        } => pb::final_response::Value::Unavailable(pb::UnavailableContent {
            observed_byte_length: *byte_length,
            sha256: sha256.clone(),
            reason: reason.clone(),
        }),
    };
    pb::FinalResponse { value: Some(value) }
}

pub fn turn_projection(
    run_id: uuid::Uuid,
    thread_id: &str,
    turn_id: &str,
    status: pb::TurnStatus,
    cursor: &str,
    response: Option<&crate::turn::FinalResponse>,
) -> Result<pb::TurnProjection, MachineError> {
    if thread_id.is_empty() || turn_id.is_empty() || status == pb::TurnStatus::Unspecified {
        return Err(missing("accepted Turn identity and status"));
    }
    Ok(pb::TurnProjection {
        run_id: run_id.to_string(),
        thread_id: thread_id.to_owned(),
        turn_id: turn_id.to_owned(),
        status: status as i32,
        cursor: cursor.to_owned(),
        final_response: response.map(final_response),
    })
}

fn recovery(snapshot: &RunSnapshot) -> pb::RecoveryProjection {
    recovery_for(
        snapshot.projection.lifecycle,
        snapshot
            .runtime_record
            .as_ref()
            .is_some_and(|record| record.control_recovery_required),
    )
}

fn recovery_for(
    lifecycle: domain::RunLifecycle,
    control_recovery_required: bool,
) -> pb::RecoveryProjection {
    use domain::RunLifecycle;
    let (state, action, blocker) = if control_recovery_required {
        (
            pb::RecoveryState::Required,
            pb::RecoveryAction::RecoverRun,
            Some("control_socket_recovery_failed"),
        )
    } else {
        match lifecycle {
            RunLifecycle::ReconciliationRequired => (
                pb::RecoveryState::ReconciliationRequired,
                pb::RecoveryAction::ReconcileRun,
                Some("reconciliation_required"),
            ),
            RunLifecycle::StartFailed => (
                pb::RecoveryState::Required,
                pb::RecoveryAction::RecoverRun,
                Some("start_failed"),
            ),
            RunLifecycle::OutcomeUnknown => (
                pb::RecoveryState::OutcomeUnknown,
                pb::RecoveryAction::ReconcileRun,
                Some("outcome_unknown"),
            ),
            _ => (
                pb::RecoveryState::NotRequired,
                pb::RecoveryAction::None,
                None,
            ),
        }
    };
    pb::RecoveryProjection {
        state: state as i32,
        required_action: action as i32,
        blocker_code: blocker.map(str::to_owned),
    }
}

fn policy(
    snapshot: &RunSnapshot,
    facts: &ProjectionFacts,
) -> Result<pb::EffectivePolicyProjection, MachineError> {
    let value = &facts.effective_policy;
    let thread_generation = match (
        snapshot.projection.thread_id.is_some(),
        value.thread_generation,
    ) {
        (true, Some(generation)) if generation > 0 => generation,
        (false, None | Some(0)) => 0,
        _ => return Err(missing("effective policy thread generation")),
    };
    Ok(pb::EffectivePolicyProjection {
        access: access(value.access) as i32,
        verification: verification(value.verification) as i32,
        policy_epoch: value.policy_epoch.0,
        thread_generation,
        server_epoch: value.server_epoch,
        writer_generation: value.writer_generation,
    })
}

pub fn run_projection(
    snapshot: &RunSnapshot,
    facts: &ProjectionFacts,
) -> Result<pb::RunProjection, MachineError> {
    let manifest = &snapshot.manifest;
    let state = &snapshot.projection;
    let dedicated = manifest.execution_lane == domain::ExecutionLane::Dedicated;
    let owns_writer = snapshot
        .writer
        .holder
        .as_ref()
        .is_some_and(|holder| holder.run_id == manifest.run_id);
    let authority = if owns_writer {
        snapshot.writer.state
    } else {
        WriterAuthorityState::None
    };
    let state_variant = if !dedicated {
        pb::RunStateVariant::SharedReadonly
    } else if facts.lane_state == pb::ServerLaneState::Failed {
        pb::RunStateVariant::DedicatedGenerationFailed
    } else if state.thread_id.is_none() {
        pb::RunStateVariant::DedicatedUnstarted
    } else {
        match authority {
            WriterAuthorityState::Active => pb::RunStateVariant::DedicatedWriterActive,
            WriterAuthorityState::Reserved => pb::RunStateVariant::DedicatedWriterReserved,
            WriterAuthorityState::HandoffPrepared | WriterAuthorityState::Releasing => {
                pb::RunStateVariant::DedicatedReleasing
            }
            WriterAuthorityState::BlockedUnknown => pb::RunStateVariant::DedicatedBlockedUnknown,
            WriterAuthorityState::None if state.lifecycle == domain::RunLifecycle::Paused => {
                pb::RunStateVariant::DedicatedPaused
            }
            WriterAuthorityState::None => pb::RunStateVariant::DedicatedReader,
        }
    };
    let effective_policy = policy(snapshot, facts)?;
    let thread = state
        .thread_id
        .as_ref()
        .map(|thread_id| pb::ThreadProjection {
            thread_id: thread_id.clone(),
            thread_generation: effective_policy.thread_generation,
            server_epoch: effective_policy.server_epoch,
        });
    let active_turn = state
        .active_turn_id
        .as_ref()
        .map(|turn_id| {
            turn_projection(
                manifest.run_id,
                state
                    .thread_id
                    .as_deref()
                    .ok_or_else(|| missing("active Turn thread"))?,
                turn_id,
                facts
                    .active_turn_status
                    .ok_or_else(|| missing("active Turn status"))?,
                &snapshot.stamp.captured_head_cursor,
                None,
            )
        })
        .transpose()?;
    if facts.lane_state == pb::ServerLaneState::Unspecified {
        return Err(missing("server lane state"));
    }
    if facts.background.is_none() {
        return Err(missing("background execution observation"));
    }
    let background_execution = facts
        .background
        .as_ref()
        .map(|value| {
            if value.state == pb::BackgroundExecutionState::Unspecified
                || value.mechanism == pb::BackgroundExecutionMechanism::Unspecified
            {
                return Err(missing("background execution observation"));
            }
            Ok(pb::BackgroundExecutionProjection {
                state: value.state as i32,
                mechanism: value.mechanism as i32,
                census_revision: value.census_revision,
                observed_process_count: value.observed_process_count,
                quiescent_since: value.quiescent_since,
                consecutive_empty_samples: value.consecutive_empty_samples,
            })
        })
        .transpose()?;
    Ok(pb::RunProjection {
        workspace_id: manifest.workspace_id.clone(),
        run_id: manifest.run_id.to_string(),
        lifecycle: lifecycle(state.lifecycle) as i32,
        control_mode: control_mode(manifest.control_mode) as i32,
        execution_lane: execution_lane(manifest.execution_lane) as i32,
        controller: Some(controller(&snapshot.controller)),
        thread,
        active_turn,
        event_cursor: snapshot.stamp.captured_head_cursor.clone(),
        pending_interaction_count: u32::try_from(state.pending_requests.len())
            .map_err(|_| missing("bounded pending interaction count"))?,
        recovery: Some(recovery(snapshot)),
        effective_policy: Some(effective_policy),
        writer_authority: Some(pb::WriterAuthorityProjection {
            state: writer_authority(authority) as i32,
            writer_generation: if owns_writer {
                snapshot.writer.writer_generation
            } else {
                0
            },
            transaction_id: if owns_writer {
                snapshot.writer.transaction_id.map(|id| id.to_string())
            } else {
                None
            },
            reconciliation_action: reconciliation_action(if owns_writer {
                snapshot.writer.recovery_action.as_deref()
            } else {
                None
            })? as i32,
        }),
        state_revision: snapshot.stamp.run_state_revision,
        last_final_response: facts.last_final_response.as_ref().map(final_response),
        state_variant: state_variant as i32,
        server_lane: Some(pb::ServerLaneProjection {
            kind: execution_lane(manifest.execution_lane) as i32,
            lane_id: dedicated.then(|| manifest.run_id.to_string()),
            process_generation: facts.process_generation,
            server_epoch: snapshot.app_server_epoch,
            state: facts.lane_state as i32,
            socket_identity_sha256: facts.socket_identity_sha256.clone(),
        }),
        background_execution,
        requested_assurance: assurance(manifest.requested_assurance) as i32,
        achieved_assurance: assurance(manifest.achieved_assurance) as i32,
        lineage: manifest
            .write_continuation_provenance
            .as_ref()
            .map(lineage)
            .transpose()?,
        configuration: Some(configuration(snapshot)),
        stamp: Some(stamp(&snapshot.stamp)),
    })
}

pub fn writer_state(
    snapshot: &RunSnapshot,
    facts: &ProjectionFacts,
    context: pb::ResponseContext,
) -> Result<pb::WriterState, MachineError> {
    let writer = &snapshot.writer;
    if writer
        .holder
        .as_ref()
        .is_some_and(|holder| holder.run_id != snapshot.manifest.run_id)
    {
        return Err(missing("Writer owner Run snapshot"));
    }
    let policy = policy(snapshot, facts)?;
    let recovery = recovery(snapshot);
    let background_execution_blocker = match facts.background.as_ref().map(|value| value.state) {
        Some(
            pb::BackgroundExecutionState::VerifiedAbsent
            | pb::BackgroundExecutionState::NotApplicable,
        ) => None,
        Some(pb::BackgroundExecutionState::Active) => {
            Some("background_execution_active".to_owned())
        }
        _ => Some("background_execution_unverified".to_owned()),
    };
    Ok(pb::WriterState {
        context: Some(context),
        workspace_id: writer.workspace_id.clone(),
        authority_state: writer_authority(writer.state) as i32,
        owner_run_id: writer
            .holder
            .as_ref()
            .map(|holder| holder.run_id.to_string()),
        writer_generation: writer.writer_generation,
        handoff_id: writer
            .handoff
            .as_ref()
            .map(|value| value.handoff_id.to_string()),
        handoff_eligible: writer.state == WriterAuthorityState::Active
            && policy.verification == pb::PolicyVerification::Verified as i32
            && policy.access == pb::EffectiveAccess::Write as i32
            && background_execution_blocker.is_none()
            && recovery.state == pb::RecoveryState::NotRequired as i32
            && snapshot.projection.lifecycle == domain::RunLifecycle::Idle
            && snapshot.projection.pending_requests.is_empty(),
        effective_access: policy.access,
        policy_verification: policy.verification,
        execution_lane: execution_lane(snapshot.manifest.execution_lane) as i32,
        requested_assurance: assurance(snapshot.manifest.requested_assurance) as i32,
        achieved_assurance: assurance(snapshot.manifest.achieved_assurance) as i32,
        background_execution_blocker,
        recovery_blocker: recovery.blocker_code,
        reconciliation_action: reconciliation_action(writer.recovery_action.as_deref())? as i32,
        state_revision: snapshot.stamp.writer_state_revision,
        stamp: Some(stamp(&snapshot.stamp)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_preserves_non_utf8_bytes_and_rejects_noncanonical_base64() {
        let raw = vec![b'/', 0xff];
        let source = crate::workspace::LosslessPath::Bytes {
            bytes: base64::engine::general_purpose::STANDARD.encode(&raw),
        };
        assert_eq!(
            path(&source).unwrap().value,
            Some(pb::path_projection::Value::OpaquePath(raw))
        );
        assert!(
            path(&crate::workspace::LosslessPath::Bytes {
                bytes: "/w".to_owned()
            })
            .is_err()
        );
    }

    #[test]
    fn unknown_reconciliation_action_fails_closed() {
        assert!(reconciliation_action(Some("future_action")).is_err());
        assert_eq!(
            reconciliation_action(Some("reverify_source_writer_policy")).unwrap(),
            pb::ReconciliationAction::ReverifyWriterPolicy
        );
    }

    #[test]
    fn turn_requires_real_identity_and_status() {
        assert!(
            turn_projection(
                uuid::Uuid::nil(),
                "",
                "turn",
                pb::TurnStatus::Accepted,
                "4",
                None
            )
            .is_err()
        );
        assert!(
            turn_projection(
                uuid::Uuid::nil(),
                "thread",
                "turn",
                pb::TurnStatus::Unspecified,
                "4",
                None
            )
            .is_err()
        );
        let response = crate::turn::FinalResponse::Inline {
            text: "done".to_owned(),
        };
        let projection = turn_projection(
            uuid::Uuid::nil(),
            "thread",
            "turn",
            pb::TurnStatus::Completed,
            "4",
            Some(&response),
        )
        .unwrap();
        assert_eq!(
            projection.final_response.unwrap().value,
            Some(pb::final_response::Value::InlineUtf8("done".to_owned()))
        );
    }

    #[test]
    fn stamp_preserves_independent_authorities() {
        let value = stamp(&crate::snapshot::ProjectionStamp {
            captured_head_cursor: "29".to_owned(),
            run_state_revision: 29,
            writer_state_revision: 7,
            interaction_state_revision: 18,
        });
        assert_eq!(
            (
                value.run_state_revision,
                value.writer_state_revision,
                value.interaction_state_revision
            ),
            (29, 7, 18)
        );
        assert_eq!(value.captured_head_cursor, "29");
    }
}

macro_rules! checked_wire_enum {
    ($name:ident, $target:ident, $prefix:literal) => {
        fn $name(value: &str) -> Result<i32, MachineError> {
            let name = format!(concat!($prefix, "{}"), value.to_ascii_uppercase());
            pb::$target::from_str_name(&name)
                .filter(|value| *value as i32 != 0)
                .map(|value| value as i32)
                .ok_or_else(|| missing(concat!(stringify!($target), " vocabulary")))
        }
    };
}
checked_wire_enum!(support_name, SupportState, "SUPPORT_STATE_");
checked_wire_enum!(lane_name, ExecutionLane, "EXECUTION_LANE_");
checked_wire_enum!(assurance_name, AssuranceLevel, "ASSURANCE_LEVEL_");
checked_wire_enum!(controller_name, ControllerKind, "CONTROLLER_KIND_");
checked_wire_enum!(
    interaction_support_name,
    InteractionSupport,
    "INTERACTION_SUPPORT_"
);
checked_wire_enum!(
    background_mechanism_name,
    BackgroundExecutionMechanism,
    "BACKGROUND_EXECUTION_MECHANISM_"
);
checked_wire_enum!(transport_name, PublicTransport, "PUBLIC_TRANSPORT_");
checked_wire_enum!(
    projection_profile_name,
    ProjectionProfile,
    "PROJECTION_PROFILE_"
);
checked_wire_enum!(control_mode_name, ControlMode, "CONTROL_MODE_");
checked_wire_enum!(
    command_support_name,
    CommandExecutionSupport,
    "COMMAND_EXECUTION_SUPPORT_"
);
checked_wire_enum!(
    background_support_name,
    BackgroundControlSupport,
    "BACKGROUND_CONTROL_SUPPORT_"
);
checked_wire_enum!(
    cleanup_support_name,
    ProcessCleanupSupport,
    "PROCESS_CLEANUP_SUPPORT_"
);
checked_wire_enum!(visibility_name, ArtifactVisibility, "ARTIFACT_VISIBILITY_");
checked_wire_enum!(launch_mode_name, ProfileLaunchMode, "PROFILE_LAUNCH_MODE_");
checked_wire_enum!(
    native_policy_name,
    NativeSubagentPolicy,
    "NATIVE_SUBAGENT_POLICY_"
);

fn map_names(
    values: &[String],
    convert: fn(&str) -> Result<i32, MachineError>,
) -> Result<Vec<i32>, MachineError> {
    values.iter().map(|value| convert(value)).collect()
}

pub fn capabilities(
    value: &crate::runtime::RuntimeCapabilities,
    context: pb::ResponseContext,
) -> Result<pb::GetCapabilitiesResponse, MachineError> {
    let credential = &value.controller_credential;
    if credential.capability_encoding != "base64url_no_padding"
        || credential.normalized_principal != "kind+subject_id_else_kind+instance_id"
        || credential.symlinks != "forbidden"
        || value.artifact_bounds.digest != "sha256"
        || value.assurance.selection_time != "before_run_allocation"
        || value.controller_carrier_root != "home/.dolgorae/controller-carriers"
    {
        return Err(missing("credential and artifact capability contract"));
    }
    let interactions = &value.interactions;
    let interaction_items = [
        (
            pb::InteractionKind::CommandExecutionApproval,
            &interactions.command_execution_approval,
        ),
        (
            pb::InteractionKind::FileChangeApproval,
            &interactions.file_change_approval,
        ),
        (
            pb::InteractionKind::PermissionRequest,
            &interactions.permission_request,
        ),
        (pb::InteractionKind::UserInput, &interactions.user_input),
        (
            pb::InteractionKind::McpElicitation,
            &interactions.mcp_elicitation,
        ),
        (
            pb::InteractionKind::ConnectorApproval,
            &interactions.connector_approval,
        ),
    ]
    .into_iter()
    .map(|(kind, support)| {
        Ok(pb::InteractionCapability {
            kind: kind as i32,
            support: interaction_support_name(support)?,
        })
    })
    .collect::<Result<Vec<_>, MachineError>>()?;
    let lanes = value
        .lane_capabilities
        .iter()
        .map(|(name, lane)| {
            Ok(pb::LaneCapability {
                lane: lane_name(name)?,
                command_execution: command_support_name(&lane.command_execution)?,
                background_control: background_support_name(&lane.background_control)?,
                per_run_process_cleanup: cleanup_support_name(&lane.per_run_process_cleanup)?,
                maximum_assurance: assurance_name(&lane.maximum_assurance)?,
                writer_support: lane.writer_support,
                lazy_first_input: match lane.physical_start.as_deref() {
                    None => false,
                    Some("lazy_first_input") => true,
                    Some(_) => return Err(missing("lane physical start policy")),
                },
            })
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    let features = &value.features;
    Ok(pb::GetCapabilitiesResponse {
        context: Some(context),
        dolgorae_version: value.dolgorae_version.clone(),
        protocol: Some(pb::ProtocolCapabilities {
            rpc_protocol_version: value.rpc_protocol_version,
            minimum_client_protocol_version: value.minimum_rpc_client_version,
            maximum_client_protocol_version: value.maximum_rpc_client_version,
            machine_protocol_version: value.machine_protocol_version,
            event_protocol_version: value.event_protocol_version,
            timeline_protocol_version: value.timeline_protocol_version,
            event_projection_version: value.event_projection_version,
            grpc_error_detail_version: value.grpc_error_detail_version,
            projection_profiles: map_names(&value.projection_profiles, projection_profile_name)?,
        }),
        controller_carrier: Some(pb::CredentialCarrierCapabilities {
            schema_id: credential.schema_id.clone(),
            schema_version: credential.schema_version,
            schema_sha256: credential.schema_sha256.clone(),
            carrier_root_locator: value.controller_carrier_root.clone(),
            accepted_controller_kinds: map_names(&credential.accepted_kinds, controller_name)?,
            capability_byte_length: credential.capability_byte_length,
            capability_encoding: pb::ControllerCapabilityEncoding::Base64urlNoPadding as i32,
            parent_directory_mode: u32::from_str_radix(&credential.parent_directory_mode, 8)
                .map_err(|_| missing("credential parent mode"))?,
            credential_file_mode: u32::from_str_radix(&credential.file_mode, 8)
                .map_err(|_| missing("credential file mode"))?,
            same_uid_required: credential.same_uid,
            regular_file_required: credential.regular_file,
            symlinks_forbidden: true,
            create_exclusive_required: credential.create_exclusive,
            maximum_file_bytes: u64::from(credential.maximum_file_bytes),
            client_descendant_pattern: credential.client_descendant_pattern.clone(),
            normalized_principal_rule: pb::ControllerPrincipalRule::KindSubjectIdElseKindInstanceId
                as i32,
            initial_generation: u64::from(credential.initial_generation),
            carrier_root_policy: pb::ControllerCarrierRootPolicy::DolgoraeOwnedHome as i32,
        }),
        features: Some(pb::RuntimeFeatureCapabilities {
            persistent_runs: features.persistent_runs,
            run_fork: features.run_fork,
            reader_writer_access: features.reader_writer_access,
            threadless_acquire_write: features.threadless_acquire_write,
            first_write_via_submit_turn: features.first_write_via_submit_turn,
            write_continuation: features.write_continuation,
            controller_timeline: features.controller_timeline,
            writer_handoff: features.writer_handoff,
            durable_writer_authority: features.durable_writer_authority,
            sticky_dedicated_lanes: features.sticky_dedicated_lanes,
            event_replay: features.event_replay,
            artifact_retrieval: features.artifact_retrieval,
            controller_binding: features.controller_binding,
            worker_controller_revalidation: features.worker_controller_revalidation,
            safe_client_projection: features.safe_client_projection,
            public_local_socket: features.public_local_socket,
            workspace_event_stream: features.workspace_event_stream,
            control_modes: features.control_modes,
            brokered_independent_subagent_runs: features.brokered_independent_subagent_runs,
            assurance_negotiation: features.assurance_negotiation,
            profile_server_migration: features.profile_server_migration,
            profile_membership_repair: features.profile_membership_repair,
            profile_diagnostics: features.profile_diagnostics,
            operator_capability: features.operator_capability,
            operator_controller_reset: features.operator_controller_reset,
        }),
        interactions: Some(pb::InteractionCapabilities {
            known_kinds: interaction_items.iter().map(|item| item.kind).collect(),
            items: interaction_items,
            maximum_response_bytes: interactions.maximum_response_bytes,
            maximum_safe_payload_bytes: interactions.maximum_safe_payload_bytes,
        }),
        assurance: Some(pb::AssuranceCapabilities {
            supported: map_names(&value.assurance.supported, assurance_name)?,
            maximum_achievable: assurance_name(&value.assurance.maximum_achievable)?,
            selected_before_run_allocation: true,
        }),
        lanes: Some(pb::LaneCapabilities {
            supported_lanes: map_names(&value.execution_lanes, lane_name)?,
            items: lanes,
        }),
        native_subagents: Some(pb::NativeSubagentCapabilities {
            lifecycle_observation: support_name(&value.native_subagents.lifecycle_observation)?,
            disable_enforcement: support_name(&value.native_subagents.disable_enforcement)?,
            quiescence_tracking: support_name(&value.native_subagents.quiescence_tracking)?,
            enabled: native_policy_name(&value.native_subagents_policy)?
                == pb::NativeSubagentPolicy::Enabled as i32,
            safe_reason: value.native_subagents.reason.clone(),
        }),
        artifacts: Some(pb::ArtifactCapabilities {
            maximum_artifact_size: u64::from(value.artifact_bounds.maximum_artifact_bytes),
            maximum_chunk_size: value.artifact_bounds.maximum_chunk_bytes,
            digest_verification_required: true,
            exact_byte_length_reported: value.artifact_bounds.exact_byte_length,
            visibility_classes: map_names(
                &value.artifact_bounds.visibility_classes,
                visibility_name,
            )?,
            maximum_inline_response_bytes: value.artifact_bounds.maximum_inline_response_bytes,
        }),
        supported_methods: value.grpc_methods.clone(),
        descriptor_sha256: value.rpc_descriptor_sha256.clone(),
        supported_control_modes: map_names(&value.control_modes, control_mode_name)?,
        access_policy_transition: support_name(&value.access_policy_transition)?,
        background_execution: Some(pb::BackgroundExecutionCapabilities {
            support: support_name(&value.background_execution_control.support)?,
            mechanisms: vec![background_mechanism_name(
                &value.background_execution_control.mechanism,
            )?],
        }),
        supported_transports: map_names(&value.supported_transports, transport_name)?,
        independent_run_concurrency: Some(pb::IndependentRunConcurrencyCapabilities {
            basic_same_home_coexistence: support_name(
                &value
                    .independent_run_concurrency
                    .basic_same_home_coexistence,
            )?,
            storage_and_long_duration: support_name(
                &value.independent_run_concurrency.storage_and_long_duration,
            )?,
            resource_warning_live_dedicated: value
                .independent_run_concurrency
                .resource_warning_live_dedicated,
        }),
        profile_launch_mode: launch_mode_name(&value.profile_launch_mode)?,
        native_subagent_policy: native_policy_name(&value.native_subagents_policy)?,
    })
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn runtime_capabilities_preserve_support_distinctions_and_fail_closed() {
        let mut source = crate::runtime::capabilities();
        source.interactions.permission_request = "recognized_unsupported".to_owned();
        let projected = capabilities(&source, pb::ResponseContext::default()).unwrap();
        let interactions = projected.interactions.unwrap();
        assert_eq!(
            interactions
                .items
                .iter()
                .find(|item| item.kind == pb::InteractionKind::PermissionRequest as i32)
                .unwrap()
                .support,
            pb::InteractionSupport::RecognizedUnsupported as i32
        );
        let credential = projected.controller_carrier.unwrap();
        assert_eq!(credential.parent_directory_mode, 0o700);
        assert_eq!(credential.credential_file_mode, 0o600);
        assert_eq!(
            projected.features.unwrap().persistent_runs,
            source.features.persistent_runs
        );
        source.interactions.permission_request = "future_support".to_owned();
        assert!(capabilities(&source, pb::ResponseContext::default()).is_err());
    }

    #[test]
    fn artifact_response_retains_the_authoritative_markdown_media_type() {
        let response = final_response(&crate::turn::FinalResponse::Artifact {
            artifact_id: uuid::Uuid::nil(),
            byte_length: 100,
            sha256: "a".repeat(64),
            created_at: "2026-09-09T00:00:00Z".to_owned(),
        });
        let Some(pb::final_response::Value::Artifact(artifact)) = response.value else {
            panic!("expected artifact");
        };
        assert_eq!(artifact.media_type, "text/markdown");
        assert_eq!(artifact.visibility, pb::ArtifactVisibility::Observer as i32);
    }
}

/// A model catalog entry from the profile's verified model-list observation.
#[derive(Clone, Debug)]
pub struct ProfileModelFacts {
    pub model_id: String,
    pub is_default: bool,
    pub supported_efforts: Vec<String>,
}

/// Profile-scoped facts; runtime capabilities must already reflect this profile.
#[derive(Clone, Debug)]
pub struct ProfileFacts {
    pub snapshot: crate::profile::ProfileSnapshot,
    pub server_epoch: Option<u64>,
    pub models: Vec<ProfileModelFacts>,
    pub capabilities: crate::runtime::RuntimeCapabilities,
    pub blockers: Vec<pb::CapabilityBlocker>,
}

pub fn profile(value: &ProfileFacts) -> Result<pb::ProfileProjection, MachineError> {
    let snapshot = &value.snapshot;
    if snapshot.profile_name.is_empty()
        || snapshot.server_key.is_empty()
        || snapshot.codex_version.is_empty()
    {
        return Err(missing("profile runtime identity"));
    }
    if !value.models.is_empty() && value.models.iter().filter(|model| model.is_default).count() != 1
    {
        return Err(missing("model catalog with exactly one default"));
    }
    let mut model_ids = std::collections::BTreeSet::new();
    let models = value
        .models
        .iter()
        .map(|model| {
            let mut efforts = std::collections::BTreeSet::new();
            if model.model_id.is_empty()
                || !model_ids.insert(&model.model_id)
                || model.supported_efforts.is_empty()
                || model
                    .supported_efforts
                    .iter()
                    .any(|effort| effort.is_empty() || !efforts.insert(effort))
            {
                return Err(missing("unique model and nonempty unique effort catalog"));
            }
            Ok(pb::ModelCapability {
                model_id: model.model_id.clone(),
                is_default: model.is_default,
                supported_efforts: model.supported_efforts.clone(),
            })
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    if value.blockers.iter().any(|blocker| {
        pb::CapabilityBlockerCode::try_from(blocker.code)
            .map_or(true, |code| code == pb::CapabilityBlockerCode::Unspecified)
    }) {
        return Err(missing("typed profile capability blocker"));
    }
    let runtime = capabilities(&value.capabilities, pb::ResponseContext::default())?;
    let supported_interaction_kinds = runtime
        .interactions
        .as_ref()
        .ok_or_else(|| missing("profile interactions"))?
        .items
        .iter()
        .filter(|item| item.support == pb::InteractionSupport::Supported as i32)
        .map(|item| item.kind)
        .collect();
    let mut feature_flags = snapshot.enabled_features.clone();
    feature_flags.sort();
    feature_flags.dedup();
    Ok(pb::ProfileProjection {
        name: snapshot.profile_name.clone(),
        server_key: snapshot.server_key.clone(),
        server_epoch: value.server_epoch,
        compatibility: match snapshot.compatibility_verdict {
            crate::profile::CompatibilityVerdict::Tested => pb::ProfileCompatibility::Compatible,
            crate::profile::CompatibilityVerdict::Unverified => {
                pb::ProfileCompatibility::Unverified
            }
            crate::profile::CompatibilityVerdict::Rejected => {
                pb::ProfileCompatibility::Incompatible
            }
        } as i32,
        models,
        runtime_version: Some(snapshot.codex_version.clone()),
        supported_execution_lanes: runtime
            .lanes
            .ok_or_else(|| missing("profile execution lanes"))?
            .supported_lanes,
        maximum_assurance: runtime
            .assurance
            .ok_or_else(|| missing("profile assurance"))?
            .maximum_achievable,
        access_policy_transition: runtime.access_policy_transition,
        background_execution: runtime.background_execution,
        supported_interaction_kinds,
        native_subagents: runtime.native_subagents,
        feature_flags,
        blockers: value.blockers.clone(),
    })
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    fn facts() -> ProfileFacts {
        ProfileFacts {
            snapshot: crate::profile::ProfileSnapshot {
                schema_version: 1,
                profile_name: "test".to_owned(),
                canonical_codex_home: "/test".to_owned(),
                normalized_argv: vec![],
                launch_cwd_policy: "test".to_owned(),
                derived_launch_cwd: "/test".to_owned(),
                sanitized_environment: Default::default(),
                enabled_features: vec!["z".to_owned(), "a".to_owned()],
                disabled_features: vec![],
                process_static_configuration: Default::default(),
                initial_configuration_observation: Default::default(),
                executable_identity: crate::profile::ExecutableIdentity {
                    resolved_path: "/test/codex".to_owned(),
                    device: 1,
                    inode: 2,
                    sha256: "a".repeat(64),
                },
                codex_version: "0.153.4".to_owned(),
                schema_bundle_sha256: "b".repeat(64),
                compatibility_manifest_sha256: "c".repeat(64),
                launch_contract_sha256: "d".repeat(64),
                compatibility_verdict: crate::profile::CompatibilityVerdict::Tested,
                server_key: "server".to_owned(),
            },
            server_epoch: Some(7),
            models: vec![ProfileModelFacts {
                model_id: "model".to_owned(),
                is_default: true,
                supported_efforts: vec!["low".to_owned(), "high".to_owned()],
            }],
            capabilities: crate::runtime::capabilities(),
            blockers: vec![],
        }
    }

    #[test]
    fn profile_retains_real_model_menu_and_support_filter() {
        let mut facts = facts();
        facts.capabilities.interactions.user_input = "supported".to_owned();
        facts.capabilities.interactions.permission_request = "recognized_unsupported".to_owned();
        let projected = profile(&facts).unwrap();
        assert_eq!(projected.server_epoch, Some(7));
        assert_eq!(projected.models[0].supported_efforts, ["low", "high"]);
        assert_eq!(
            projected.supported_interaction_kinds,
            [
                pb::InteractionKind::CommandExecutionApproval as i32,
                pb::InteractionKind::FileChangeApproval as i32,
                pb::InteractionKind::UserInput as i32,
            ]
        );
        assert_eq!(projected.feature_flags, ["a", "z"]);
    }

    #[test]
    fn unavailable_profile_preserves_empty_catalog_and_typed_blocker() {
        let mut facts = facts();
        facts.models.clear();
        facts.server_epoch = None;
        facts.snapshot.compatibility_verdict = crate::profile::CompatibilityVerdict::Unverified;
        facts.blockers.push(pb::CapabilityBlocker {
            code: pb::CapabilityBlockerCode::ProfileServerUnavailable as i32,
            safe_message: "Profile server is stopped".to_owned(),
        });
        let projected = profile(&facts).unwrap();
        assert!(projected.models.is_empty());
        assert_eq!(projected.server_epoch, None);
        assert_eq!(
            projected.blockers[0].code,
            pb::CapabilityBlockerCode::ProfileServerUnavailable as i32
        );
    }

    #[test]
    fn incomplete_or_ambiguous_model_catalog_is_an_invariant_error() {
        let mut facts = facts();
        facts.models[0].supported_efforts.clear();
        let error = profile(&facts).unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert!(error.details.get("invariant").is_some());
        assert!(error.details.get("missing_fact").is_none());
        facts.models[0].supported_efforts.push("low".to_owned());
        facts.models.push(facts.models[0].clone());
        assert!(profile(&facts).is_err());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn failed_start_and_control_recovery_remain_blocking() {
        let failed = recovery_for(domain::RunLifecycle::StartFailed, false);
        assert_eq!(failed.state, pb::RecoveryState::Required as i32);
        assert_eq!(
            failed.required_action,
            pb::RecoveryAction::RecoverRun as i32
        );
        let control = recovery_for(domain::RunLifecycle::Idle, true);
        assert_eq!(control.state, pb::RecoveryState::Required as i32);
        assert_eq!(
            control.blocker_code.as_deref(),
            Some("control_socket_recovery_failed")
        );
        let idle = recovery_for(domain::RunLifecycle::Idle, false);
        assert_eq!(idle.state, pb::RecoveryState::NotRequired as i32);
        assert!(idle.blocker_code.is_none());
    }
}

impl ProjectionFacts {
    pub fn from_observation(
        snapshot: &RunSnapshot,
        observed: &crate::snapshot::RunObservation,
    ) -> Result<Self, MachineError> {
        use crate::snapshot::{BackgroundObservation, ObservedTurnStatus};
        use crate::worker::ProcessIdentityVerdict;
        let dedicated = snapshot.manifest.execution_lane == domain::ExecutionLane::Dedicated;
        let lane_state = if !dedicated {
            // A worker observation does not claim the shared Profile server's identity.
            pb::ServerLaneState::Unverified
        } else if snapshot.projection.thread_id.is_none() && observed.server_identity.is_none() {
            pb::ServerLaneState::Absent
        } else {
            match observed.server_identity {
                Some(ProcessIdentityVerdict::Match) => pb::ServerLaneState::Ready,
                Some(ProcessIdentityVerdict::Absent) => pb::ServerLaneState::Absent,
                Some(ProcessIdentityVerdict::Mismatch | ProcessIdentityVerdict::Unverifiable)
                | None => pb::ServerLaneState::Unverified,
            }
        };
        let background = match &observed.background {
            BackgroundObservation::NotApplicable => BackgroundFacts {
                state: pb::BackgroundExecutionState::NotApplicable,
                mechanism: pb::BackgroundExecutionMechanism::SharedProfileAggregate,
                census_revision: 0,
                observed_process_count: 0,
                quiescent_since: None,
                consecutive_empty_samples: 0,
            },
            BackgroundObservation::Unstarted => BackgroundFacts {
                state: pb::BackgroundExecutionState::VerifiedAbsent,
                mechanism: pb::BackgroundExecutionMechanism::DedicatedLaneProcessCensus,
                census_revision: 0,
                observed_process_count: 0,
                quiescent_since: None,
                consecutive_empty_samples: 0,
            },
            BackgroundObservation::VerifiedAbsent(evidence) => BackgroundFacts {
                state: pb::BackgroundExecutionState::VerifiedAbsent,
                mechanism: pb::BackgroundExecutionMechanism::DedicatedLaneProcessCensus,
                census_revision: evidence.census_revision,
                observed_process_count: 0,
                quiescent_since: None,
                consecutive_empty_samples: u32::from(evidence.consecutive_empty_samples),
            },
            BackgroundObservation::Unverified => BackgroundFacts {
                state: pb::BackgroundExecutionState::Unverified,
                mechanism: pb::BackgroundExecutionMechanism::DedicatedLaneProcessCensus,
                census_revision: 0,
                observed_process_count: 0,
                quiescent_since: None,
                consecutive_empty_samples: 0,
            },
        };
        Ok(Self {
            effective_policy: observed.effective_policy.clone(),
            lane_state,
            process_generation: dedicated
                .then(|| {
                    snapshot
                        .runtime_record
                        .as_ref()
                        .map(|record| record.identity.run_generation)
                })
                .flatten(),
            socket_identity_sha256: None,
            background: Some(background),
            active_turn_status: observed.active_turn_status.map(|status| match status {
                ObservedTurnStatus::Running => pb::TurnStatus::Running,
                ObservedTurnStatus::WaitingInteraction => pb::TurnStatus::WaitingInteraction,
                ObservedTurnStatus::OutcomeUnknown => pb::TurnStatus::OutcomeUnknown,
            }),
            last_final_response: observed.last_final_response.clone(),
        })
    }
}

pub fn ownerless_writer_state(
    writer: &crate::writer::WriterRecord,
    context: pb::ResponseContext,
) -> Result<pb::WriterState, MachineError> {
    if writer.holder.is_some()
        || writer.state != WriterAuthorityState::None
        || writer.handoff.is_some()
    {
        return Err(missing("ownerless Writer state"));
    }
    Ok(pb::WriterState {
        context: Some(context),
        workspace_id: writer.workspace_id.clone(),
        authority_state: pb::WriterAuthorityState::None as i32,
        owner_run_id: None,
        writer_generation: writer.writer_generation,
        handoff_id: None,
        handoff_eligible: false,
        effective_access: pb::EffectiveAccess::Unknown as i32,
        policy_verification: pb::PolicyVerification::Unverified as i32,
        execution_lane: pb::ExecutionLane::Unspecified as i32,
        requested_assurance: pb::AssuranceLevel::Unspecified as i32,
        achieved_assurance: pb::AssuranceLevel::Unspecified as i32,
        background_execution_blocker: None,
        recovery_blocker: writer.recovery_action.clone(),
        reconciliation_action: reconciliation_action(writer.recovery_action.as_deref())? as i32,
        state_revision: writer.authority_revision,
        stamp: Some(pb::ProjectionStamp {
            captured_head_cursor: String::new(),
            run_state_revision: 0,
            writer_state_revision: writer.authority_revision,
            interaction_state_revision: 0,
        }),
    })
}

#[cfg(test)]
mod ownerless_tests {
    use super::*;

    #[test]
    fn ownerless_writer_does_not_borrow_a_run_stamp_or_invent_a_lane() {
        let mut writer = crate::writer::WriterRecord::empty("workspace");
        writer.authority_revision = 19;
        writer.writer_generation = 4;
        let projection = ownerless_writer_state(&writer, pb::ResponseContext::default()).unwrap();
        let stamp = projection.stamp.unwrap();
        assert!(stamp.captured_head_cursor.is_empty());
        assert_eq!(
            (
                stamp.run_state_revision,
                stamp.writer_state_revision,
                stamp.interaction_state_revision
            ),
            (0, 19, 0)
        );
        assert_eq!(projection.writer_generation, 4);
        assert_eq!(
            projection.execution_lane,
            pb::ExecutionLane::Unspecified as i32
        );
        assert!(!projection.handoff_eligible);
    }
}

/// Restate the acceptance boundary from the immutable accepted-operation receipt.
/// The supplied snapshot must be reconstructed from that receipt, not reread at delivery.
pub fn acceptance_facts(
    snapshot: &RunSnapshot,
    effective_policy: &domain::EffectivePolicy,
) -> Result<ProjectionFacts, MachineError> {
    if snapshot.projection.lifecycle != domain::RunLifecycle::Running
        || snapshot.projection.thread_id.is_none()
        || snapshot.projection.active_turn_id.is_none()
        || effective_policy
            .thread_generation
            .is_none_or(|generation| generation == 0)
        || effective_policy.server_epoch.is_none()
        || effective_policy.server_epoch != snapshot.app_server_epoch
        || snapshot.stamp.run_state_revision != snapshot.projection.ledger_head.sequence
        || snapshot.stamp.writer_state_revision != snapshot.writer.authority_revision
    {
        return Err(missing("immutable accepted operation projection boundary"));
    }
    let dedicated = snapshot.manifest.execution_lane == domain::ExecutionLane::Dedicated;
    Ok(ProjectionFacts {
        effective_policy: effective_policy.clone(),
        // A correlated accepted Turn proves the addressed server responded at this boundary.
        lane_state: pb::ServerLaneState::Ready,
        process_generation: None,
        socket_identity_sha256: None,
        background: Some(BackgroundFacts {
            state: if dedicated {
                pb::BackgroundExecutionState::Unverified
            } else {
                pb::BackgroundExecutionState::NotApplicable
            },
            mechanism: if dedicated {
                pb::BackgroundExecutionMechanism::DedicatedLaneProcessCensus
            } else {
                pb::BackgroundExecutionMechanism::SharedProfileAggregate
            },
            census_revision: 0,
            observed_process_count: 0,
            quiescent_since: None,
            consecutive_empty_samples: 0,
        }),
        active_turn_status: Some(pb::TurnStatus::Accepted),
        last_final_response: None,
    })
}
