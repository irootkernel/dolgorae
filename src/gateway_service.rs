//! Public request decoding and typed dispatch into the shared semantic service.
use crate::controller::{CredentialCarrier, authorize_controller, binding_from_carrier};
use crate::darwin::DarwinSystem;
use crate::domain;
use crate::gateway::GatewayBackend;
use crate::gateway_projection::{self as project, ProjectionFacts};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::protocol::public_v1 as pb;
use crate::semantic::{
    BrokerApprovalInteractionSnapshot, CoreSemanticService, RunMutationInput, RunMutationOperation,
    RunMutationReceipt, StartRunInput, broker_approval_interaction, broker_approval_interactions,
    resolve_broker_approval, validate_broker_approval_response_size,
};
use crate::snapshot::RunSnapshot;
use crate::workspace::{WorkspaceService, WorkspaceView};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct CoreGatewayBackend {
    pub(crate) home: DolgoraeHome,
    instance_id: Uuid,
}

impl CoreGatewayBackend {
    pub fn new(home: DolgoraeHome, instance_id: Uuid) -> Self {
        Self { home, instance_id }
    }

    pub(crate) fn context(&self) -> pb::ResponseContext {
        pb::ResponseContext {
            protocol_version: 1,
            server_instance_id: self.instance_id.to_string(),
            operation_id: None,
        }
    }

    pub(crate) fn workspace(
        &self,
        reference: &pb::WorkspaceRef,
    ) -> Result<WorkspaceView, MachineError> {
        absolute(&reference.absolute_path, "workspace.absolute_path")?;
        if reference.expected_workspace_id.is_empty() {
            return Err(invalid(
                "workspace.expected_workspace_id",
                "workspace identity is required",
            ));
        }
        let view =
            WorkspaceService::system()?.discover(Some(Path::new(&reference.absolute_path)))?;
        if view.workspace_id != reference.expected_workspace_id {
            return Err(invalid(
                "workspace.expected_workspace_id",
                "workspace identity does not match canonical path",
            ));
        }
        Ok(view)
    }

    pub(crate) fn run_snapshot(
        &self,
        reference: &pb::RunRef,
    ) -> Result<(PathBuf, RunSnapshot, ProjectionFacts), MachineError> {
        let view = self.workspace(required(reference.workspace.as_ref(), "run.workspace")?)?;
        let run_id = uuid(&reference.run_id, "run.run_id")?;
        let root = self.home.workspace_root(&view.workspace_id);
        let (snapshot, observation) = RunSnapshot::observe(&root, run_id, 0)?;
        if snapshot.manifest.workspace_id != view.workspace_id {
            return Err(invalid("run.run_id", "Run belongs to another workspace"));
        }
        let facts = ProjectionFacts::from_observation(&snapshot, &observation)?;
        Ok((root, snapshot, facts))
    }

    fn run_state(&self, reference: &pb::RunRef) -> Result<(PathBuf, RunSnapshot), MachineError> {
        let view = self.workspace(required(reference.workspace.as_ref(), "run.workspace")?)?;
        let root = self.home.workspace_root(&view.workspace_id);
        let snapshot = RunSnapshot::load(&root, uuid(&reference.run_id, "run.run_id")?, 0)?;
        if snapshot.manifest.workspace_id != view.workspace_id {
            return Err(invalid("run.run_id", "Run belongs to another workspace"));
        }
        Ok((root, snapshot))
    }

    pub(crate) fn controller(
        &self,
        reference: &pb::ControllerCarrierRef,
    ) -> Result<CredentialCarrier, MachineError> {
        let path = absolute(
            &reference.absolute_file_path,
            "controller.absolute_file_path",
        )?;
        let root = self.home.root().join("controller-carriers");
        let carrier = CredentialCarrier::open_confined(&root, path)?;
        let expected = uuid(
            &reference.expected_controller_id,
            "controller.expected_controller_id",
        )?;
        if reference.expected_controller_generation == 0 {
            return Err(invalid(
                "controller.expected_controller_generation",
                "generation must be positive",
            ));
        }
        let binding = binding_from_carrier(&carrier, reference.expected_controller_generation)?;
        if binding.identity.controller_id != expected {
            return Err(invalid(
                "controller.expected_controller_id",
                "carrier identity does not match",
            ));
        }
        Ok(carrier.with_expected_generation(reference.expected_controller_generation))
    }

    pub(crate) fn run_controller(
        &self,
        snapshot: &RunSnapshot,
        reference: &pb::ControllerCarrierRef,
    ) -> Result<CredentialCarrier, MachineError> {
        self.run_controller_for(snapshot, reference, "run.interaction.get")
    }

    fn run_controller_for(
        &self,
        snapshot: &RunSnapshot,
        reference: &pb::ControllerCarrierRef,
        operation: &str,
    ) -> Result<CredentialCarrier, MachineError> {
        let carrier = self.controller(reference)?;
        if reference.expected_controller_generation != snapshot.controller.generation {
            return Err(controller_mismatch(snapshot.manifest.run_id));
        }
        authorize_controller(
            snapshot.manifest.run_id,
            operation,
            snapshot.controller_authority()?,
            &carrier,
        )?;
        Ok(carrier)
    }

    pub(crate) fn mutate(
        &self,
        reference: &pb::RunRef,
        controller: &pb::ControllerCarrierRef,
        revision: Option<u64>,
        operation: RunMutationOperation,
    ) -> Result<RunMutationReceipt, MachineError> {
        let view = self.workspace(required(reference.workspace.as_ref(), "run.workspace")?)?;
        let run_id = uuid(&reference.run_id, "run.run_id")?;
        let carrier = self.controller(controller)?;
        let binding = crate::controller::load_reconciled_controller_binding(
            &self.home.workspace_root(&view.workspace_id),
            run_id,
        )?;
        if binding.identity.generation != controller.expected_controller_generation {
            return Err(controller_mismatch(run_id));
        }
        CoreSemanticService.mutate_run(RunMutationInput {
            workspace: view.canonical_path.to_path_buf()?,
            expected_workspace_id: view.workspace_id,
            run_id,
            expected_state_revision: revision,
            carrier,
            operation,
        })
    }

    fn mutation_response(
        &self,
        reference: &pb::RunRef,
        controller: &pb::ControllerCarrierRef,
        revision: u64,
        operation: RunMutationOperation,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        self.mutate(reference, controller, Some(revision), operation)?;
        let (_, snapshot, facts) = self.run_snapshot(reference)?;
        Ok(pb::RunMutationResponse {
            context: Some(self.context()),
            run: Some(project::run_projection(&snapshot, &facts)?),
        })
    }

