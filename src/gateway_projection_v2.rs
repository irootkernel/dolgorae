//! Pure conversion of captured semantic facts into the checked public protocol.

use crate::domain;
use crate::machine::MachineError;
use crate::protocol::public_v2 as pb;
use crate::snapshot::RunSnapshot;
use crate::writer::WriterAuthorityState;
use base64::Engine;

use crate::gateway_projection::ProjectionFacts;

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

pub fn configuration(
    snapshot: &RunSnapshot,
) -> Result<pb::RunConfigurationProjection, MachineError> {
    let manifest = &snapshot.manifest;
    let legacy = manifest.instructions.legacy();
    let mut required_capabilities = manifest.required_capabilities.clone();
    required_capabilities.sort();
    required_capabilities.dedup();
    Ok(pb::RunConfigurationProjection {
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
            schema_id: match &manifest.instructions {
                crate::run::RunInstructionSnapshot::Legacy(value) => value.schema.clone(),
                crate::run::RunInstructionSnapshot::Caller(value) => value.schema.clone(),
                crate::run::RunInstructionSnapshot::Delegation(value) => value.schema.clone(),
            },
            common_prefix_version: legacy.map_or(0, |i| i.common_prefix_version),
            mode_prefix_version: legacy.map_or(0, |i| i.mode_prefix_version),
            purpose_prefix_version: legacy.map_or(0, |i| i.purpose_prefix_version),
        }),
        controller_instructions_byte_length: legacy.map_or(0, |i| i.normalized_byte_length),
        controller_instructions_sha256: manifest.instructions.digest().to_owned(),
    })
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
    } else if facts.lane_state as i32 == pb::ServerLaneState::Failed as i32 {
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
                    .and_then(|status| pb::TurnStatus::try_from(status as i32).ok())
                    .ok_or_else(|| missing("active Turn status"))?,
                &snapshot.stamp.captured_head_cursor,
                None,
            )
        })
        .transpose()?;
    if facts.lane_state as i32 == pb::ServerLaneState::Unspecified as i32 {
        return Err(missing("server lane state"));
    }
    if facts.background.is_none() {
        return Err(missing("background execution observation"));
    }
    let background_execution = facts
        .background
        .as_ref()
        .map(|value| {
            if value.state as i32 == pb::BackgroundExecutionState::Unspecified as i32
                || value.mechanism as i32 == pb::BackgroundExecutionMechanism::Unspecified as i32
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
        pending_interaction_count: u32::try_from(
            state.pending_requests.len()
                + snapshot
                    .broker_approvals()?
                    .iter()
                    .filter(|b| b.status == "pending")
                    .count(),
        )
        .map_err(|_| missing("bounded pending interaction count"))?,
        recovery: Some(if facts.profile_cleanup_required {
            pb::RecoveryProjection {
                state: pb::RecoveryState::Required as i32,
                required_action: pb::RecoveryAction::OperatorRepair as i32,
                blocker_code: Some("profile_membership_cleanup_required".to_owned()),
            }
        } else {
            recovery(snapshot)
        }),
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
        configuration: Some(configuration(snapshot)?),
        stamp: Some(stamp(&snapshot.stamp)),
        requested_mode: match manifest.requested_access {
            Some(domain::CallerWorkspaceAccess::Writable) => pb::CreationMode::Writable as i32,
            _ => pb::CreationMode::ReadOnly as i32,
        },
        admitted_configuration: manifest
            .agent_configuration
            .admitted_configuration
            .as_ref()
            .map(admitted_configuration)
            .transpose()?,
        manifest_schema: format!("dolgorae.run-manifest/v{}", manifest.schema_version),
        record_compatibility: if manifest.schema_version <= 2 {
            pb::RecordCompatibility::AcceptedLegacyReadRecoverClose
        } else {
            pb::RecordCompatibility::Modern
        } as i32,
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
    let background_execution_blocker = match facts
        .background
        .as_ref()
        .and_then(|value| pb::BackgroundExecutionState::try_from(value.state as i32).ok())
    {
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

pub fn admitted_configuration(
    value: &crate::delegation_identity::AdmittedConfiguration,
) -> Result<pb::AdmittedConfiguration, MachineError> {
    value.digest()?;
    fn named(prefix: &str, value: &str) -> String {
        format!("{prefix}{}", value.to_ascii_uppercase())
    }
    Ok(pb::AdmittedConfiguration {
        schema: value.schema.clone(),
        profile_binding_schema: value.profile_binding_schema.clone(),
        profile_binding_digest: value.profile_binding_digest.clone(),
        model: value.model.clone(),
        effort: value.effort.clone(),
        purpose_kind: pb::PurposeKind::from_str_name(&named("PURPOSE_KIND_", &value.purpose_kind))
            .ok_or_else(|| missing("purpose"))? as i32,
        purpose_label: value.purpose_label.clone(),
        required_capabilities: value.required_capabilities.clone(),
        execution_lane: pb::ExecutionLane::from_str_name(&named(
            "EXECUTION_LANE_",
            &value.execution_lane,
        ))
        .ok_or_else(|| missing("lane"))? as i32,
        requested_access: if value.requested_access == "read_only" {
            pb::CreationMode::ReadOnly
        } else {
            pb::CreationMode::Writable
        } as i32,
        required_assurance: pb::AssuranceLevel::from_str_name(&named(
            "ASSURANCE_LEVEL_",
            &value.required_assurance,
        ))
        .ok_or_else(|| missing("assurance"))? as i32,
        native_subagent_policy: if value.native_subagent_policy == "disabled" {
            pb::NativeSubagentPolicy::Disabled
        } else {
            pb::NativeSubagentPolicy::Enabled
        } as i32,
        instruction_identity_digest: value.instruction_identity_digest.clone(),
    })
}

/// Controller-supplied provisioning capture. Run observer projections never use it.
pub fn captured_instructions(
    value: &crate::delegation_identity::InstructionSnapshot,
) -> pb::CapturedInstructions {
    fn bytes(value: &crate::delegation_identity::BytesRef) -> pb::BytesRef {
        pb::BytesRef {
            byte_length: value.byte_length,
            sha256: value.sha256.clone(),
        }
    }
    fn named(value: &crate::delegation_identity::NamedSource) -> pb::NamedSource {
        pb::NamedSource {
            name: value.name.clone(),
            content: Some(bytes(&value.content)),
        }
    }
    fn source(name: &str, body: &str) -> pb::CapturedSource {
        pb::CapturedSource {
            name: name.into(),
            utf8: body.as_bytes().to_vec(),
            content: Some(bytes(&crate::delegation_identity::BytesRef::from_bytes(
                body.as_bytes(),
            ))),
        }
    }
    let role = &value.role_snapshot;
    let identity = &value.instruction_identity;
    pb::CapturedInstructions {
        role_name: value.role.name.clone(),
        common: Some(source("common", &value.common.body)),
        role_body: Some(source(&value.role.name, &value.role.body)),
        dependencies: value
            .dependencies
            .iter()
            .map(|s| source(&s.name, &s.source.body))
            .collect(),
        selected_guidance: value
            .selected_guidance
            .iter()
            .map(|s| source(&s.name, &s.source.body))
            .collect(),
        role_snapshot: Some(pb::RoleSnapshot {
            schema: role.schema.clone(),
            role_body: Some(bytes(&role.role_body)),
            dependencies: role.dependencies.iter().map(named).collect(),
            selected_guidance: role.selected_guidance.iter().map(named).collect(),
        }),
        role_snapshot_digest: value.role_snapshot_digest.clone(),
        instruction_identity: Some(pb::InstructionIdentity {
            schema: identity.schema.clone(),
            core_version: identity.core_version as u32,
            core: Some(bytes(&identity.core)),
            common: Some(bytes(&identity.common)),
            role_snapshot_digest: identity.role_snapshot_digest.clone(),
            dependency_order: identity.dependency_order.clone(),
            guidance_order: identity.guidance_order.clone(),
            composition_version: identity.composition_version.clone(),
            composition: Some(bytes(&identity.composition)),
        }),
        instruction_identity_digest: value.instruction_identity_digest.clone(),
    }
}