    fn orchestration_close_recovery_response(
        &self,
        reference: &pb::RunRef,
        controller: &pb::ControllerCarrierRef,
        expected_state_revision: u64,
        operation: RunMutationOperation,
    ) -> Result<Option<pb::RunMutationResponse>, MachineError> {
        let gate = orchestration_close_gate(uuid(&reference.run_id, "run.run_id")?)?;
        let _guard = gate
            .lock()
            .map_err(|_| internal_projection("orchestration close gate"))?;
        let (state_root, snapshot) = self.run_state(reference)?;
        let Some(binding) = snapshot
            .manifest
            .aggregate_binding
            .as_ref()
            .filter(|binding| {
                binding.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                    && binding.member_kind == crate::run::AggregateMemberKind::Primary
                    && binding.aggregate_id == snapshot.manifest.run_id
            })
        else {
            return Ok(None);
        };
        let carrier = self.run_controller_for(&snapshot, controller, "run.recover")?;
        if snapshot.stamp.run_state_revision != expected_state_revision {
            return Err(MachineError::new(
                "RUN_STATE_CONFLICT",
                "run recovery expected state revision is stale",
                false,
                serde_json::json!({
                    "run_id":snapshot.manifest.run_id,
                    "expected_state_revision":expected_state_revision,
                    "actual_state_revision":snapshot.stamp.run_state_revision
                }),
            ));
        }
        let mut store = open_orchestration_mutator(&state_root)?;
        let session = store.session(snapshot.manifest.run_id)?;
        if binding.operation_id != session.bootstrap_operation_id
            || binding.policy_sha256.as_deref() != Some(session.specialist_policy_sha256.as_str())
        {
            return Err(internal_projection("Orchestrated Session root binding"));
        }
        let Some(close) = store.session_close(snapshot.manifest.run_id)? else {
            return Ok(None);
        };
        snapshot.authorize_current_controller(&state_root, &carrier, "run.recover")?;
        if matches!(
            snapshot.projection.lifecycle,
            domain::RunLifecycle::ReconciliationRequired | domain::RunLifecycle::OutcomeUnknown
        ) && let Err(mut error) = self.mutate(
            reference,
            controller,
            Some(expected_state_revision),
            operation,
        ) {
            let _ = store.record_session_close_failure(snapshot.manifest.run_id, &error.code);
            if let Some(details) = error.details.as_object_mut() {
                details.insert(
                    "operation_id".to_owned(),
                    serde_json::Value::String(close.operation_id.to_string()),
                );
            }
            return Err(error);
        }
        store.reconcile_unknown_work(snapshot.manifest.run_id)?;
        let mut effects = crate::semantic::ProductionOrchestrationEffects::new(&state_root);
        if let Err(mut error) = store.settle_session_close(snapshot.manifest.run_id, &mut effects) {
            if let Some(details) = error.details.as_object_mut() {
                details.insert(
                    "operation_id".to_owned(),
                    serde_json::Value::String(close.operation_id.to_string()),
                );
                details.insert(
                    "run_id".to_owned(),
                    serde_json::Value::String(snapshot.manifest.run_id.to_string()),
                );
            }
            return Err(error);
        }
        let (_, current) = self.run_state(reference)?;
        if current.projection.lifecycle != domain::RunLifecycle::Closed
            && let Err(mut error) = self.mutate(
                reference,
                controller,
                Some(current.stamp.run_state_revision),
                RunMutationOperation::Close {
                    interrupt: close.interrupt,
                },
            )
        {
            if error.code == "RUN_BUSY" {
                return Err(session_close_in_progress(
                    snapshot.manifest.run_id,
                    close.operation_id,
                ));
            }
            let (_, after_error) = self.run_state(reference)?;
            if after_error.projection.lifecycle != domain::RunLifecycle::Closed {
                if error.code == "RUN_STATE_CONFLICT" {
                    return Err(session_close_in_progress(
                        snapshot.manifest.run_id,
                        close.operation_id,
                    ));
                }
                let _ = store.record_session_close_failure(snapshot.manifest.run_id, &error.code);
                if let Some(details) = error.details.as_object_mut() {
                    details.insert(
                        "operation_id".to_owned(),
                        serde_json::Value::String(close.operation_id.to_string()),
                    );
                }
                return Err(error);
            }
        }
        store.complete_session_close(snapshot.manifest.run_id)?;
        let (_, snapshot, facts) = self.run_snapshot(reference)?;
        let mut context = self.context();
        context.operation_id = Some(close.operation_id.to_string());
        Ok(Some(pb::RunMutationResponse {
            context: Some(context),
            run: Some(project::run_projection(&snapshot, &facts)?),
        }))
    }

    fn profile(&self, name: &str) -> Result<pb::ProfileProjection, MachineError> {
        let observation = crate::profile::observe_global_profile(&self.home, name)?;
        let blockers = observation
            .blockers
            .into_iter()
            .map(|blocker| match blocker {
                crate::profile::ProfileObservationBlocker::ServerUnavailable => {
                    pb::CapabilityBlocker {
                        code: pb::CapabilityBlockerCode::ProfileServerUnavailable as i32,
                        safe_message: "Profile server is unavailable".to_owned(),
                    }
                }
                crate::profile::ProfileObservationBlocker::RuntimeIncompatible => {
                    pb::CapabilityBlocker {
                        code: pb::CapabilityBlockerCode::ProfileRuntimeIncompatible as i32,
                        safe_message: "Profile runtime is incompatible".to_owned(),
                    }
                }
            })
            .collect();
        project::profile(&project::ProfileFacts {
            snapshot: observation.snapshot,
            server_epoch: observation.server_epoch,
            models: observation
                .models
                .into_iter()
                .map(|model| project::ProfileModelFacts {
                    model_id: model.model_id,
                    is_default: model.is_default,
                    supported_efforts: model.supported_efforts,
                })
                .collect(),
            capabilities: crate::runtime::capabilities(),
            blockers,
        })
    }

    fn workspace_writer(
        &self,
        reference: &pb::WorkspaceRef,
    ) -> Result<pb::WriterState, MachineError> {
        let view = self.workspace(reference)?;
        let root = self.home.workspace_root(&view.workspace_id);
        let writer =
            crate::writer::WriterStore::new(&root, &view.workspace_id, DarwinSystem.current_uid())
                .load()?;
        if let Some(holder) = writer.holder.as_ref() {
            let (_, snapshot, facts) = self.run_snapshot(&pb::RunRef {
                workspace: Some(reference.clone()),
                run_id: holder.run_id.to_string(),
            })?;
            project::writer_state(&snapshot, &facts, self.context())
        } else {
            project::ownerless_writer_state(&writer, self.context())
        }
    }
}

fn open_orchestration_mutator(
    state_root: &Path,
) -> Result<crate::orchestration::OrchestrationStore, MachineError> {
    static OPEN_SERIALIZER: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = OPEN_SERIALIZER
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| internal_projection("orchestration open serializer"))?;
    crate::orchestration::OrchestrationStore::open(state_root)
}

fn orchestration_close_gate(run_id: Uuid) -> Result<Arc<Mutex<()>>, MachineError> {
    static GATES: OnceLock<Mutex<HashMap<Uuid, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| internal_projection("orchestration close gate registry"))?;
    gates.retain(|_, gate| gate.strong_count() != 0);
    if let Some(gate) = gates.get(&run_id).and_then(Weak::upgrade) {
        return Ok(gate);
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(run_id, Arc::downgrade(&gate));
    Ok(gate)
}

pub(crate) fn invalid(field: &str, reason: &str) -> MachineError {
    MachineError::invalid_argument(field, reason)
}
pub(crate) fn required<'a, T>(value: Option<&'a T>, field: &str) -> Result<&'a T, MachineError> {
    value.ok_or_else(|| invalid(field, "field is required"))
}
pub(crate) fn uuid(value: &str, field: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value).map_err(|_| invalid(field, "expected UUID"))
}
fn controller_mismatch(run_id: Uuid) -> MachineError {
    MachineError::new(
        "CONTROLLER_MISMATCH",
        "controller generation does not authorize this operation",
        false,
        serde_json::json!({"run_id":run_id,"operation":"rpc.controller.authorize"}),
    )
}

fn timestamp_ms(value: i64) -> Result<prost_types::Timestamp, MachineError> {
    if value < 0 {
        return Err(MachineError::new(
            "INTERNAL_ERROR",
            "broker interaction timestamp is invalid",
            false,
            serde_json::json!({"invariant":"nonnegative timestamp"}),
        ));
    }
    Ok(prost_types::Timestamp {
        seconds: value / 1_000,
        nanos: ((value % 1_000) * 1_000_000) as i32,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultPageCursor {
    schema_version: u32,
    session_id: Uuid,
    projection_version: u32,
    captured_publication_head: u64,
    after_publication_order: u64,
}

fn result_cursor_mac(payload: &[u8], controller_digest: &str) -> String {
    let mut authenticated = Vec::with_capacity(payload.len() + controller_digest.len() + 40);
    authenticated.extend_from_slice(b"dolgorae-result-page-cursor-v1\0");
    authenticated.extend_from_slice(payload);
    authenticated.push(0);
    authenticated.extend_from_slice(controller_digest.as_bytes());
    sha256_hex(&authenticated)
}

fn encode_result_cursor(
    cursor: &ResultPageCursor,
    controller_digest: &str,
) -> Result<String, MachineError> {
    let payload = serde_json::to_vec(cursor).map_err(|_| {
        MachineError::new(
            "INTERNAL_ERROR",
            "result cursor could not be encoded",
            false,
            serde_json::json!({"invariant":"result cursor encoding"}),
        )
    })?;
    Ok(format!(
        "{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload),
        result_cursor_mac(&payload, controller_digest)
    ))
}

fn decode_result_cursor(
    token: &str,
    session_id: Uuid,
    projection_version: u32,
    controller_digest: &str,
) -> Result<ResultPageCursor, MachineError> {
    let invalid_cursor = || {
        MachineError::invalid_argument(
            "page_cursor",
            "result page cursor is malformed or belongs to another observation",
        )
    };
    if token.is_empty() || token.len() > 2_048 {
        return Err(invalid_cursor());
    }
    let (encoded, mac) = token.split_once('.').ok_or_else(invalid_cursor)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid_cursor())?;
    if payload.len() > 1_024 || mac != result_cursor_mac(&payload, controller_digest) {
        return Err(invalid_cursor());
    }
    let cursor: ResultPageCursor =
        serde_json::from_slice(&payload).map_err(|_| invalid_cursor())?;
    if cursor.schema_version != 1
        || cursor.session_id != session_id
        || cursor.projection_version != projection_version
        || cursor.after_publication_order > cursor.captured_publication_head
    {
        return Err(invalid_cursor());
    }
    Ok(cursor)
}

fn project_orchestrated_session(
    value: crate::orchestration::OrchestratedSessionObservation,
    workspace: pb::WorkspaceRef,
) -> Result<pb::OrchestratedSessionProjection, MachineError> {
    let lifecycle = match value.status.as_str() {
        "creating" => pb::OrchestratedSessionLifecycle::Creating,
        "active" => pb::OrchestratedSessionLifecycle::Active,
        "degraded" => pb::OrchestratedSessionLifecycle::Degraded,
        "recovering" => pb::OrchestratedSessionLifecycle::Recovering,
        "completing" => pb::OrchestratedSessionLifecycle::Completing,
        "aborting" => pb::OrchestratedSessionLifecycle::Aborting,
        "completed" => pb::OrchestratedSessionLifecycle::Completed,
        "aborted" => pb::OrchestratedSessionLifecycle::Aborted,
        _ => return Err(internal_projection("orchestrated session lifecycle")),
    };
    let composition = match value.composition_state.as_str() {
        "standalone_primary" => pb::OrchestratedSessionComposition::StandalonePrimary,
        "brokered_hierarchy" => pb::OrchestratedSessionComposition::BrokeredHierarchy,
        _ => return Err(internal_projection("orchestrated session composition")),
    };
    let approval_policy = match value.approval_policy.as_str() {
        "user_approval_required" => pb::OrchestratedSessionApprovalPolicy::UserApprovalRequired,
        "fully_delegated" => pb::OrchestratedSessionApprovalPolicy::FullyDelegated,
        _ => return Err(internal_projection("orchestrated session approval policy")),
    };
    let close_intent = match value.close_interrupt {
        Some(true) => pb::SessionCloseIntent::Abort,
        Some(false) => pb::SessionCloseIntent::Complete,
        None => pb::SessionCloseIntent::None,
    };
    let close_progress = match value.close_progress.as_deref() {
        Some("settling") => pb::SessionCloseProgress::Settling,
        Some("completed") => pb::SessionCloseProgress::Completed,
        Some("aborted") => pb::SessionCloseProgress::Aborted,
        Some("recovery_required") => pb::SessionCloseProgress::RecoveryRequired,
        Some("outcome_unknown") => pb::SessionCloseProgress::OutcomeUnknown,
        Some(_) => return Err(internal_projection("orchestrated session close progress")),
        None if value.status == "recovering" => pb::SessionCloseProgress::RecoveryRequired,
        None => pb::SessionCloseProgress::None,
    };
    let recovery_required = matches!(
        close_progress,
        pb::SessionCloseProgress::RecoveryRequired | pb::SessionCloseProgress::OutcomeUnknown
    ) || matches!(value.status.as_str(), "degraded" | "recovering");
    Ok(pb::OrchestratedSessionProjection {
        session_id: value.session_id.to_string(),
        primary_run: Some(pb::RunRef {
            workspace: Some(workspace),
            run_id: value.root_run_id.to_string(),
        }),
        aggregate_revision: value.aggregate_revision,
        lifecycle: lifecycle as i32,
        composition: composition as i32,
        approval_policy: approval_policy as i32,
        specialist_policy_name: value.specialist_policy_name,
        specialist_policy_revision: value.specialist_policy_revision,
        specialist_policy_sha256: value.specialist_policy_sha256,
        nonretired_member_count: value.nonretired_member_count,
        nonterminal_spawn_count: value.nonterminal_spawn_count,
        pending_approval_count: value.pending_approval_count,
        accepted_unfinished_task_count: value.accepted_unfinished_task_count,
        unknown_outcome_task_count: value.unknown_outcome_task_count,
        published_result_count: value.published_result_count,
        close_intent: close_intent as i32,
        close_progress: close_progress as i32,
        close_operation_id: value.close_operation_id.map(|value| value.to_string()),
        recovery_classification: if recovery_required {
            pb::RecoveryClassification::ReconcileRequired
        } else {
            pb::RecoveryClassification::None
        } as i32,
        required_action: if recovery_required {
            pb::RequiredClientAction::ReconcileRun
        } else {
            pb::RequiredClientAction::None
        } as i32,
        captured_at: Some(timestamp_ms(value.captured_at_ms)?),
        source_revision: value.aggregate_revision,
        availability: pb::OrchestratedSessionAvailability::Available as i32,
    })
}

fn project_published_result(
    value: crate::orchestration::PublishedResultObservation,
    workspace: pb::WorkspaceRef,
) -> Result<pb::OrchestratedSessionResult, MachineError> {
    Ok(pb::OrchestratedSessionResult {
        result_id: value.result_id.to_string(),
        task_id: value.task_id.to_string(),
        specialist_run: Some(pb::RunRef {
            workspace: Some(workspace.clone()),
            run_id: value.specialist_run_id.to_string(),
        }),
        specialist_role: value.specialist_role,
        publication_order: value.publication_order,
        published_at: Some(timestamp_ms(value.published_at_ms)?),
        format: pb::OrchestratedResultFormat::Utf8Text as i32,
        byte_length: value.byte_length,
        sha256: value.sha256.clone(),
        artifact: Some(pb::ArtifactRef {
            artifact_id: value.artifact_id.to_string(),
            kind: pb::ArtifactKind::FinalResponse as i32,
            visibility: pb::ArtifactVisibility::ControllerOnly as i32,
            media_type: "text/plain; charset=utf-8".to_owned(),
            byte_length: value.byte_length,
            sha256: value.sha256,
        }),
        artifact_owner: Some(pb::RunRef {
            workspace: Some(workspace),
            run_id: value.artifact_owner_run_id.to_string(),
        }),
    })
}

fn internal_projection(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable orchestration observation cannot be projected",
        false,
        serde_json::json!({"invariant":invariant}),
    )
}

fn session_close_in_progress(run_id: Uuid, operation_id: Uuid) -> MachineError {
    MachineError::new(
        "SESSION_CLOSE_IN_PROGRESS",
        "whole-session close was accepted and is still settling",
        false,
        serde_json::json!({
            "run_id":run_id,
            "operation_id":operation_id,
            "required_action":"refresh_snapshot"
        }),
    )
}

fn orchestrated_root_binding(
    snapshot: &RunSnapshot,
) -> Result<&crate::run::AggregateBinding, MachineError> {
    snapshot
        .manifest
        .aggregate_binding
        .as_ref()
        .filter(|binding| {
            binding.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                && binding.member_kind == crate::run::AggregateMemberKind::Primary
                && binding.aggregate_id == snapshot.manifest.run_id
        })
        .ok_or_else(|| invalid("root_run.run_id", "Run is not an Orchestrated Session root"))
}

fn broker_approval_pending(interaction: &BrokerApprovalInteractionSnapshot) -> bool {
    interaction.decision.is_none() && interaction.operation_state == "awaiting_approval"
}

fn broker_approval_summary(
    interaction: &BrokerApprovalInteractionSnapshot,
    snapshot: &RunSnapshot,
) -> Result<pb::InteractionSummary, MachineError> {
    let pending = broker_approval_pending(interaction);
    Ok(pb::InteractionSummary {
        interaction_id: interaction.approval_request_id.to_string(),
        run_id: interaction.session_id.to_string(),
        kind: pb::InteractionKind::UserInput as i32,
        status: if pending {
            pb::InteractionStatus::Pending
        } else {
            pb::InteractionStatus::Resolved
        } as i32,
        safe_title: if pending {
            "User input requested"
        } else {
            "Interaction resolved"
        }
        .to_owned(),
        controller_kind: project::controller_kind(snapshot.controller.kind) as i32,
        requires_user_escalation: pending,
        contains_protected_input: false,
        created_at: Some(timestamp_ms(interaction.created_at_ms)?),
        expires_at: None,
        resolved_at: (!pending)
            .then(|| {
                timestamp_ms(interaction.resolved_at_ms.ok_or_else(|| {
                    MachineError::new(
                        "INTERNAL_ERROR",
                        "resolved broker interaction has no resolution timestamp",
                        false,
                        serde_json::json!({"invariant":"resolved_at_ms"}),
                    )
                })?)
            })
            .transpose()?,
        state_revision: interaction.state_revision,
    })
}

fn broker_approval_projection(
    interaction: &BrokerApprovalInteractionSnapshot,
    snapshot: &RunSnapshot,
    context: pb::ResponseContext,
) -> Result<pb::GetControllerInteractionResponse, MachineError> {
    use pb::controller_interaction::Payload;
    Ok(pb::GetControllerInteractionResponse {
        context: Some(context),
        interaction: Some(pb::ControllerInteraction {
            summary: Some(broker_approval_summary(interaction, snapshot)?),
            response_schema_id: "dolgorae.interaction.user-input/v1".to_owned(),
            stamp: Some(project::stamp(&snapshot.stamp)),
            payload: Some(Payload::UserInput(pb::UserInputInteraction {
                is_blocking: true,
                questions: vec![pb::InteractionQuestion {
                    id: "specialist_approval".to_owned(),
                    header: "Specialist".to_owned(),
                    question: format!(
                        "Approve a {} Specialist for this request?",
                        interaction.request.role_ref
                    ),
                    allows_other: false,
                    is_secret: false,
                    options: Some(pb::InteractionOptions {
                        items: vec![
                            pb::InteractionOption {
                                label: "approve".to_owned(),
                                description: "Create the policy-admitted Specialist.".to_owned(),
                            },
                            pb::InteractionOption {
                                label: "reject".to_owned(),
                                description: "Reject this Specialist request.".to_owned(),
                            },
                        ],
                    }),
                }],
            })),
            decisions: Vec::new(),
        }),
    })
}

fn broker_approval_decision(bytes: &[u8]) -> Result<bool, MachineError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
        MachineError::new(
            "INTERACTION_RESPONSE_INVALID",
            "interaction response does not match its checked schema",
            false,
            serde_json::json!({}),
        )
    })?;
    let answer = value
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("answers"))
        .and_then(serde_json::Value::as_object)
        .filter(|answers| answers.len() == 1)
        .and_then(|answers| answers.get("specialist_approval"))
        .and_then(serde_json::Value::as_object)
        .filter(|answer| answer.len() == 1)
        .and_then(|answer| answer.get("answers"))
        .and_then(serde_json::Value::as_array)
        .filter(|answers| answers.len() == 1)
        .and_then(|answers| answers[0].as_str());
    match answer {
        Some("approve") => Ok(true),
        Some("reject") => Ok(false),
        _ => Err(MachineError::new(
            "INTERACTION_RESPONSE_INVALID",
            "interaction response does not match its checked schema",
            false,
            serde_json::json!({}),
        )),
    }
}
fn input_enum<T: TryFrom<i32>>(field: &str, value: i32, maximum: u32) -> Result<T, MachineError> {
    T::try_from(value).map_err(|_| MachineError::new("UNSUPPORTED_SCHEMA_VERSION", "input enum is not supported by this public protocol", false,
        serde_json::json!({"schema":format!("dolgorae.public.v1.{field}"),"requested":u32::from_ne_bytes(value.to_ne_bytes()),"supported":(0..=maximum).collect::<Vec<_>>()})))
}
fn absolute<'a>(value: &'a str, field: &str) -> Result<&'a Path, MachineError> {
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            field,
            "expected an absolute path without traversal components",
        ));
    }
    Ok(path)
}

impl GatewayBackend for CoreGatewayBackend {
    fn watch_run_events(
        &self,
        request: pb::WatchRunEventsRequest,
    ) -> Result<crate::gateway::EventPage, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        crate::gateway_observation::watch_page(
            &root,
            &snapshot,
            &request.after_cursor,
            request.projection,
            request.projection_version,
        )
    }
    fn list_run_timeline_items(
        &self,
        request: pb::ListRunTimelineItemsRequest,
    ) -> Result<pb::ListRunTimelineItemsResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let controller = request.controller.as_ref().ok_or_else(|| {
            MachineError::interaction_full_payload_requires_controller(
                snapshot.manifest.run_id,
                "run.timeline.list",
            )
        })?;
        let carrier = self.run_controller(&snapshot, controller)?;
        crate::timeline::list(
            &root,
            &snapshot,
            &carrier,
            &request.after_cursor,
            request.limit,
            request.timeline_version,
            self.context(),
        )
    }
    fn get_orchestrated_session(
        &self,
        request: pb::GetOrchestratedSessionRequest,
    ) -> Result<pb::GetOrchestratedSessionResponse, MachineError> {
        let reference = required(request.root_run.as_ref(), "root_run")?;
        let workspace = required(reference.workspace.as_ref(), "root_run.workspace")?.clone();
        let (state_root, snapshot) = self.run_state(reference)?;
        let controller = request.controller.as_ref().ok_or_else(|| {
            MachineError::interaction_full_payload_requires_controller(
                snapshot.manifest.run_id,
                "run.respond",
            )
        })?;
        let carrier =
            self.run_controller_for(&snapshot, controller, "orchestration.session.observe")?;
        let binding = orchestrated_root_binding(&snapshot)?;
        let store = crate::orchestration::OrchestrationStore::open_observer(&state_root)?;
        let observation = store
            .observe_session(snapshot.manifest.run_id)
            .map_err(|error| {
                if error.code == "RUN_NOT_FOUND" {
                    invalid("root_run.run_id", "Run is not an Orchestrated Session root")
                } else {
                    error
                }
            })?;
        if binding.operation_id != observation.bootstrap_operation_id
            || binding.policy_sha256.as_deref()
                != Some(observation.specialist_policy_sha256.as_str())
        {
            return Err(internal_projection("Orchestrated Session root binding"));
        }
        snapshot.authorize_current_controller(
            &state_root,
            &carrier,
            "orchestration.session.observe",
        )?;
        Ok(pb::GetOrchestratedSessionResponse {
            context: Some(self.context()),
            session: Some(project_orchestrated_session(observation, workspace)?),
        })
    }
    fn list_orchestrated_session_results(
        &self,
        request: pb::ListOrchestratedSessionResultsRequest,
    ) -> Result<pb::ListOrchestratedSessionResultsResponse, MachineError> {
        if request.projection_version != 1 {
            return Err(MachineError::new(
                "UNSUPPORTED_SCHEMA_VERSION",
                "unsupported Orchestrated Session result projection version",
                false,
                serde_json::json!({
                    "schema":"dolgorae.public.v1.OrchestratedSessionResult",
                    "requested":request.projection_version,
                    "supported":[1]
                }),
            ));
        }
        let limit = if request.limit == 0 {
            100
        } else {
            request.limit
        };
        if limit > 500 {
            return Err(invalid("limit", "result page limit must be at most 500"));
        }
        let reference = required(request.root_run.as_ref(), "root_run")?;
        let workspace = required(reference.workspace.as_ref(), "root_run.workspace")?.clone();
        let (state_root, snapshot) = self.run_state(reference)?;
        let controller = required(request.controller.as_ref(), "controller")?;
        let carrier =
            self.run_controller_for(&snapshot, controller, "orchestration.results.list")?;
        let binding = orchestrated_root_binding(&snapshot)?;
        let controller_digest = snapshot.controller_authority()?.capability_sha256.clone();
        let cursor = request
            .page_cursor
            .as_deref()
            .map(|token| {
                decode_result_cursor(
                    token,
                    snapshot.manifest.run_id,
                    request.projection_version,
                    &controller_digest,
                )
            })
            .transpose()?;
        let store = crate::orchestration::OrchestrationStore::open_observer(&state_root)?;
        let page = store
            .observe_published_results(
                snapshot.manifest.run_id,
                cursor
                    .as_ref()
                    .map(|cursor| cursor.captured_publication_head),
                cursor
                    .as_ref()
                    .map_or(0, |cursor| cursor.after_publication_order),
                limit,
            )
            .map_err(|error| {
                if error.code == "RUN_NOT_FOUND" {
                    invalid("root_run.run_id", "Run is not an Orchestrated Session root")
                } else {
                    error
                }
            })?;
        if binding.operation_id != page.bootstrap_operation_id
            || binding.policy_sha256.as_deref() != Some(page.specialist_policy_sha256.as_str())
        {
            return Err(internal_projection("Orchestrated Session root binding"));
        }
        snapshot.authorize_current_controller(
            &state_root,
            &carrier,
            "orchestration.results.list",
        )?;
        let next_page_cursor = if page.has_more {
            Some(encode_result_cursor(
                &ResultPageCursor {
                    schema_version: 1,
                    session_id: snapshot.manifest.run_id,
                    projection_version: request.projection_version,
                    captured_publication_head: page.captured_publication_head,
                    after_publication_order: page
                        .items
                        .last()
                        .ok_or_else(|| internal_projection("result page continuation"))?
                        .publication_order,
                },
                &controller_digest,
            )?)
        } else {
            None
        };
        let items = page
            .items
            .into_iter()
            .map(|item| project_published_result(item, workspace.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(pb::ListOrchestratedSessionResultsResponse {
            context: Some(self.context()),
            captured_publication_head: page.captured_publication_head,
            source_revision: page.source_revision,
            captured_at: Some(timestamp_ms(page.captured_at_ms)?),
            items,
            next_page_cursor,
        })
    }
    fn list_pending_interactions(
        &self,
        request: pb::ListPendingInteractionsRequest,
    ) -> Result<pb::ListPendingInteractionsResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let mut response =
            crate::gateway_observation::list_pending(&root, &snapshot, self.context())?;
        if orchestrated_root_binding(&snapshot).is_ok() {
            for interaction in broker_approval_interactions(&root, snapshot.manifest.run_id)? {
                if broker_approval_pending(&interaction) {
                    response
                        .items
                        .push(broker_approval_summary(&interaction, &snapshot)?);
                }
            }
        }
        Ok(response)
    }
    fn get_controller_interaction(
        &self,
        request: pb::GetControllerInteractionRequest,
    ) -> Result<pb::GetControllerInteractionResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let controller = request.controller.as_ref().ok_or_else(|| {
            MachineError::interaction_full_payload_requires_controller(
                snapshot.manifest.run_id,
                "run.interaction.get",
            )
        })?;
        let carrier = self.run_controller(&snapshot, controller)?;
        snapshot.authorize_current_controller(&root, &carrier, "run.interaction.get")?;
        if let Ok(interaction_id) = Uuid::parse_str(&request.interaction_id)
            && orchestrated_root_binding(&snapshot).is_ok()
            && let Some(interaction) =
                broker_approval_interaction(&root, snapshot.manifest.run_id, interaction_id)?
        {
            return broker_approval_projection(&interaction, &snapshot, self.context());
        }
        crate::gateway_observation::get_interaction(
            &root,
            &snapshot,
            &request.interaction_id,
            &carrier,
            self.context(),
        )
    }
    fn resolve_interaction(
        &self,
        mut request: pb::ResolveInteractionRequest,
    ) -> Result<pb::ResolveInteractionResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let controller = required(request.controller.as_ref(), "controller")?;
        let carrier = self.run_controller(&snapshot, controller)?;
        snapshot.authorize_current_controller(&root, &carrier, "run.respond")?;
        if let Ok(interaction_id) = Uuid::parse_str(&request.interaction_id)
            && orchestrated_root_binding(&snapshot).is_ok()
            && broker_approval_interaction(&root, snapshot.manifest.run_id, interaction_id)?
                .is_some()
        {
            if request.idempotency_key.is_empty() || request.idempotency_key.len() > 256 {
                return Err(MachineError::invalid_argument(
                    "idempotency_key",
                    "idempotency key must contain 1 to 256 UTF-8 bytes",
                ));
            }
            let bytes = Zeroizing::new(std::mem::take(&mut request.response_json));
            validate_broker_approval_response_size(
                snapshot.manifest.run_id,
                &request.interaction_id,
                bytes.len(),
            )?;
            let approved = broker_approval_decision(&bytes)?;
            let resolution = resolve_broker_approval(
                &root,
                snapshot.manifest.run_id,
                interaction_id,
                approved,
                &request.idempotency_key,
            )?;
            let mut context = self.context();
            context.operation_id = Some(resolution.resolution_receipt_id.to_string());
            return Ok(pb::ResolveInteractionResponse {
                context: Some(context),
                interaction_id: request.interaction_id,
                status: pb::InteractionStatus::Resolved as i32,
                resolution_receipt: resolution.resolution_receipt_id.to_string(),
            });
        }
        let operation = crate::gateway_observation::resolve_operation(&mut request)?;
        let receipt = self.mutate(
            required(request.run.as_ref(), "run")?,
            required(request.controller.as_ref(), "controller")?,
            None,
            operation,
        )?;
        let Some(crate::worker::ControlResponseV1::Responded {
            resolution_receipt_id: Some(id),
            ..
        }) = receipt.response
        else {
            return Err(crate::snapshot::state_conflict(
                receipt.snapshot.manifest.run_id,
                receipt.snapshot.projection.lifecycle,
                "run.respond",
            ));
        };
        let mut context = self.context();
        context.operation_id = Some(id.to_string());
        Ok(pb::ResolveInteractionResponse {
            context: Some(context),
            interaction_id: request.interaction_id,
            status: pb::InteractionStatus::Resolved as i32,
            resolution_receipt: id.to_string(),
        })
    }
    fn get_artifact(
        &self,
        request: pb::GetArtifactRequest,
    ) -> Result<pb::GetArtifactResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let carrier = request
            .controller
            .as_ref()
            .map(|reference| self.run_controller(&snapshot, reference))
            .transpose()?;
        crate::gateway_observation::artifact_metadata(
            &root,
            &snapshot,
            &request.artifact_id,
            carrier.as_ref(),
            self.context(),
        )
    }
    fn read_artifact_chunk(
        &self,
        request: pb::ReadArtifactChunkRequest,
    ) -> Result<pb::ReadArtifactChunkResponse, MachineError> {
        let (root, snapshot) = self.run_state(required(request.run.as_ref(), "run")?)?;
        let carrier = request
            .controller
            .as_ref()
            .map(|reference| self.run_controller(&snapshot, reference))
            .transpose()?;
        crate::gateway_observation::artifact_chunk(
            &root,
            &snapshot,
            &request.artifact_id,
            (request.offset, request.length),
            carrier.as_ref(),
            self.context(),
        )
    }
    fn submit_turn(
        &self,
        request: pb::SubmitTurnRequest,
    ) -> Result<pb::SubmitTurnAccepted, MachineError> {
        input_enum::<pb::WriteIntent>("WriteIntent", request.write_intent, 2)?;
        let reference = required(request.run.as_ref(), "run")?;
        let write = match pb::WriteIntent::try_from(request.write_intent) {
            Ok(pb::WriteIntent::Read) => false,
            Ok(pb::WriteIntent::Write) => true,
            _ => {
                return Err(invalid(
                    "write_intent",
                    "explicit read or write intent is required",
                ));
            }
        };
        let images = request
            .images
            .into_iter()
            .map(|image| {
                input_enum::<pb::ImageDetail>("ImageDetail", image.detail, 3)?;
                let path =
                    absolute(&image.absolute_file_path, "images.absolute_file_path")?.to_path_buf();
                let detail = match pb::ImageDetail::try_from(image.detail) {
                    Ok(pb::ImageDetail::Auto) => crate::turn::ImageDetail::Auto,
                    Ok(pb::ImageDetail::Low) => crate::turn::ImageDetail::Low,
                    Ok(pb::ImageDetail::High) => crate::turn::ImageDetail::High,
                    _ => {
                        return Err(invalid(
                            "images.detail",
                            "explicit image detail is required",
                        ));
                    }
                };
                Ok(crate::worker::TurnControlImage { path, detail })
            })
            .collect::<Result<Vec<_>, MachineError>>()?;
        let receipt = self.mutate(
            reference,
            required(request.controller.as_ref(), "controller")?,
            Some(request.expected_state_revision),
            RunMutationOperation::Submit {
                request: crate::worker::TurnControlRequest {
                    normalized_request_sha256: None,
                    message: request.message,
                    idempotency_key: request.idempotency_key.clone(),
                    effort: request.effort,
                    images,
                    write,
                },
                write,
            },
        )?;
        let Some(crate::worker::ControlResponseV1::AcceptedReceipt {
            accepted,
            operation_id,
            effective_policy,
            ..
        }) = receipt.response
        else {
            return Err(crate::snapshot::state_conflict(
                receipt.snapshot.manifest.run_id,
                receipt.snapshot.projection.lifecycle,
                "run.submit",
            ));
        };
        let snapshot = receipt.snapshot;
        let facts = project::acceptance_facts(&snapshot, &effective_policy)?;
        let mut context = self.context();
        context.operation_id = Some(operation_id.to_string());
        Ok(pb::SubmitTurnAccepted {
            context: Some(context.clone()),
            accepted_turn: Some(project::turn_projection(
                snapshot.manifest.run_id,
                &accepted.thread_id,
                &accepted.turn_id,
                pb::TurnStatus::Accepted,
                &snapshot.stamp.captured_head_cursor,
                None,
            )?),
            run: Some(project::run_projection(&snapshot, &facts)?),
            writer: Some(project::writer_state(&snapshot, &facts, context)?),
            idempotency_key: request.idempotency_key,
            correlation_id: operation_id.to_string(),
        })
    }
    fn list_profiles(
        &self,
        _: pb::ListProfilesRequest,
    ) -> Result<pb::ListProfilesResponse, MachineError> {
        let registry = crate::global_profile::GlobalProfileStore::new(&self.home).load()?;
        let items = registry
            .profiles
            .keys()
            .map(|name| self.profile(name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(pb::ListProfilesResponse {
            context: Some(self.context()),
            items,
        })
    }
    fn get_profile(
        &self,
        r: pb::GetProfileRequest,
    ) -> Result<pb::GetProfileResponse, MachineError> {
        Ok(pb::GetProfileResponse {
            context: Some(self.context()),
            profile: Some(self.profile(&r.profile_name)?),
        })
    }
    fn list_runs(&self, r: pb::ListRunsRequest) -> Result<pb::ListRunsResponse, MachineError> {
        let view = self.workspace(required(r.workspace.as_ref(), "workspace")?)?;
        let controller = r
            .controller_id
            .as_deref()
            .map(|value| uuid(value, "controller_id"))
            .transpose()?;
        let items = CoreSemanticService
            .list_runs(&view, controller)?
            .into_iter()
            .map(|(snapshot, observation)| {
                let facts = ProjectionFacts::from_observation(&snapshot, &observation)?;
                project::run_projection(&snapshot, &facts)
            })
            .collect::<Result<Vec<_>, MachineError>>()?;
        Ok(pb::ListRunsResponse {
            context: Some(self.context()),
            items,
        })
    }
    fn get_workspace_writer_status(
        &self,
        r: pb::GetWorkspaceWriterStatusRequest,
    ) -> Result<pb::GetWorkspaceWriterStatusResponse, MachineError> {
        Ok(pb::GetWorkspaceWriterStatusResponse {
            writer: Some(self.workspace_writer(required(r.workspace.as_ref(), "workspace")?)?),
        })
    }
    fn acquire_writer(&self, r: pb::AcquireWriterRequest) -> Result<pb::WriterState, MachineError> {
        let reference = required(r.run.as_ref(), "run")?;
        self.mutate(
            reference,
            required(r.controller.as_ref(), "controller")?,
            Some(r.expected_state_revision),
            RunMutationOperation::AcquireWriter,
        )?;
        self.workspace_writer(required(reference.workspace.as_ref(), "run.workspace")?)
    }
    fn release_writer(&self, r: pb::ReleaseWriterRequest) -> Result<pb::WriterState, MachineError> {
        let reference = required(r.run.as_ref(), "run")?;
        self.mutate(
            reference,
            required(r.controller.as_ref(), "controller")?,
            Some(r.expected_state_revision),
            RunMutationOperation::ReleaseWriter,
        )?;
        self.workspace_writer(required(reference.workspace.as_ref(), "run.workspace")?)
    }
    fn get_capabilities(
        &self,
        _: pb::GetCapabilitiesRequest,
    ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
        project::capabilities(&crate::runtime::capabilities(), self.context())
    }

    fn inspect_workspace(
        &self,
        request: pb::InspectWorkspaceRequest,
    ) -> Result<pb::InspectWorkspaceResponse, MachineError> {
        absolute(&request.absolute_path, "absolute_path")?;
        let view = WorkspaceService::system()?.discover(Some(Path::new(&request.absolute_path)))?;
        if request
            .expected_workspace_id
            .as_ref()
            .is_some_and(|id| *id != view.workspace_id)
        {
            return Err(invalid(
                "expected_workspace_id",
                "workspace identity does not match",
            ));
        }
        Ok(pb::InspectWorkspaceResponse {
            context: Some(self.context()),
            workspace_id: view.workspace_id,
            canonical_path: Some(project::path(&view.canonical_path)?),
            mode: match view.mode {
                crate::workspace::WorkspaceMode::Git => pb::WorkspaceMode::Git,
                crate::workspace::WorkspaceMode::NonGit => pb::WorkspaceMode::NonGit,
            } as i32,
            status: pb::WorkspaceInspectionStatus::Compatible as i32,
            blockers: vec![],
        })
    }

    fn get_run(&self, request: pb::GetRunRequest) -> Result<pb::GetRunResponse, MachineError> {
        let (_, snapshot, facts) = self.run_snapshot(required(request.run.as_ref(), "run")?)?;
        Ok(pb::GetRunResponse {
            context: Some(self.context()),
            run: Some(project::run_projection(&snapshot, &facts)?),
        })
    }

    fn start_run(
        &self,
        request: pb::StartRunRequest,
    ) -> Result<pb::StartRunResponse, MachineError> {
        input_enum::<pb::ControlMode>("ControlMode", request.control_mode, 2)?;
        input_enum::<pb::ExecutionLane>("ExecutionLane", request.execution_lane, 2)?;
        input_enum::<pb::AssuranceLevel>("AssuranceLevel", request.required_assurance, 3)?;
        input_enum::<pb::PurposeKind>("PurposeKind", request.purpose, 8)?;
        if request.control_mode == pb::ControlMode::Unspecified as i32 {
            return Err(MachineError::new(
                "CONTROL_MODE_REQUIRED",
                "explicit control mode is required",
                false,
                serde_json::json!({"argument":"control_mode","reason":"control mode is unspecified"}),
            ));
        }
        if request.required_assurance == pb::AssuranceLevel::Unspecified as i32
            && request.execution_lane != 0
        {
            return Err(MachineError::new(
                "ASSURANCE_LEVEL_UNAVAILABLE",
                "explicit assurance is required",
                false,
                serde_json::json!({"run_id":null,"execution_lane":if request.execution_lane == pb::ExecutionLane::Dedicated as i32 {"dedicated"} else {"shared_readonly"},"reason":"assurance is unspecified","required_action":"lower_required_assurance"}),
            ));
        }
        let reference = required(request.workspace.as_ref(), "workspace")?;
        let view = self.workspace(reference)?;
        let controller = required(request.controller.as_ref(), "controller")?;
        if controller.expected_controller_generation != 1 {
            return Err(invalid(
                "controller.expected_controller_generation",
                "new Run requires initial generation 1",
            ));
        }
        let input = StartRunInput {
            workspace: Some(view.canonical_path.to_path_buf()?),
            expected_workspace_id: Some(view.workspace_id),
            profile: request.profile_name,
            control_mode: match pb::ControlMode::try_from(request.control_mode) {
                Ok(pb::ControlMode::DirectInteractive) => domain::ControlMode::DirectInteractive,
                Ok(pb::ControlMode::ManagedAgent) => domain::ControlMode::ManagedAgent,
                _ => return Err(invalid("control_mode", "unsupported control mode")),
            },
            execution_lane: match pb::ExecutionLane::try_from(request.execution_lane) {
                Ok(pb::ExecutionLane::SharedReadonly) => domain::ExecutionLane::SharedReadonly,
                Ok(pb::ExecutionLane::Dedicated) => domain::ExecutionLane::Dedicated,
                _ => return Err(invalid("execution_lane", "unsupported execution lane")),
            },
            assurance: match pb::AssuranceLevel::try_from(request.required_assurance) {
                Ok(pb::AssuranceLevel::BestEffortPersonalAlpha) => {
                    domain::Assurance::BestEffortPersonalAlpha
                }
                Ok(pb::AssuranceLevel::VerifiedThreadScopedControl) => {
                    domain::Assurance::VerifiedThreadScopedControl
                }
                Ok(pb::AssuranceLevel::StrongProcessContainment) => {
                    domain::Assurance::StrongProcessContainment
                }
                _ => return Err(invalid("required_assurance", "unsupported assurance")),
            },
            purpose: domain::Purpose {
                kind: match pb::PurposeKind::try_from(request.purpose) {
                    Ok(pb::PurposeKind::Interactive) => domain::PurposeKind::Interactive,
                    Ok(pb::PurposeKind::Planning) => domain::PurposeKind::Planning,
                    Ok(pb::PurposeKind::Implementation) => domain::PurposeKind::Implementation,
                    Ok(pb::PurposeKind::Review) => domain::PurposeKind::Review,
                    Ok(pb::PurposeKind::Research) => domain::PurposeKind::Research,
                    Ok(pb::PurposeKind::Discussion) => domain::PurposeKind::Discussion,
                    Ok(pb::PurposeKind::WorkflowStage) => domain::PurposeKind::WorkflowStage,
                    Ok(pb::PurposeKind::Other) => domain::PurposeKind::Other,
                    _ => return Err(invalid("purpose", "unsupported purpose")),
                },
                external_label: request.purpose_label,
            },
            parent_ref: request.parent.map(|p| crate::run::ParentReference {
                namespace: p.namespace,
                kind: p.kind,
                id: p.id,
            }),
            idempotency_key: request.idempotency_key.clone(),
            model: request.model,
            effort: request.effort,
            instructions: request.instructions.unwrap_or_default(),
            carrier: self.controller(controller)?,
            required_capabilities: request.required_capabilities,
        };
        let receipt = CoreSemanticService.start_run(input)?;
        let root = self
            .home
            .workspace_root(&receipt.snapshot.manifest.workspace_id);
        let (snapshot, observation) =
            RunSnapshot::observe(&root, receipt.snapshot.manifest.run_id, 0)?;
        let facts = ProjectionFacts::from_observation(&snapshot, &observation)?;
        Ok(pb::StartRunResponse {
            context: Some(self.context()),
            run: Some(project::run_projection(&snapshot, &facts)?),
            idempotency_key: request.idempotency_key,
            exact_replay: receipt.exact_replay,
        })
    }

    fn interrupt_turn(
        &self,
        r: pb::InterruptTurnRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        self.mutation_response(
            required(r.run.as_ref(), "run")?,
            required(r.controller.as_ref(), "controller")?,
            r.expected_state_revision,
            RunMutationOperation::Interrupt,
        )
    }
    fn pause_run(&self, r: pb::PauseRunRequest) -> Result<pb::RunMutationResponse, MachineError> {
        self.mutation_response(
            required(r.run.as_ref(), "run")?,
            required(r.controller.as_ref(), "controller")?,
            r.expected_state_revision,
            RunMutationOperation::Pause {
                interrupt: r.interrupt,
            },
        )
    }
    fn resume_run(&self, r: pb::ResumeRunRequest) -> Result<pb::RunMutationResponse, MachineError> {
        self.mutation_response(
            required(r.run.as_ref(), "run")?,
            required(r.controller.as_ref(), "controller")?,
            r.expected_state_revision,
            RunMutationOperation::Resume,
        )
    }
    fn close_run(&self, r: pb::CloseRunRequest) -> Result<pb::RunMutationResponse, MachineError> {
        let reference = required(r.run.as_ref(), "run")?;
        let controller = required(r.controller.as_ref(), "controller")?;
        let gate = orchestration_close_gate(uuid(&reference.run_id, "run.run_id")?)?;
        let _guard = gate
            .lock()
            .map_err(|_| internal_projection("orchestration close gate"))?;
        let (state_root, snapshot) = self.run_state(reference)?;
        let Some(binding) = snapshot
            .manifest
            .aggregate_binding
            .as_ref()
            .filter(|binding| {
                binding.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                    && binding.member_kind == crate::run::AggregateMemberKind::Primary
                    && binding.aggregate_id == snapshot.manifest.run_id
            })
        else {
            return self.mutation_response(
                reference,
                controller,
                r.expected_state_revision,
                RunMutationOperation::Close {
                    interrupt: r.interrupt,
                },
            );
        };
        let carrier = self.run_controller_for(&snapshot, controller, "run.close")?;
        let mut store = open_orchestration_mutator(&state_root)?;
        let session = store.session(snapshot.manifest.run_id).map_err(|error| {
            if error.code == "RUN_NOT_FOUND" {
                invalid("run.run_id", "Run is not an Orchestrated Session root")
            } else {
                error
            }
        })?;
        if binding.operation_id != session.bootstrap_operation_id
            || binding.policy_sha256.as_deref() != Some(session.specialist_policy_sha256.as_str())
        {
            return Err(internal_projection("Orchestrated Session root binding"));
        }
        snapshot.authorize_current_controller(&state_root, &carrier, "run.close")?;
        if store.session_close(snapshot.manifest.run_id)?.is_none() {
            if snapshot.stamp.run_state_revision != r.expected_state_revision {
                return Err(MachineError::new(
                    "RUN_STATE_CONFLICT",
                    "run.close expected state revision is stale",
                    false,
                    serde_json::json!({
                        "run_id":snapshot.manifest.run_id,
                        "expected_state_revision":r.expected_state_revision,
                        "actual_state_revision":snapshot.stamp.run_state_revision
                    }),
                ));
            }
            if !r.interrupt
                && (snapshot.projection.active_turn_id.is_some()
                    || !snapshot.projection.pending_requests.is_empty())
            {
                return Err(MachineError::new(
                    "RUN_STATE_CONFLICT",
                    "graceful whole-session close requires an idle Primary",
                    false,
                    serde_json::json!({"run_id":snapshot.manifest.run_id}),
                ));
            }
        }
        let (close, newly_admitted) = store.begin_session_close(
            snapshot.manifest.run_id,
            r.interrupt,
            controller.expected_controller_generation,
        )?;
        let Some(close) = close else {
            let (_, snapshot, facts) = self.run_snapshot(reference)?;
            return Ok(pb::RunMutationResponse {
                context: Some(self.context()),
                run: Some(project::run_projection(&snapshot, &facts)?),
            });
        };
        if !newly_admitted {
            if matches!(close.progress.as_str(), "completed" | "aborted")
                || snapshot.projection.lifecycle == domain::RunLifecycle::Closed
            {
                store.complete_session_close(snapshot.manifest.run_id)?;
                let (_, snapshot, facts) = self.run_snapshot(reference)?;
                let mut context = self.context();
                context.operation_id = Some(close.operation_id.to_string());
                return Ok(pb::RunMutationResponse {
                    context: Some(context),
                    run: Some(project::run_projection(&snapshot, &facts)?),
                });
            }
            return Err(match close.progress.as_str() {
                "settling" => {
                    session_close_in_progress(snapshot.manifest.run_id, close.operation_id)
                }
                "outcome_unknown" => MachineError::new(
                    "OUTCOME_UNKNOWN",
                    "whole-session close requires authoritative effect reconciliation",
                    false,
                    serde_json::json!({
                        "run_id":snapshot.manifest.run_id,
                        "operation_id":close.operation_id,
                        "required_action":"reconcile_run"
                    }),
                ),
                _ => MachineError::new(
                    "RECOVERY_REQUIRED",
                    "whole-session close requires recovery before settlement can continue",
                    false,
                    serde_json::json!({
                        "run_id":snapshot.manifest.run_id,
                        "operation_id":close.operation_id,
                        "required_action":"recover_run"
                    }),
                ),
            });
        }
        let mut effects = crate::semantic::ProductionOrchestrationEffects::new(&state_root);
        if let Err(mut error) = store.settle_session_close(snapshot.manifest.run_id, &mut effects) {
            if let Some(details) = error.details.as_object_mut() {
                details.insert(
                    "operation_id".to_owned(),
                    serde_json::Value::String(close.operation_id.to_string()),
                );
                details.insert(
                    "run_id".to_owned(),
                    serde_json::Value::String(snapshot.manifest.run_id.to_string()),
                );
            }
            return Err(error);
        }
        if snapshot.projection.lifecycle != domain::RunLifecycle::Closed
            && let Err(mut error) = self.mutate(
                reference,
                controller,
                Some(r.expected_state_revision),
                RunMutationOperation::Close {
                    interrupt: r.interrupt,
                },
            )
        {
            if error.code == "RUN_BUSY" {
                return Err(session_close_in_progress(
                    snapshot.manifest.run_id,
                    close.operation_id,
                ));
            }
            let (_, after_error) = self.run_state(reference)?;
            if after_error.projection.lifecycle != domain::RunLifecycle::Closed {
                if error.code == "RUN_STATE_CONFLICT" {
                    return Err(session_close_in_progress(
                        snapshot.manifest.run_id,
                        close.operation_id,
                    ));
                }
                let _ = store.record_session_close_failure(snapshot.manifest.run_id, &error.code);
                if let Some(details) = error.details.as_object_mut() {
                    details.insert(
                        "operation_id".to_owned(),
                        serde_json::Value::String(close.operation_id.to_string()),
                    );
                    details.insert(
                        "run_id".to_owned(),
                        serde_json::Value::String(snapshot.manifest.run_id.to_string()),
                    );
                }
                return Err(error);
            }
        }
        store.complete_session_close(snapshot.manifest.run_id)?;
        let (_, snapshot, facts) = self.run_snapshot(reference)?;
        let mut context = self.context();
        context.operation_id = Some(close.operation_id.to_string());
        Ok(pb::RunMutationResponse {
            context: Some(context),
            run: Some(project::run_projection(&snapshot, &facts)?),
        })
    }
    fn recover_run(
        &self,
        r: pb::RecoverRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        let reference = required(r.run.as_ref(), "run")?;
        let controller = required(r.controller.as_ref(), "controller")?;
        if let Some(response) = self.orchestration_close_recovery_response(
            reference,
            controller,
            r.expected_state_revision,
            RunMutationOperation::Recover,
        )? {
            return Ok(response);
        }
        self.mutation_response(
            reference,
            controller,
            r.expected_state_revision,
            RunMutationOperation::Recover,
        )
    }
    fn reconcile_run(
        &self,
        r: pb::ReconcileRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        let reference = required(r.run.as_ref(), "run")?;
        let controller = required(r.controller.as_ref(), "controller")?;
        if let Some(response) = self.orchestration_close_recovery_response(
            reference,
            controller,
            r.expected_state_revision,
            RunMutationOperation::Reconcile,
        )? {
            return Ok(response);
        }
        self.mutation_response(
            reference,
            controller,
            r.expected_state_revision,
            RunMutationOperation::Reconcile,
        )
    }

    fn verify_controller(
        &self,
        r: pb::VerifyControllerRequest,
    ) -> Result<pb::VerifyControllerResponse, MachineError> {
        let (_, snapshot) = self.run_state(required(r.run.as_ref(), "run")?)?;
        let reference = required(r.controller.as_ref(), "controller")?;
        let carrier = self.controller(reference)?;
        if reference.expected_controller_generation != snapshot.controller.generation {
            return Err(controller_mismatch(snapshot.manifest.run_id));
        }
        authorize_controller(
            snapshot.manifest.run_id,
            "controller.verify",
            snapshot.controller_authority()?,
            &carrier,
        )?;
        use crate::ledger::LedgerClock as _;
        Ok(pb::VerifyControllerResponse {
            context: Some(self.context()),
            run_id: snapshot.manifest.run_id.to_string(),
            controller: Some(project::controller(&snapshot.controller)),
            verified_at: Some(crate::gateway_event::timestamp(
                &crate::ledger::SystemLedgerClock::default().timestamp(),
            )?),
        })
    }
}

#[cfg(test)]
mod broker_approval_tests {
    use super::*;

    #[test]
    fn broker_approval_response_accepts_only_the_closed_answer_shape() {
        assert_eq!(
            broker_approval_decision(
                br#"{"answers":{"specialist_approval":{"answers":["approve"]}}}"#,
            ),
            Ok(true)
        );
        assert_eq!(
            broker_approval_decision(
                br#"{"answers":{"specialist_approval":{"answers":["reject"]}}}"#,
            ),
            Ok(false)
        );
        for invalid in [
            br#"{}"#.as_slice(),
            br#"{"decision":"approve"}"#.as_slice(),
            br#"{"answers":{"specialist_approval":{"answers":["approve","reject"]}}}"#.as_slice(),
            br#"{"answers":{"specialist_approval":{"answers":["other"]}}}"#.as_slice(),
        ] {
            assert_eq!(
                broker_approval_decision(invalid).unwrap_err().code,
                "INTERACTION_RESPONSE_INVALID"
            );
        }
    }

    #[test]
    fn result_page_cursor_is_session_head_projection_and_controller_bound() {
        let session_id = Uuid::now_v7();
        let cursor = ResultPageCursor {
            schema_version: 1,
            session_id,
            projection_version: 1,
            captured_publication_head: 9,
            after_publication_order: 4,
        };
        let token = encode_result_cursor(&cursor, &"a".repeat(64)).unwrap();
        let decoded = decode_result_cursor(&token, session_id, 1, &"a".repeat(64)).unwrap();
        assert_eq!(decoded.captured_publication_head, 9);
        assert_eq!(decoded.after_publication_order, 4);
        assert_eq!(
            decode_result_cursor(&token, Uuid::now_v7(), 1, &"a".repeat(64))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            decode_result_cursor(&token, session_id, 2, &"a".repeat(64))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            decode_result_cursor(&token, session_id, 1, &"b".repeat(64))
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        let mut tampered = token.into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            decode_result_cursor(
                std::str::from_utf8(&tampered).unwrap(),
                session_id,
                1,
                &"a".repeat(64),
            )
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );
    }
}
