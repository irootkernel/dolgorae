//! Public-v2 adapters. Durable identity, lifecycle and authorization stay semantic-owned.
use crate::domain;
use crate::gateway::{GatewayBackend, GatewayTransport};
use crate::gateway_projection::{ProjectionFacts, acceptance_facts};
use crate::gateway_projection_v2 as project;
use crate::gateway_service::{
    ResultPageCursor, decode_result_cursor, encode_result_cursor, invalid, required, uuid,
};
use crate::machine::MachineError;
use crate::protocol::{public_v1 as old, public_v2 as pb};
use crate::semantic::{CoreSemanticService, RunMutationInput, RunMutationOperation};
use crate::snapshot::RunSnapshot;
use std::path::PathBuf;
use tonic::{Request, Response, Status};

pub trait ModernGatewayBackend: GatewayBackend {
    fn home(&self) -> &crate::paths::DolgoraeHome;
    fn instance_id(&self) -> uuid::Uuid;
    fn workspace(
        &self,
        reference: &old::WorkspaceRef,
    ) -> Result<crate::workspace::WorkspaceView, MachineError>;
    fn workspace_observed(
        &self,
        reference: &old::WorkspaceRef,
    ) -> Result<crate::workspace::WorkspaceView, MachineError>;
    fn controller(
        &self,
        reference: &old::ControllerCarrierRef,
    ) -> Result<crate::controller::CredentialCarrier, MachineError>;
    fn context(&self) -> old::ResponseContext;
}

fn workspace(value: &pb::WorkspaceRef) -> old::WorkspaceRef {
    old::WorkspaceRef {
        absolute_path: value.absolute_path.clone(),
        expected_workspace_id: value.expected_workspace_id.clone(),
    }
}
fn controller(value: &pb::ControllerCarrierRef) -> old::ControllerCarrierRef {
    old::ControllerCarrierRef {
        absolute_file_path: value.absolute_file_path.clone(),
        expected_controller_id: value.expected_controller_id.clone(),
        expected_controller_generation: value.expected_controller_generation,
    }
}
fn context(backend: &dyn ModernGatewayBackend) -> pb::ResponseContext {
    pb::ResponseContext {
        protocol_version: 2,
        server_instance_id: backend.instance_id().to_string(),
        operation_id: None,
    }
}
fn snapshot(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::RunRef,
) -> Result<(PathBuf, RunSnapshot, ProjectionFacts), MachineError> {
    let view = backend.workspace_observed(&workspace(required(
        reference.workspace.as_ref(),
        "run.workspace",
    )?))?;
    let root = backend.home().workspace_root(&view.workspace_id);
    let (snapshot, observation) =
        RunSnapshot::observe_display(&root, uuid(&reference.run_id, "run.run_id")?, 0)?;
    if snapshot.manifest.workspace_id != view.workspace_id {
        return Err(invalid("run", "workspace identity differs"));
    }
    let mut facts = ProjectionFacts::from_observation(&snapshot, &observation)?;
    facts.profile_cleanup_required = crate::semantic::profile_cleanup_required(&root, &snapshot);
    Ok((root, snapshot, facts))
}
fn protected_snapshot(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::RunRef,
) -> Result<(PathBuf, RunSnapshot), MachineError> {
    let view = backend.workspace_observed(&workspace(required(
        reference.workspace.as_ref(),
        "run.workspace",
    )?))?;
    let root = backend.home().workspace_root(&view.workspace_id);
    let snapshot = RunSnapshot::load(&root, uuid(&reference.run_id, "run.run_id")?, 0)?;
    if snapshot.manifest.workspace_id != view.workspace_id {
        return Err(invalid("run", "workspace identity differs"));
    }
    Ok((root, snapshot))
}
fn get(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::RunRef,
) -> Result<pb::RunProjection, MachineError> {
    let (_, snapshot, facts) = snapshot(backend, reference)?;
    project::run_projection(&snapshot, &facts)
}

fn submit_operation(
    identity: &domain::ControllerIdentity,
    workspace_id: &str,
    run_id: uuid::Uuid,
    key: &str,
) -> Result<serde_json::Value, MachineError> {
    let namespace = crate::operation::Namespace::submit(identity, workspace_id, run_id, key)
        .map_err(|_| invalid("idempotency_key", "operation namespace is invalid"))?;
    Ok(serde_json::json!({
        "controller_kind": project::controller_kind(identity.kind) as i32,
        "principal_ref": namespace.controller_principal,
        "controller_generation": identity.generation,
        "workspace_id": workspace_id,
        "run_id": run_id,
        "idempotency_key": key,
    }))
}

fn submit_error(
    mut error: MachineError,
    operation: &serde_json::Value,
    accepted: bool,
) -> MachineError {
    let mut details = error.details.as_object().cloned().unwrap_or_default();
    details.insert("public_v2_operation".into(), operation.clone());
    details.insert("public_v2_accepted".into(), accepted.into());
    details.insert("run_id".into(), operation["run_id"].clone());
    error.details = serde_json::Value::Object(details);
    error
}

fn mutate(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::RunRef,
    carrier: &pb::ControllerCarrierRef,
    revision: u64,
    operation: RunMutationOperation,
) -> Result<
    (
        crate::semantic::RunMutationReceipt,
        Option<serde_json::Value>,
    ),
    MachineError,
> {
    let view = backend.workspace(&workspace(required(
        reference.workspace.as_ref(),
        "run.workspace",
    )?))?;
    let modern = crate::run::RunStore::new(
        crate::workspace::SystemWorkspacePlatform,
        backend.home().workspace_root(&view.workspace_id),
    )
    .load_manifest(uuid(&reference.run_id, "run.run_id")?)?
    .schema_version
        >= 3;
    let caller = modern
        && matches!(
            operation,
            RunMutationOperation::AcquireWriter
                | RunMutationOperation::ReleaseWriter
                | RunMutationOperation::Resume
                | RunMutationOperation::Close { .. }
                | RunMutationOperation::Recover
                | RunMutationOperation::Reconcile
        );
    let input = RunMutationInput {
        workspace: view.canonical_path.to_path_buf()?,
        expected_workspace_id: view.workspace_id,
        run_id: uuid(&reference.run_id, "run.run_id")?,
        expected_state_revision: Some(revision),
        carrier: backend.controller(&controller(carrier))?,
        operation,
    };
    let operation_identity = if let RunMutationOperation::Submit { request, .. } = &input.operation
    {
        let binding = crate::controller::load_reconciled_controller_binding(
            &backend.home().workspace_root(&input.expected_workspace_id),
            input.run_id,
        )?;
        let identity = crate::controller::authorize_controller(
            input.run_id,
            "run.submit",
            &binding,
            &input.carrier,
        )?;
        Some(submit_operation(
            &identity,
            &input.expected_workspace_id,
            input.run_id,
            &request.idempotency_key,
        )?)
    } else {
        None
    };
    let result = if caller {
        CoreSemanticService.mutate_caller_run(input)
    } else if modern {
        CoreSemanticService.mutate_modern_run(input)
    } else {
        CoreSemanticService.mutate_run(input)
    };
    result
        .map_err(|error| match operation_identity.as_ref() {
            Some(identity) => submit_error(error, identity, false),
            None => error,
        })
        .map(|receipt| (receipt, operation_identity))
}
fn mutation(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::RunRef,
    carrier: &pb::ControllerCarrierRef,
    revision: u64,
    operation: RunMutationOperation,
) -> Result<pb::RunMutationResponse, MachineError> {
    let (receipt, _) = mutate(backend, reference, carrier, revision, operation)?;
    let mut context = context(backend);
    if receipt
        .snapshot
        .manifest
        .aggregate_binding
        .as_ref()
        .is_some_and(|binding| binding.member_kind == crate::run::AggregateMemberKind::Primary)
    {
        context.operation_id = crate::orchestration::OrchestrationStore::open_observer(
            &backend
                .home()
                .workspace_root(&receipt.snapshot.manifest.workspace_id),
        )?
        .observe_session(receipt.snapshot.manifest.run_id)?
        .close_operation_id
        .map(|id| id.to_string());
    }
    Ok(pb::RunMutationResponse {
        context: Some(context),
        run: Some(get(backend, reference)?),
    })
}
fn checked_context(value: Option<&pb::RequestContext>) -> Result<(), MachineError> {
    let value = required(value, "context")?;
    if value.protocol_version != 2
        || value.client_request_id.is_empty()
        || value.client_instance_id.is_empty()
    {
        return Err(invalid("context", "public-v2 context is required"));
    }
    Ok(())
}
pub(crate) fn error_status(error: &MachineError, method: &str) -> Status {
    use prost::Message as _;
    static MAPPING: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    let mapping = MAPPING.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-grpc-error-mapping-v2.json"
        ))
        .expect("checked error mapping")
    });
    static POLICY: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    let policy = POLICY.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-rpc-mutation-policy-v2.json"
        ))
        .expect("checked mutation policy")
    });
    let mutating = policy["methods"][method]["effect_class"].as_str() != Some("R");
    let code = match mapping["code_status"][&error.code]
        .as_str()
        .unwrap_or("INTERNAL")
    {
        "INVALID_ARGUMENT" => tonic::Code::InvalidArgument,
        "NOT_FOUND" => tonic::Code::NotFound,
        "PERMISSION_DENIED" => tonic::Code::PermissionDenied,
        "ABORTED" => tonic::Code::Aborted,
        "RESOURCE_EXHAUSTED" => tonic::Code::ResourceExhausted,
        "UNAVAILABLE" => tonic::Code::Unavailable,
        "DATA_LOSS" => tonic::Code::DataLoss,
        "INTERNAL" => tonic::Code::Internal,
        _ => tonic::Code::FailedPrecondition,
    };
    let mut operation_name = String::from("OPERATION_KIND_");
    for (index, character) in method
        .rsplit('.')
        .next()
        .unwrap_or(method)
        .chars()
        .enumerate()
    {
        if index > 0 && character.is_uppercase() {
            operation_name.push('_');
        }
        operation_name.push(character.to_ascii_uppercase());
    }
    let operation_kind =
        pb::OperationKind::from_str_name(&operation_name).unwrap_or(pb::OperationKind::Unspecified);
    let recovery = matches!(
        error.code.as_str(),
        "RECOVERY_REQUIRED" | "BACKGROUND_EXECUTION_UNVERIFIED"
    );
    let value = &error.details["public_v2_operation"];
    let operation = value["idempotency_key"]
        .as_str()
        .map(|key| pb::OperationRef {
            caller_scope: Some(pb::CallerScope {
                controller_kind: value["controller_kind"].as_i64().unwrap_or_default() as i32,
                principal_ref: value["principal_ref"].as_str().unwrap_or_default().into(),
                controller_generation: value["controller_generation"].as_u64().unwrap_or_default(),
                workspace_id: value["workspace_id"].as_str().unwrap_or_default().into(),
                aggregate: value["run_id"].as_str().map_or_else(
                    || {
                        Some(pb::caller_scope::Aggregate::ProvisioningScope(
                            "run-start".into(),
                        ))
                    },
                    |run_id| Some(pb::caller_scope::Aggregate::RunId(run_id.into())),
                ),
            }),
            operation_kind: operation_kind as i32,
            idempotency_key: key.into(),
        });
    let run_id = error.details["run_id"]
        .as_str()
        .or_else(|| value["run_id"].as_str())
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .map(str::to_owned);
    let wire_code = if mapping["code_status"].get(&error.code).is_some() {
        error.code.as_str()
    } else {
        "INTERNAL_ERROR"
    };
    let accepted_start = method == "RunService.StartRun"
        && run_id.is_some()
        && !matches!(wire_code, "WRITER_BUSY" | "IDEMPOTENCY_CONFLICT");
    let accepted_submit =
        method == "RunService.SubmitTurn" && error.details["public_v2_accepted"] == true;
    let accepted = accepted_start || accepted_submit;
    let unknown = mutating
        && !accepted
        && matches!(
            wire_code,
            "INTERNAL_ERROR"
                | "OUTCOME_UNKNOWN"
                | "TRANSPORT_FAILURE"
                | "INTERACTION_OUTCOME_UNKNOWN"
        );
    let recovery = recovery || accepted;
    let detail = pb::DolgoraeErrorDetail {
        detail_version: 2,
        dolgorae_error_code: wire_code.to_owned(),
        operation_kind: operation_kind as i32,
        run_id: run_id.clone(),
        subjects: run_id.map(|run_id| pb::SubjectIdentities {
            run_id: Some(run_id),
            ..Default::default()
        }),
        idempotency_key: operation
            .as_ref()
            .map(|operation| operation.idempotency_key.clone()),
        operation,
        effect: if unknown {
            pb::EffectState::Unresolved
        } else if accepted {
            pb::EffectState::Accepted
        } else {
            pb::EffectState::None
        } as i32,
        classification: if unknown {
            pb::FailureClassification::Unknown
        } else if recovery {
            pb::FailureClassification::RecoveryNeeded
        } else {
            pb::FailureClassification::Rejected
        } as i32,
        action: if unknown || accepted_submit {
            pb::RequiredClientAction::ReconcileOperation
        } else if recovery {
            pb::RequiredClientAction::RecoverRun
        } else if error.code == "POOL_CAPACITY_EXCEEDED" {
            pb::RequiredClientAction::Wait
        } else {
            pb::RequiredClientAction::FixRequest
        } as i32,
        retry_classification: pb::RetryClassification::Forbidden as i32,
        recovery_classification: if unknown || accepted_submit {
            pb::RecoveryClassification::ReconcileRequired
        } else if recovery {
            pb::RecoveryClassification::SnapshotRequired
        } else {
            pb::RecoveryClassification::None
        } as i32,
        writer_busy: if wire_code == "WRITER_BUSY" {
            Some(pb::WriterBusyDiagnostic {
                requested_run_id: error.details["requested_run_id"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                holder_run_id: error.details["holder_run_id"].as_str().map(str::to_owned),
                holder_profile: error.details["holder_profile"].as_str().map(str::to_owned),
                authority_state: pb::WriterAuthorityState::from_str_name(&format!(
                    "WRITER_AUTHORITY_STATE_{}",
                    error.details["authority_state"]
                        .as_str()
                        .unwrap_or("none")
                        .to_ascii_uppercase()
                ))
                .unwrap_or(pb::WriterAuthorityState::None) as i32,
                writer_generation: error.details["writer_generation"]
                    .as_u64()
                    .unwrap_or_default(),
                handoff_eligible: error.details["handoff_eligible"].as_bool().unwrap_or(false),
            })
        } else {
            None
        },
        ..Default::default()
    };
    let rich = crate::gateway::GoogleRpcStatus {
        code: code as i32,
        message: wire_code.to_owned(),
        details: vec![prost_types::Any {
            type_url: "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail".into(),
            value: detail.encode_to_vec(),
        }],
    };
    Status::with_details(code, wire_code, rich.encode_to_vec().into())
}

macro_rules! execute {
    ($self:ident,$request:ident,$method:literal,$backend:ident,$body:expr) => {{
        let deadline = crate::gateway::rpc_deadline_with(&$request, $method, error_status)?;
        let request = $request.into_inner();
        checked_context(request.context.as_ref()).map_err(|e| error_status(&e, $method))?;
        $self
            .execute_v2($method, deadline, move |source| {
                let $backend = source.modern()?;
                let $request = request;
                $body
            })
            .await
            .map(Response::new)
    }};
}
fn enum_name<T>(value: Option<T>, field: &str) -> Result<T, MachineError> {
    value.ok_or_else(|| invalid(field, "unknown or unspecified selection"))
}
fn admitted(
    value: &pb::AdmittedConfiguration,
) -> Result<crate::delegation_identity::AdmittedConfiguration, MachineError> {
    let purpose = enum_name(
        pb::PurposeKind::try_from(value.purpose_kind)
            .ok()
            .filter(|v| *v != pb::PurposeKind::Unspecified),
        "purpose",
    )?
    .as_str_name()
    .trim_start_matches("PURPOSE_KIND_")
    .to_ascii_lowercase();
    let lane = enum_name(
        pb::ExecutionLane::try_from(value.execution_lane)
            .ok()
            .filter(|v| *v != pb::ExecutionLane::Unspecified),
        "lane",
    )?
    .as_str_name()
    .trim_start_matches("EXECUTION_LANE_")
    .to_ascii_lowercase();
    let assurance = enum_name(
        pb::AssuranceLevel::try_from(value.required_assurance)
            .ok()
            .filter(|v| *v != pb::AssuranceLevel::Unspecified),
        "assurance",
    )?
    .as_str_name()
    .trim_start_matches("ASSURANCE_LEVEL_")
    .to_ascii_lowercase();
    let access = match pb::CreationMode::try_from(value.requested_access) {
        Ok(pb::CreationMode::ReadOnly) => "read_only",
        Ok(pb::CreationMode::Writable) => "canonical_workspace_write",
        _ => return Err(invalid("access", "explicit access is required")),
    };
    let native = match pb::NativeSubagentPolicy::try_from(value.native_subagent_policy) {
        Ok(pb::NativeSubagentPolicy::Disabled) => "disabled",
        Ok(pb::NativeSubagentPolicy::Enabled) => "enabled",
        _ => {
            return Err(invalid(
                "native_subagent_policy",
                "explicit policy is required",
            ));
        }
    };
    let result = crate::delegation_identity::AdmittedConfiguration {
        schema: value.schema.clone(),
        profile_binding_schema: value.profile_binding_schema.clone(),
        profile_binding_digest: value.profile_binding_digest.clone(),
        model: value.model.clone(),
        effort: value.effort.clone(),
        purpose_kind: purpose,
        purpose_label: value.purpose_label.clone(),
        required_capabilities: value.required_capabilities.clone(),
        execution_lane: lane,
        requested_access: access.into(),
        required_assurance: assurance,
        native_subagent_policy: native.into(),
        instruction_identity_digest: value.instruction_identity_digest.clone(),
    };
    result.digest()?;
    Ok(result)
}
fn decode_capture(
    input: &pb::CapturedInstructions,
) -> Result<crate::delegation_identity::InstructionSnapshot, MachineError> {
    use crate::instructions::{CapturedNamedSource, DelegationCapture, RoleCapture, SourceCapture};
    fn source(value: &pb::CapturedSource) -> Result<String, MachineError> {
        let text = std::str::from_utf8(&value.utf8)
            .map_err(|_| invalid("instructions", "source must be UTF-8"))?;
        let reference = required(value.content.as_ref(), "content")?;
        if crate::delegation_identity::BytesRef::from_bytes(text.as_bytes())
            != (crate::delegation_identity::BytesRef {
                byte_length: reference.byte_length,
                sha256: reference.sha256.clone(),
            })
        {
            return Err(invalid("instructions", "captured byte identity differs"));
        }
        Ok(text.to_owned())
    }
    let named = |values: &[pb::CapturedSource]| {
        values
            .iter()
            .map(|v| {
                Ok(CapturedNamedSource {
                    name: v.name.clone(),
                    body: source(v)?,
                })
            })
            .collect::<Result<Vec<_>, MachineError>>()
    };
    let common = source(required(input.common.as_ref(), "instructions.common")?)?;
    let role = source(required(
        input.role_body.as_ref(),
        "instructions.role_body",
    )?)?;
    let result =
        crate::delegation_identity::InstructionSnapshot::from_capture(DelegationCapture {
            common: SourceCapture::from_body(&common)?,
            role: RoleCapture::from_body(&input.role_name, &role)?,
            dependencies: named(&input.dependencies)?,
            selected_guidance: named(&input.selected_guidance)?,
        })?;
    if project::captured_instructions(&result) != *input {
        return Err(invalid(
            "instructions",
            "captured instruction bytes or identities differ",
        ));
    }
    Ok(result)
}
fn provisioning(
    backend: &dyn ModernGatewayBackend,
    input: &pb::ProvisioningInput,
    retained_binding: Option<&crate::global_runtime::GlobalProfileBinding>,
) -> Result<crate::provisioning::RetainedSnapshot, MachineError> {
    backend.workspace_observed(&workspace(required(input.workspace.as_ref(), "workspace")?))?;
    let selection = required(input.selection.as_ref(), "selection")?;
    let config = admitted(required(selection.configuration.as_ref(), "configuration")?)?;
    let instructions = decode_capture(required(input.instructions.as_ref(), "instructions")?)?;
    let binding = match retained_binding {
        Some(binding) => binding.clone(),
        None => crate::global_runtime::ResolvedGlobalProfile::resolve(
            backend.home(),
            &selection.profile_name,
        )?
        .prepare_specialist(backend.home(), config.native_subagent_policy == "enabled")?,
    };
    let qualified = if retained_binding.is_some() {
        binding.accepted_caller_workspace_supported()
    } else {
        binding.caller_workspace_supported()
    };
    if !qualified {
        return Err(invalid("profile", "selected runtime is not qualified"));
    }
    if selection.policy_name.is_empty() != selection.policy_snapshot_digest.is_empty() {
        return Err(invalid(
            "policy",
            "policy name and digest must appear together",
        ));
    }
    if retained_binding.is_none() && !selection.policy_name.is_empty() {
        let workspace = backend
            .workspace_observed(&workspace(required(input.workspace.as_ref(), "workspace")?))?;
        let path = workspace.canonical_path.to_path_buf()?;
        let policy = crate::markdown_policy::MarkdownPolicyRegistry::discover(Some(&path))?
            .prepare_session(&selection.policy_name)?;
        if format!("sha256:{}", policy.policy().digest()?) != selection.policy_snapshot_digest {
            return Err(invalid("policy", "selected policy digest differs"));
        }
    }
    let snapshot = crate::provisioning::RetainedSnapshot {
        schema: crate::provisioning::SNAPSHOT_SCHEMA.into(),
        common: instructions.common.clone(),
        role: instructions.role.clone(),
        dependencies: instructions
            .dependencies
            .iter()
            .map(|s| crate::provisioning::RetainedNamedSource {
                name: s.name.clone(),
                body: s.source.body.clone(),
            })
            .collect(),
        selected_guidance: instructions
            .selected_guidance
            .iter()
            .map(|s| crate::provisioning::RetainedNamedSource {
                name: s.name.clone(),
                body: s.source.body.clone(),
            })
            .collect(),
        profile: selection.profile_name.clone(),
        profile_binding: binding,
        admitted: config,
    };
    if snapshot.role_name() != selection.role_name
        || snapshot.admitted.digest()? != selection.agent_configuration_digest
    {
        return Err(invalid("selection", "configuration identity differs"));
    }
    snapshot.admitted_configuration()?;
    Ok(snapshot)
}
fn start(
    backend: &dyn ModernGatewayBackend,
    request: pb::StartRunRequest,
) -> Result<pb::StartRunResponse, MachineError> {
    let view = backend.workspace_observed(&workspace(required(
        request.workspace.as_ref(),
        "workspace",
    )?))?;
    let reference = required(request.controller.as_ref(), "controller")?;
    let carrier = backend.controller(&controller(reference))?;
    let binding = crate::controller::binding_from_carrier(
        &carrier,
        reference.expected_controller_generation,
    )?;
    let namespace = crate::operation::Namespace::delegation(
        &binding.identity,
        &view.workspace_id,
        None,
        "start_run",
        &request.idempotency_key,
    )
    .map_err(|reason| invalid("idempotency_key", reason))?;
    let identity = serde_json::json!({"controller_kind":project::controller_kind(binding.identity.kind) as i32,
        "principal_ref":namespace.controller_principal,"controller_generation":binding.identity.generation,
        "workspace_id":view.workspace_id,"idempotency_key":request.idempotency_key});
    let retained = crate::run::StartReservationStore::new(
        crate::workspace::SystemWorkspacePlatform,
        backend.home().workspace_root(&view.workspace_id),
    )
    .retained_delegation_start_binding(&request.idempotency_key, &binding.identity)?;
    start_inner(backend, request, retained.as_ref()).map_err(|mut error| {
        if let Some(details) = error.details.as_object_mut() {
            details.insert("public_v2_operation".into(), identity);
        }
        error
    })
}
fn start_inner(
    backend: &dyn ModernGatewayBackend,
    request: pb::StartRunRequest,
    retained_binding: Option<&crate::global_runtime::GlobalProfileBinding>,
) -> Result<pb::StartRunResponse, MachineError> {
    let selection = required(request.provisioning.as_ref(), "provisioning")?.clone();
    let snapshot = provisioning(
        backend,
        &pb::ProvisioningInput {
            workspace: request.workspace.clone(),
            selection: Some(selection.clone()),
            instructions: request.instructions.clone(),
        },
        retained_binding,
    )?;
    let admitted = &snapshot.admitted;
    let configuration = required(selection.configuration.as_ref(), "configuration")?;
    if request.requested_mode != configuration.requested_access
        || request.execution_lane != configuration.execution_lane
        || request.required_assurance != configuration.required_assurance
        || request.purpose != configuration.purpose_kind
        || request.purpose_label != admitted.purpose_label
        || request.model.as_ref().is_some_and(|m| m != &admitted.model)
        || request
            .effort
            .as_ref()
            .is_some_and(|e| e != &admitted.effort)
        || request.required_capabilities != admitted.required_capabilities
        || request.required_assurance != pb::AssuranceLevel::BestEffortPersonalAlpha as i32
    {
        return Err(invalid(
            "provisioning",
            "startup selections differ from admitted configuration",
        ));
    }
    let reference = required(request.workspace.as_ref(), "workspace")?;
    let view = backend.workspace(&workspace(reference))?;
    let carrier_ref = required(request.controller.as_ref(), "controller")?;
    if carrier_ref.expected_controller_generation != 1 {
        return Err(invalid("controller", "new Run requires generation 1"));
    }
    let carrier = backend.controller(&controller(carrier_ref))?;
    let binding = crate::controller::binding_from_carrier(&carrier, 1)?;
    let control_mode = match binding.identity.kind {
        domain::ControllerKind::HumanCli | domain::ControllerKind::InteractiveClient => {
            domain::ControlMode::DirectInteractive
        }
        _ => domain::ControlMode::ManagedAgent,
    };
    let purpose = serde_json::from_value(serde_json::json!(admitted.purpose_kind))
        .map_err(|_| invalid("purpose", "invalid purpose"))?;
    let input = crate::run::CallerRunStartRequest {
        schema: "dolgorae-run-start-request/v2".into(),
        workspace: Some(reference.absolute_path.clone()),
        expected_workspace_id: Some(view.workspace_id),
        profile: selection.profile_name,
        control_mode,
        access: if request.requested_mode == pb::CreationMode::Writable as i32 {
            domain::CallerWorkspaceAccess::Writable
        } else {
            domain::CallerWorkspaceAccess::ReadOnly
        },
        role: selection.role_name,
        specialist_policy: (!selection.policy_name.is_empty()).then_some(selection.policy_name),
        model: Some(admitted.model.clone()),
        effort: Some(admitted.effort.clone()),
        purpose: domain::Purpose {
            kind: purpose,
            external_label: request.purpose_label,
        },
        parent_ref: request.parent.map(|p| crate::run::ParentReference {
            namespace: p.namespace,
            kind: p.kind,
            id: p.id,
        }),
        required_capabilities: request.required_capabilities,
        idempotency_key: request.idempotency_key.clone(),
    };
    let receipt = CoreSemanticService.start_provisioned_run(
        input,
        carrier,
        &snapshot,
        (!selection.policy_snapshot_digest.is_empty())
            .then_some(selection.policy_snapshot_digest.as_str()),
    )?;
    let reference = pb::RunRef {
        workspace: request.workspace,
        run_id: receipt.snapshot.manifest.run_id.to_string(),
    };
    Ok(pb::StartRunResponse {
        context: Some(context(backend)),
        run: Some(get(backend, &reference)?),
        idempotency_key: request.idempotency_key,
        exact_replay: receipt.exact_replay,
    })
}

const METHODS: &[&str] = &[
    "OperationService.LookupOperation",
    "RuntimeService.ListProfileDiagnostics",
    "ControllerService.VerifyController",
    "ArtifactService.GetArtifact",
    "ArtifactService.ReadArtifactChunk",
    "RuntimeService.GetCapabilities",
    "RuntimeService.InspectWorkspace",
    "RuntimeService.ListProfiles",
    "RuntimeService.GetProfile",
    "RunService.StartRun",
    "RunService.ListRuns",
    "RunService.GetRun",
    "RunService.SubmitTurn",
    "RunService.InterruptTurn",
    "RunService.PauseRun",
    "RunService.ResumeRun",
    "RunService.CloseRun",
    "RunService.RecoverRun",
    "RunService.ReconcileRun",
    "WriterService.GetWorkspaceWriterStatus",
    "WriterService.AcquireWriter",
    "WriterService.ReleaseWriter",
    "ProvisioningService.ValidateProvisioning",
    "OrchestrationService.GetOrchestratedSession",
    "OrchestrationService.ListOrchestratedSessionResults",
];

#[tonic::async_trait]
impl pb::runtime_service_server::RuntimeService for GatewayTransport {
    async fn get_capabilities(
        &self,
        request: Request<pb::GetCapabilitiesRequest>,
    ) -> Result<Response<pb::GetCapabilitiesResponse>, Status> {
        execute!(self, request, "RuntimeService.GetCapabilities", backend, {
            if request.minimum_protocol_version > 2 || request.maximum_protocol_version < 2 {
                return Err(invalid(
                    "protocol_version",
                    "public-v2 negotiation required",
                ));
            }
            Ok(pb::GetCapabilitiesResponse {
                context:Some(context(backend)),dolgorae_version:env!("CARGO_PKG_VERSION").into(),
                protocol:Some(pb::ProtocolCapabilities {rpc_protocol_version:2,minimum_client_protocol_version:2,maximum_client_protocol_version:2,machine_protocol_version:5,event_protocol_version:1,timeline_protocol_version:2,event_projection_version:2,grpc_error_detail_version:2,projection_profiles:vec![1,2]}),
                supported_methods:METHODS.iter().map(|s|(*s).into()).collect(),
                descriptor_sha256:crate::jcs::sha256_hex(crate::protocol::PUBLIC_V2_DESCRIPTOR),
                contract_manifest_sha256:crate::jcs::sha256_hex(include_bytes!("../docs/protocol/dolgorae-gul-consumer-v2.json")),
                supported_control_modes:vec![1,2],supported_transports:vec![2],
                controller_carrier:Some(credential_capabilities(backend.get_capabilities(old::GetCapabilitiesRequest::default())?.controller_carrier.ok_or_else(||invalid("capabilities","missing credential contract"))?)),
                features:Some(pb::RuntimeFeatureCapabilities {persistent_runs:true,reader_writer_access:true,threadless_acquire_write:true,first_write_via_submit_turn:true,durable_writer_authority:true,sticky_dedicated_lanes:true,artifact_retrieval:true,controller_binding:true,worker_controller_revalidation:true,safe_client_projection:true,public_local_socket:true,control_modes:true,assurance_negotiation:true,profile_diagnostics:true,..Default::default()}),
                assurance:Some(pb::AssuranceCapabilities {supported:vec![1],maximum_achievable:1,selected_before_run_allocation:true}),
                lanes:Some(pb::LaneCapabilities {supported_lanes:vec![1,2],items:vec![pb::LaneCapability {lane:1,command_execution:pb::CommandExecutionSupport::BoundedBestEffort as i32,background_control:pb::BackgroundControlSupport::ProfileAggregateOnly as i32,per_run_process_cleanup:pb::ProcessCleanupSupport::Unavailable as i32,maximum_assurance:1,writer_support:false,lazy_first_input:false},pb::LaneCapability {lane:2,command_execution:pb::CommandExecutionSupport::Supported as i32,background_control:pb::BackgroundControlSupport::BestEffortPersonalAlpha as i32,per_run_process_cleanup:pb::ProcessCleanupSupport::BoundedProcessCensus as i32,maximum_assurance:1,writer_support:true,lazy_first_input:false}]}),
                native_subagents:Some(pb::NativeSubagentCapabilities {lifecycle_observation:pb::SupportState::Unverified as i32,disable_enforcement:pb::SupportState::Supported as i32,quiescence_tracking:pb::SupportState::Unverified as i32,enabled:false,safe_reason:"successor configurations disable native subagents by default; enabled configurations require a dedicated lane".into()}),
                access_policy_transition:1,
                independent_run_concurrency:Some(pb::IndependentRunConcurrencyCapabilities {basic_same_home_coexistence:1,storage_and_long_duration:3,resource_warning_live_dedicated:6}),
                native_subagent_policy:pb::NativeSubagentPolicy::Disabled as i32,
                profile_launch_mode:pb::ProfileLaunchMode::DolgoraeOwnedDirectExecutable as i32,
                bounds:Some(pb::SuccessorBounds {captured_source_bytes:65536,dependencies:64,selected_guidance:64,source_alias_bytes:128,group_tasks:64,group_manifest_bytes:1048576,findings_per_source:64,merge_inputs:64,merged_artifact_bytes:33554432,inline_result_bytes:1048576,artifact_bytes:33554432,chunk_bytes:1048576,per_run_artifact_bytes:268435456,stream_envelopes:32,stream_bytes:4194304,stream_stall_ms:5000,timeline_page_bytes:4194304,pool_servers_max:16,dedicated_generations_max:32,action_rounds_max:32,ledger_generation_bytes:536870912,ledger_generation_records:1000000}),
                ..Default::default()
            })
        })
    }
    async fn inspect_workspace(
        &self,
        request: Request<pb::InspectWorkspaceRequest>,
    ) -> Result<Response<pb::InspectWorkspaceResponse>, Status> {
        execute!(self, request, "RuntimeService.InspectWorkspace", backend, {
            let response = backend.inspect_workspace(old::InspectWorkspaceRequest {
                context: None,
                absolute_path: request.absolute_path,
                expected_workspace_id: request.expected_workspace_id,
            })?;
            Ok(pb::InspectWorkspaceResponse {
                context: Some(context(backend)),
                workspace_id: response.workspace_id,
                canonical_path: response.canonical_path.map(|p| pb::PathProjection {
                    value: p.value.map(|v| match v {
                        old::path_projection::Value::Utf8Path(p) => {
                            pb::path_projection::Value::Utf8Path(p)
                        }
                        old::path_projection::Value::OpaquePath(p) => {
                            pb::path_projection::Value::OpaquePath(p)
                        }
                    }),
                }),
                mode: response.mode,
                status: response.status,
                blockers: response
                    .blockers
                    .into_iter()
                    .map(|b| pb::CapabilityBlocker {
                        code: b.code,
                        safe_message: b.safe_message,
                    })
                    .collect(),
            })
        })
    }
    async fn list_profiles(
        &self,
        request: Request<pb::ListProfilesRequest>,
    ) -> Result<Response<pb::ListProfilesResponse>, Status> {
        execute!(self, request, "RuntimeService.ListProfiles", backend, {
            let _ = request;
            let value = backend.list_profiles(old::ListProfilesRequest::default())?;
            Ok(pb::ListProfilesResponse {
                context: Some(context(backend)),
                items: value.items.into_iter().map(profile).collect(),
            })
        })
    }
    async fn get_profile(
        &self,
        request: Request<pb::GetProfileRequest>,
    ) -> Result<Response<pb::GetProfileResponse>, Status> {
        execute!(self, request, "RuntimeService.GetProfile", backend, {
            let value = backend.get_profile(old::GetProfileRequest {
                profile_name: request.profile_name,
                ..Default::default()
            })?;
            Ok(pb::GetProfileResponse {
                context: Some(context(backend)),
                profile: value.profile.map(profile),
            })
        })
    }
    async fn list_profile_diagnostics(
        &self,
        request: Request<pb::ListProfileDiagnosticsRequest>,
    ) -> Result<Response<pb::ListProfileDiagnosticsResponse>, Status> {
        execute!(
            self,
            request,
            "RuntimeService.ListProfileDiagnostics",
            backend,
            {
                if request.projection != pb::ProjectionProfile::Minimal as i32 {
                    return Err(invalid(
                        "projection",
                        "public Profile diagnostics require the minimal projection",
                    ));
                }
                let page = crate::profile::public_diagnostics(
                    backend.home(),
                    &request.profile_name,
                    &request.after_cursor,
                    if request.limit == 0 {
                        100
                    } else {
                        request.limit
                    },
                )?;
                let items = page
                    .items
                    .into_iter()
                    .enumerate()
                    .map(|(index, (id, kind))| pb::ProfileDiagnostic {
                        cursor: format!(
                            "{}:{}",
                            page.head.split_once(':').expect("diagnostic head").0,
                            page.offset + index + 1
                        ),
                        diagnostic_id: id,
                        severity: if kind == "sixth_dedicated_generation" {
                            "warning"
                        } else {
                            "info"
                        }
                        .into(),
                        message: if kind == "sixth_dedicated_generation" {
                            "Six live dedicated generations are admitted."
                        } else {
                            "Profile diagnostic."
                        }
                        .into(),
                        category: kind,
                        occurred_at: None,
                    })
                    .collect();
                Ok(pb::ListProfileDiagnosticsResponse {
                    context: Some(context(backend)),
                    items,
                    captured_head_cursor: page.head,
                    next_after_cursor: page.next,
                })
            }
        )
    }
}
fn profile(p: old::ProfileProjection) -> pb::ProfileProjection {
    pb::ProfileProjection {
        name: p.name,
        server_key: p.server_key,
        server_epoch: p.server_epoch,
        compatibility: p.compatibility,
        models: p
            .models
            .into_iter()
            .map(|m| pb::ModelCapability {
                model_id: m.model_id,
                is_default: m.is_default,
                supported_efforts: m.supported_efforts,
            })
            .collect(),
        runtime_version: p.runtime_version,
        supported_execution_lanes: p.supported_execution_lanes,
        maximum_assurance: p.maximum_assurance,
        access_policy_transition: p.access_policy_transition,
        background_execution: p
            .background_execution
            .map(|b| pb::BackgroundExecutionCapabilities {
                support: b.support,
                mechanisms: b.mechanisms,
            }),
        supported_interaction_kinds: p.supported_interaction_kinds,
        native_subagents: p.native_subagents.map(|n| pb::NativeSubagentCapabilities {
            lifecycle_observation: n.lifecycle_observation,
            disable_enforcement: n.disable_enforcement,
            quiescence_tracking: n.quiescence_tracking,
            enabled: n.enabled,
            safe_reason: n.safe_reason,
        }),
        feature_flags: p.feature_flags,
        blockers: p
            .blockers
            .into_iter()
            .map(|b| pb::CapabilityBlocker {
                code: b.code,
                safe_message: b.safe_message,
            })
            .collect(),
    }
}
fn unsupported(method: &str) -> Status {
    error_status(
        &MachineError::new(
            "CAPABILITY_UNSUPPORTED",
            "method is not advertised",
            false,
            serde_json::json!({}),
        ),
        method,
    )
}

#[tonic::async_trait]
impl pb::run_service_server::RunService for GatewayTransport {
    async fn start_run(
        &self,
        request: Request<pb::StartRunRequest>,
    ) -> Result<Response<pb::StartRunResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.StartRun",
            backend,
            start(backend, request)
        )
    }
    async fn get_run(
        &self,
        request: Request<pb::GetRunRequest>,
    ) -> Result<Response<pb::GetRunResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.GetRun",
            backend,
            Ok(pb::GetRunResponse {
                context: Some(context(backend)),
                run: Some(get(backend, required(request.run.as_ref(), "run")?)?)
            })
        )
    }
    async fn list_runs(
        &self,
        request: Request<pb::ListRunsRequest>,
    ) -> Result<Response<pb::ListRunsResponse>, Status> {
        execute!(self, request, "RunService.ListRuns", backend, {
            let view = backend.workspace_observed(&workspace(required(
                request.workspace.as_ref(),
                "workspace",
            )?))?;
            let controller = request
                .controller_id
                .as_deref()
                .map(|s| uuid(s, "controller_id"))
                .transpose()?;
            let items = CoreSemanticService
                .list_runs(&view, controller)?
                .into_iter()
                .map(|(snapshot, observation)| {
                    let facts = ProjectionFacts::from_observation(&snapshot, &observation)?;
                    project::run_projection(&snapshot, &facts)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(pb::ListRunsResponse {
                context: Some(context(backend)),
                items,
            })
        })
    }
    async fn submit_turn(
        &self,
        request: Request<pb::SubmitTurnRequest>,
    ) -> Result<Response<pb::SubmitTurnAccepted>, Status> {
        execute!(self, request, "RunService.SubmitTurn", backend, {
            let write = match pb::WriteIntent::try_from(request.write_intent) {
                Ok(pb::WriteIntent::Read) => false,
                Ok(pb::WriteIntent::Write) => true,
                _ => return Err(invalid("write_intent", "explicit intent is required")),
            };
            let images = request
                .images
                .into_iter()
                .map(|i| {
                    let detail = match pb::ImageDetail::try_from(i.detail) {
                        Ok(pb::ImageDetail::Auto) => crate::turn::ImageDetail::Auto,
                        Ok(pb::ImageDetail::Low) => crate::turn::ImageDetail::Low,
                        Ok(pb::ImageDetail::High) => crate::turn::ImageDetail::High,
                        _ => return Err(invalid("image", "explicit detail is required")),
                    };
                    Ok(crate::worker::TurnControlImage {
                        path: PathBuf::from(i.absolute_file_path),
                        detail,
                    })
                })
                .collect::<Result<Vec<_>, MachineError>>()?;
            let (receipt, operation) = mutate(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
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
            let operation = operation.expect("Submit authenticates its original operation");
            let Some(crate::worker::ControlResponseV1::AcceptedReceipt {
                accepted,
                operation_id,
                effective_policy,
                ..
            }) = receipt.response
            else {
                return Err(submit_error(
                    MachineError::new(
                        "INTERNAL_ERROR",
                        "turn admission returned no receipt",
                        false,
                        serde_json::json!({}),
                    ),
                    &operation,
                    false,
                ));
            };
            (|| {
                let facts = acceptance_facts(&receipt.snapshot, &effective_policy)?;
                let mut context = context(backend);
                context.operation_id = Some(operation_id.to_string());
                Ok(pb::SubmitTurnAccepted {
                    context: Some(context.clone()),
                    accepted_turn: Some(project::turn_projection(
                        receipt.snapshot.manifest.run_id,
                        &accepted.thread_id,
                        &accepted.turn_id,
                        pb::TurnStatus::Accepted,
                        &receipt.snapshot.stamp.captured_head_cursor,
                        None,
                    )?),
                    run: Some(project::run_projection(&receipt.snapshot, &facts)?),
                    writer: Some(project::writer_state(&receipt.snapshot, &facts, context)?),
                    idempotency_key: request.idempotency_key,
                    correlation_id: operation_id.to_string(),
                })
            })()
            .map_err(|error| submit_error(error, &operation, true))
        })
    }
    async fn interrupt_turn(
        &self,
        request: Request<pb::InterruptTurnRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.InterruptTurn",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Interrupt
            )
        )
    }
    async fn pause_run(
        &self,
        request: Request<pb::PauseRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.PauseRun",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Pause {
                    interrupt: request.interrupt
                }
            )
        )
    }
    async fn resume_run(
        &self,
        request: Request<pb::ResumeRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.ResumeRun",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Resume
            )
        )
    }
    async fn close_run(
        &self,
        request: Request<pb::CloseRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.CloseRun",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Close {
                    interrupt: request.interrupt
                }
            )
        )
    }
    async fn recover_run(
        &self,
        request: Request<pb::RecoverRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.RecoverRun",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Recover
            )
        )
    }
    async fn reconcile_run(
        &self,
        request: Request<pb::ReconcileRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        execute!(
            self,
            request,
            "RunService.ReconcileRun",
            backend,
            mutation(
                backend,
                required(request.run.as_ref(), "run")?,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::Reconcile
            )
        )
    }
    async fn set_default_effort(
        &self,
        _request: Request<pb::SetDefaultEffortRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        Err(unsupported("RunService.SetDefaultEffort"))
    }
    async fn delete_run(
        &self,
        _request: Request<pb::DeleteRunRequest>,
    ) -> Result<Response<pb::DeleteRunResponse>, Status> {
        Err(unsupported("RunService.DeleteRun"))
    }
    async fn fork_run(
        &self,
        _request: Request<pb::ForkRunRequest>,
    ) -> Result<Response<pb::StartRunResponse>, Status> {
        Err(unsupported("RunService.ForkRun"))
    }
    async fn verify_run(
        &self,
        _request: Request<pb::VerifyRunRequest>,
    ) -> Result<Response<pb::VerifyRunResponse>, Status> {
        Err(unsupported("RunService.VerifyRun"))
    }
    async fn create_write_continuation(
        &self,
        _request: Request<pb::CreateWriteContinuationRequest>,
    ) -> Result<Response<pb::CreateWriteContinuationResponse>, Status> {
        Err(unsupported("RunService.CreateWriteContinuation"))
    }
}
fn writer(
    backend: &dyn ModernGatewayBackend,
    reference: &pb::WorkspaceRef,
) -> Result<pb::WriterState, MachineError> {
    let view = backend.workspace_observed(&workspace(reference))?;
    let root = backend.home().workspace_root(&view.workspace_id);
    let writer = crate::writer::WriterStore::new(
        &root,
        &view.workspace_id,
        crate::darwin::DarwinSystem.current_uid(),
    )
    .load()?;
    if let Some(holder) = writer.holder.as_ref() {
        let (_, snapshot, facts) = snapshot(
            backend,
            &pb::RunRef {
                workspace: Some(reference.clone()),
                run_id: holder.run_id.to_string(),
            },
        )?;
        project::writer_state(&snapshot, &facts, context(backend))
    } else {
        project::ownerless_writer_state(&writer, context(backend))
    }
}
#[tonic::async_trait]
impl pb::writer_service_server::WriterService for GatewayTransport {
    async fn get_workspace_writer_status(
        &self,
        request: Request<pb::GetWorkspaceWriterStatusRequest>,
    ) -> Result<Response<pb::GetWorkspaceWriterStatusResponse>, Status> {
        execute!(
            self,
            request,
            "WriterService.GetWorkspaceWriterStatus",
            backend,
            Ok(pb::GetWorkspaceWriterStatusResponse {
                writer: Some(writer(
                    backend,
                    required(request.workspace.as_ref(), "workspace")?
                )?)
            })
        )
    }
    async fn acquire_writer(
        &self,
        request: Request<pb::AcquireWriterRequest>,
    ) -> Result<Response<pb::WriterState>, Status> {
        execute!(self, request, "WriterService.AcquireWriter", backend, {
            let reference = required(request.run.as_ref(), "run")?;
            mutate(
                backend,
                reference,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::AcquireWriter,
            )?;
            writer(
                backend,
                required(reference.workspace.as_ref(), "workspace")?,
            )
        })
    }
    async fn release_writer(
        &self,
        request: Request<pb::ReleaseWriterRequest>,
    ) -> Result<Response<pb::WriterState>, Status> {
        execute!(self, request, "WriterService.ReleaseWriter", backend, {
            let reference = required(request.run.as_ref(), "run")?;
            mutate(
                backend,
                reference,
                required(request.controller.as_ref(), "controller")?,
                request.expected_state_revision,
                RunMutationOperation::ReleaseWriter,
            )?;
            writer(
                backend,
                required(reference.workspace.as_ref(), "workspace")?,
            )
        })
    }
    async fn prepare_writer_handoff(
        &self,
        _request: Request<pb::PrepareWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        Err(unsupported("WriterService.PrepareWriterHandoff"))
    }
    async fn commit_writer_handoff(
        &self,
        _request: Request<pb::CommitWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        Err(unsupported("WriterService.CommitWriterHandoff"))
    }
    async fn cancel_writer_handoff(
        &self,
        _request: Request<pb::CancelWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        Err(unsupported("WriterService.CancelWriterHandoff"))
    }
}
#[tonic::async_trait]
impl pb::provisioning_service_server::ProvisioningService for GatewayTransport {
    async fn validate_provisioning(
        &self,
        request: Request<pb::ValidateProvisioningRequest>,
    ) -> Result<Response<pb::ValidateProvisioningResponse>, Status> {
        execute!(
            self,
            request,
            "ProvisioningService.ValidateProvisioning",
            backend,
            {
                let input = required(request.input.as_ref(), "input")?;
                let snapshot = provisioning(backend, input, None)?;
                Ok(pb::ValidateProvisioningResponse {
                    context: Some(context(backend)),
                    captured: Some(input.clone()),
                    captured_source_bytes: u32::try_from(snapshot.candidate(false)?.source_bytes)
                        .map_err(|_| {
                        invalid("instructions", "capture exceeds bound")
                    })?,
                    blockers: vec![],
                })
            }
        )
    }
    async fn install_provisioning(
        &self,
        _request: Request<pb::InstallProvisioningRequest>,
    ) -> Result<Response<pb::InstallProvisioningResponse>, Status> {
        Err(unsupported("ProvisioningService.InstallProvisioning"))
    }
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
        _ => return Err(invalid("session", "orchestrated session lifecycle")),
    };
    let composition = match value.composition_state.as_str() {
        "standalone_primary" => pb::OrchestratedSessionComposition::StandalonePrimary,
        "brokered_hierarchy" => pb::OrchestratedSessionComposition::BrokeredHierarchy,
        _ => return Err(invalid("session", "orchestrated session composition")),
    };
    let approval_policy = match value.approval_policy.as_str() {
        "user_approval_required" => pb::OrchestratedSessionApprovalPolicy::UserApprovalRequired,
        "fully_delegated" => pb::OrchestratedSessionApprovalPolicy::FullyDelegated,
        _ => return Err(invalid("session", "orchestrated session approval policy")),
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
        Some(_) => return Err(invalid("session", "orchestrated session close progress")),
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
        captured_at: Some(crate::gateway_service::timestamp_ms(value.captured_at_ms)?),
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
        published_at: Some(crate::gateway_service::timestamp_ms(value.published_at_ms)?),
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

#[tonic::async_trait]
impl pb::orchestration_service_server::OrchestrationService for GatewayTransport {
    async fn get_orchestrated_session(
        &self,
        request: Request<pb::GetOrchestratedSessionRequest>,
    ) -> Result<Response<pb::GetOrchestratedSessionResponse>, Status> {
        execute!(
            self,
            request,
            "OrchestrationService.GetOrchestratedSession",
            backend,
            {
                let reference = required(request.root_run.as_ref(), "root_run")?;
                let (root, snapshot) = protected_snapshot(backend, reference)?;
                let carrier = backend.controller(&controller(required(
                    request.controller.as_ref(),
                    "controller",
                )?))?;
                snapshot.authorize_current_controller(
                    &root,
                    &carrier,
                    "orchestration.session.observe",
                )?;
                let binding = snapshot
                    .manifest
                    .aggregate_binding
                    .as_ref()
                    .filter(|b| {
                        b.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                            && b.member_kind == crate::run::AggregateMemberKind::Primary
                    })
                    .ok_or_else(|| invalid("root_run", "not a Session root"))?;
                let value = crate::orchestration::OrchestrationStore::open_observer(&root)?
                    .observe_session(snapshot.manifest.run_id)?;
                if binding.operation_id != value.bootstrap_operation_id
                    || binding.policy_sha256.as_deref()
                        != Some(value.specialist_policy_sha256.as_str())
                {
                    return Err(invalid("session", "accepted Session binding differs"));
                }
                Ok(pb::GetOrchestratedSessionResponse {
                    context: Some(context(backend)),
                    session: Some(project_orchestrated_session(
                        value,
                        required(reference.workspace.as_ref(), "workspace")?.clone(),
                    )?),
                })
            }
        )
    }
    async fn list_orchestrated_session_results(
        &self,
        request: Request<pb::ListOrchestratedSessionResultsRequest>,
    ) -> Result<Response<pb::ListOrchestratedSessionResultsResponse>, Status> {
        execute!(
            self,
            request,
            "OrchestrationService.ListOrchestratedSessionResults",
            backend,
            {
                if request.projection_version != 2 {
                    return Err(MachineError::new(
                        "UNSUPPORTED_SCHEMA_VERSION",
                        "unsupported Orchestrated Session result projection version",
                        false,
                        serde_json::json!({
                            "schema":"dolgorae.public.v2.OrchestratedSessionResult",
                            "requested":request.projection_version,
                            "supported":[2]
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
                let workspace =
                    required(reference.workspace.as_ref(), "root_run.workspace")?.clone();
                let (state_root, snapshot) = protected_snapshot(backend, reference)?;
                let controller_reference = required(request.controller.as_ref(), "controller")?;
                let carrier = backend.controller(&controller(controller_reference))?;
                let binding = snapshot
                    .manifest
                    .aggregate_binding
                    .as_ref()
                    .filter(|b| {
                        b.aggregate_kind == domain::AggregateKind::OrchestratedSession
                            && b.member_kind == crate::run::AggregateMemberKind::Primary
                    })
                    .ok_or_else(|| invalid("root_run", "not a Session root"))?;
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
                    || binding.policy_sha256.as_deref()
                        != Some(page.specialist_policy_sha256.as_str())
                {
                    return Err(invalid("session", "Orchestrated Session root binding"));
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
                                .ok_or_else(|| invalid("session", "result page continuation"))?
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
                    context: Some(context(backend)),
                    captured_publication_head: page.captured_publication_head,
                    source_revision: page.source_revision,
                    captured_at: Some(crate::gateway_service::timestamp_ms(page.captured_at_ms)?),
                    items,
                    next_page_cursor,
                })
            }
        )
    }
}

fn credential_capabilities(
    value: old::CredentialCarrierCapabilities,
) -> pb::CredentialCarrierCapabilities {
    pb::CredentialCarrierCapabilities {
        schema_id: value.schema_id,
        schema_version: value.schema_version,
        schema_sha256: value.schema_sha256,
        carrier_root_locator: value.carrier_root_locator,
        accepted_controller_kinds: value.accepted_controller_kinds,
        capability_byte_length: value.capability_byte_length,
        capability_encoding: value.capability_encoding,
        parent_directory_mode: value.parent_directory_mode,
        credential_file_mode: value.credential_file_mode,
        same_uid_required: value.same_uid_required,
        regular_file_required: value.regular_file_required,
        symlinks_forbidden: value.symlinks_forbidden,
        create_exclusive_required: value.create_exclusive_required,
        maximum_file_bytes: value.maximum_file_bytes,
        client_descendant_pattern: value.client_descendant_pattern,
        normalized_principal_rule: value.normalized_principal_rule,
        initial_generation: value.initial_generation,
        carrier_root_policy: value.carrier_root_policy,
    }
}

#[tonic::async_trait]
impl pb::controller_service_server::ControllerService for GatewayTransport {
    async fn verify_controller(
        &self,
        request: Request<pb::VerifyControllerRequest>,
    ) -> Result<Response<pb::VerifyControllerResponse>, Status> {
        execute!(
            self,
            request,
            "ControllerService.VerifyController",
            backend,
            {
                let (root, snapshot) =
                    protected_snapshot(backend, required(request.run.as_ref(), "run")?)?;
                let carrier = backend.controller(&controller(required(
                    request.controller.as_ref(),
                    "controller",
                )?))?;
                snapshot.authorize_current_controller(&root, &carrier, "controller.verify")?;
                use crate::ledger::LedgerClock as _;
                Ok(pb::VerifyControllerResponse {
                    context: Some(context(backend)),
                    run_id: snapshot.manifest.run_id.to_string(),
                    controller: Some(project::controller(&snapshot.controller)),
                    verified_at: Some(crate::gateway_event::timestamp(
                        &crate::ledger::SystemLedgerClock::default().timestamp(),
                    )?),
                })
            }
        )
    }
}
fn artifact(value: old::ArtifactRef) -> pb::ArtifactRef {
    pb::ArtifactRef {
        artifact_id: value.artifact_id,
        kind: value.kind,
        visibility: value.visibility,
        media_type: value.media_type,
        byte_length: value.byte_length,
        sha256: value.sha256,
    }
}
#[tonic::async_trait]
impl pb::artifact_service_server::ArtifactService for GatewayTransport {
    async fn get_artifact(
        &self,
        request: Request<pb::GetArtifactRequest>,
    ) -> Result<Response<pb::GetArtifactResponse>, Status> {
        execute!(self, request, "ArtifactService.GetArtifact", backend, {
            let (root, snapshot) =
                protected_snapshot(backend, required(request.run.as_ref(), "run")?)?;
            let carrier = request
                .controller
                .as_ref()
                .map(|reference| backend.controller(&controller(reference)))
                .transpose()?;
            let value = crate::gateway_observation::artifact_metadata(
                &root,
                &snapshot,
                &request.artifact_id,
                carrier.as_ref(),
                backend.context(),
            )?;
            Ok(pb::GetArtifactResponse {
                context: Some(context(backend)),
                artifact: value.artifact.map(artifact),
                maximum_chunk_size: value.maximum_chunk_size,
            })
        })
    }
    async fn read_artifact_chunk(
        &self,
        request: Request<pb::ReadArtifactChunkRequest>,
    ) -> Result<Response<pb::ReadArtifactChunkResponse>, Status> {
        execute!(
            self,
            request,
            "ArtifactService.ReadArtifactChunk",
            backend,
            {
                let (root, snapshot) =
                    protected_snapshot(backend, required(request.run.as_ref(), "run")?)?;
                let carrier = request
                    .controller
                    .as_ref()
                    .map(|reference| backend.controller(&controller(reference)))
                    .transpose()?;
                let value = crate::gateway_observation::artifact_chunk(
                    &root,
                    &snapshot,
                    &request.artifact_id,
                    (request.offset, request.length),
                    carrier.as_ref(),
                    backend.context(),
                )?;
                Ok(pb::ReadArtifactChunkResponse {
                    context: Some(context(backend)),
                    artifact_id: value.artifact_id,
                    offset: value.offset,
                    length: value.length,
                    data: value.data,
                    eof: value.eof,
                    total_byte_length: value.total_byte_length,
                    sha256: value.sha256,
                })
            }
        )
    }
}

fn lookup_subjects(value: &serde_json::Value) -> pb::SubjectIdentities {
    let text = |name: &str| value[name].as_str().map(str::to_owned);
    pb::SubjectIdentities {
        run_id: text("run_ref"),
        member_id: text("member_ref"),
        task_id: text("task_ref"),
        group_id: text("group_ref"),
        result_id: text("result_ref"),
        merge_id: text("merge_ref"),
        engagement_id: text("engagement_ref"),
        maintenance_id: text("maintenance_ref"),
        provisioning_ref: text("provisioning_ref"),
        turn_id: text("turn_ref"),
        interaction_id: text("interaction_ref"),
    }
}
#[tonic::async_trait]
impl pb::operation_service_server::OperationService for GatewayTransport {
    async fn lookup_operation(
        &self,
        request: Request<pb::LookupOperationRequest>,
    ) -> Result<Response<pb::LookupOperationResponse>, Status> {
        execute!(
            self,
            request,
            "OperationService.LookupOperation",
            backend,
            {
                let original = required(request.operation.as_ref(), "operation")?;
                let scope = required(original.caller_scope.as_ref(), "caller_scope")?;
                if scope.workspace_id.len() != 64
                    || !scope
                        .workspace_id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(invalid(
                        "workspace_id",
                        "expected a canonical workspace identity",
                    ));
                }
                let carrier = backend.controller(&controller(required(
                    request.controller.as_ref(),
                    "controller",
                )?))?;
                let binding =
                    crate::controller::binding_from_carrier(&carrier, scope.controller_generation)?;
                let kind = match pb::OperationKind::try_from(original.operation_kind) {
                    Ok(pb::OperationKind::StartRun) => "start_run",
                    Ok(pb::OperationKind::SubmitTurn) => "submit_turn",
                    _ => {
                        return Err(invalid(
                            "operation_kind",
                            "this public lookup supports Run startup and Submit",
                        ));
                    }
                };
                let expected = crate::operation::Namespace::delegation(
                    &binding.identity,
                    &scope.workspace_id,
                    None,
                    kind,
                    &original.idempotency_key,
                )
                .map_err(|reason| invalid("operation", reason))?;
                if scope.controller_kind != project::controller_kind(binding.identity.kind) as i32
                    || scope.controller_generation != binding.identity.generation
                    || scope.principal_ref != expected.controller_principal
                {
                    return Err(invalid(
                        "caller_scope",
                        "authenticated original operation scope differs",
                    ));
                }
                let root = backend.home().workspace_root(&scope.workspace_id);
                let value = match (&scope.aggregate, kind) {
                    (Some(pb::caller_scope::Aggregate::ProvisioningScope(value)), "start_run")
                        if value == "run-start" =>
                    {
                        crate::semantic::lookup_start_operation_at(
                            &root,
                            &scope.workspace_id,
                            &binding.identity,
                            &original.idempotency_key,
                        )?
                    }
                    (Some(pb::caller_scope::Aggregate::RunId(run)), "submit_turn") => {
                        crate::semantic::lookup_submit_operation_at(
                            &root,
                            &scope.workspace_id,
                            uuid(run, "run_id")?,
                            &carrier,
                            &original.idempotency_key,
                        )?
                    }
                    _ => {
                        return Err(invalid(
                            "caller_scope",
                            "original Run operation scope is required",
                        ));
                    }
                };
                use pb::lookup_operation_response::Result as Lookup;
                let result = match value["state"].as_str() {
                    Some("accepted") => {
                        let receipt = &value["receipt"];
                        let outcome = match value["current_outcome"].as_str() {
                            Some("completed") => pb::OperationOutcome::Completed,
                            Some("failed") => pb::OperationOutcome::Failed,
                            Some("cancelled") => pb::OperationOutcome::Cancelled,
                            Some("expired") => pb::OperationOutcome::Expired,
                            Some("running") => pb::OperationOutcome::Running,
                            Some("pending") => pb::OperationOutcome::Pending,
                            _ => pb::OperationOutcome::Unknown,
                        };
                        Lookup::Accepted(pb::AcceptedOperation {
                            receipt: Some(pb::OperationReceipt {
                                operation: Some(original.clone()),
                                accepted_input_digest: receipt["accepted_input_digest"]
                                    .as_str()
                                    .ok_or_else(|| {
                                        invalid("receipt", "accepted input identity is missing")
                                    })?
                                    .into(),
                                receipt_id: receipt["operation_ref"]
                                    .as_str()
                                    .ok_or_else(|| {
                                        invalid("receipt", "original receipt identity is missing")
                                    })?
                                    .into(),
                                subjects: Some(lookup_subjects(&value["accepted_subjects"])),
                                effect: pb::EffectState::Accepted as i32,
                                exact_replay: false,
                            }),
                            result: None,
                            current_outcome: outcome as i32,
                        })
                    }
                    Some("not_accepted") => Lookup::NotAccepted(pb::NotAcceptedOperation {
                        operation: Some(original.clone()),
                        input_digest: value["input_digest"]
                            .as_str()
                            .ok_or_else(|| invalid("lookup", "durable refusal digest is missing"))?
                            .into(),
                        definitive_refusal_code: value["refusal_code"]
                            .as_str()
                            .ok_or_else(|| invalid("lookup", "durable refusal code is missing"))?
                            .into(),
                        original_key_retry_permitted: value["exact_key_retry_permitted"]
                            .as_bool()
                            .unwrap_or(false),
                    }),
                    Some("absent") => {
                        let proof = &value["proof"];
                        if proof["authoritative"] != true
                            || proof["complete"] != true
                            || proof["tombstones_checked"] != true
                        {
                            return Err(invalid(
                                "lookup",
                                "authoritative absence is not established",
                            ));
                        }
                        Lookup::Absent(pb::AbsentOperation {
                            operation: Some(original.clone()),
                            retained_namespace_fence: format!(
                                "{}:{}:{}",
                                scope.workspace_id,
                                scope.principal_ref,
                                proof["retained_namespace_generation"]
                            ),
                            durable_watermark: proof["durable_watermark"]
                                .as_u64()
                                .ok_or_else(|| invalid("lookup", "durable watermark is missing"))?,
                            complete_authoritative_lookup: true,
                        })
                    }
                    Some("unresolved") => Lookup::Unresolved(pb::UnresolvedOperation {
                        operation: Some(original.clone()),
                        known_subjects: Some(lookup_subjects(&value["known_subjects"])),
                        effect: pb::EffectState::Unresolved as i32,
                        required_action: pb::RequiredClientAction::ReconcileOperation as i32,
                    }),
                    _ => {
                        return Err(invalid(
                            "lookup",
                            "original operation outcome is unavailable",
                        ));
                    }
                };
                Ok(pb::LookupOperationResponse {
                    context: Some(context(backend)),
                    result: Some(result),
                })
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(status: &Status, label: &str) -> pb::DolgoraeErrorDetail {
        use base64::Engine as _;
        use prost::Message as _;
        let rich = crate::gateway::GoogleRpcStatus::decode(status.details()).unwrap();
        assert_eq!(
            rich.details[0].type_url,
            "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail"
        );
        println!(
            "v2-consumer-detail {} {}",
            label,
            base64::engine::general_purpose::STANDARD.encode(&rich.details[0].value)
        );
        pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap()
    }

    #[test]
    fn internal_and_unmapped_mutation_failures_do_not_prove_non_effect() {
        for code in ["INTERNAL_ERROR", "UNREGISTERED_PRIVATE_ERROR"] {
            let error = MachineError::new(code, "private-canary", false, serde_json::json!({}));
            let status = error_status(&error, "WriterService.AcquireWriter");
            let detail = detail(&status, code);
            assert_eq!(detail.dolgorae_error_code, "INTERNAL_ERROR");
            assert_eq!(detail.effect, pb::EffectState::Unresolved as i32);
            assert_eq!(
                detail.classification,
                pb::FailureClassification::Unknown as i32
            );
            assert_eq!(
                detail.action,
                pb::RequiredClientAction::ReconcileOperation as i32
            );
            assert!(!format!("{status:?}").contains("private-canary"));
        }
        let error = MachineError::new(
            "INTERNAL_ERROR",
            "read failed",
            false,
            serde_json::json!({}),
        );
        assert_eq!(
            detail(&error_status(&error, "RunService.GetRun"), "read-internal").effect,
            pb::EffectState::None as i32
        );
    }

    #[test]
    fn submit_transport_and_accepted_projection_failures_retain_original_scope() {
        let run_id = uuid::Uuid::now_v7();
        let controller = domain::ControllerIdentity {
            controller_id: uuid::Uuid::now_v7(),
            kind: domain::ControllerKind::Automation,
            instance_id: "v2-error-test".into(),
            subject_id: None,
            generation: 3,
        };
        let workspace = "a".repeat(64);
        let operation =
            submit_operation(&controller, &workspace, run_id, "original-submit").unwrap();
        let error = crate::worker::WorkerProtocolError::Io
            .machine_error(run_id, std::path::Path::new("private-path-canary"));
        let status = error_status(
            &submit_error(error, &operation, false),
            "RunService.SubmitTurn",
        );
        let failed = detail(&status, "submit-transport");
        assert_eq!(failed.effect, pb::EffectState::Unresolved as i32);
        assert_eq!(
            failed.classification,
            pb::FailureClassification::Unknown as i32
        );
        assert_eq!(failed.run_id.as_deref(), Some(run_id.to_string().as_str()));
        assert_eq!(failed.idempotency_key.as_deref(), Some("original-submit"));
        let reference = failed.operation.unwrap();
        assert_eq!(
            reference.operation_kind,
            pb::OperationKind::SubmitTurn as i32
        );
        let scope = reference.caller_scope.unwrap();
        assert_eq!(scope.controller_generation, 3);
        assert_eq!(scope.workspace_id, workspace);
        assert_eq!(
            scope.principal_ref,
            operation["principal_ref"].as_str().unwrap()
        );
        assert_eq!(
            scope.aggregate,
            Some(pb::caller_scope::Aggregate::RunId(run_id.to_string()))
        );
        assert!(!format!("{status:?}").contains("private-path-canary"));

        let projection_error = project::turn_projection(
            run_id,
            "",
            "accepted-turn",
            pb::TurnStatus::Accepted,
            "0",
            None,
        )
        .unwrap_err();
        let accepted = detail(
            &error_status(
                &submit_error(projection_error, &operation, true),
                "RunService.SubmitTurn",
            ),
            "accepted-projection",
        );
        assert_eq!(accepted.effect, pb::EffectState::Accepted as i32);
        assert_eq!(
            accepted.classification,
            pb::FailureClassification::RecoveryNeeded as i32
        );
        assert_eq!(
            accepted.action,
            pb::RequiredClientAction::ReconcileOperation as i32
        );
        assert_eq!(
            accepted.operation.unwrap().idempotency_key,
            "original-submit"
        );
    }

    #[tokio::test]
    async fn failed_mutation_executor_join_preserves_a_completed_effect() {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::sync::Arc;
        struct UnusedBackend;
        impl GatewayBackend for UnusedBackend {}
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let transport = GatewayTransport::new(Arc::new(UnusedBackend), receiver);
        let marker = std::env::temp_dir().join(format!("v2-accepted-{}", uuid::Uuid::now_v7()));
        let writer = marker.clone();
        let status = transport
            .execute_v2::<()>(
                "RunService.SubmitTurn",
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                move |_| {
                    use std::io::Write as _;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(writer)
                        .unwrap();
                    file.write_all(b"accepted before join failure").unwrap();
                    file.sync_all().unwrap();
                    panic!("controlled post-effect executor failure");
                },
            )
            .await
            .unwrap_err();
        let bytes = std::fs::read(&marker).unwrap();
        std::fs::remove_file(marker).unwrap();
        assert_eq!(bytes, b"accepted before join failure");
        let detail = detail(&status, "executor-join");
        assert_eq!(detail.dolgorae_error_code, "INTERNAL_ERROR");
        assert_eq!(detail.effect, pb::EffectState::Unresolved as i32);
        assert_eq!(
            detail.classification,
            pb::FailureClassification::Unknown as i32
        );
    }
    #[test]
    fn captured_instructions_are_bound_to_every_source_and_identity() {
        let snapshot = crate::delegation_identity::InstructionSnapshot::compose(
            crate::instructions::SourceCapture::from_body("common\\n").unwrap(),
            crate::instructions::RoleCapture::from_body("reader", "read only\\r\\n").unwrap(),
            vec![],
            vec![],
        )
        .unwrap();
        let capture = project::captured_instructions(&snapshot);
        assert_eq!(decode_capture(&capture).unwrap(), snapshot);
        let mut changed = capture.clone();
        changed.role_body.as_mut().unwrap().utf8[0] ^= 1;
        assert_eq!(
            decode_capture(&changed).unwrap_err().code,
            "INVALID_ARGUMENT"
        );
        let mut changed = capture;
        changed
            .instruction_identity
            .as_mut()
            .unwrap()
            .composition
            .as_mut()
            .unwrap()
            .byte_length += 1;
        assert_eq!(
            decode_capture(&changed).unwrap_err().code,
            "INVALID_ARGUMENT"
        );
    }
    #[test]
    fn capacity_detail_preserves_original_authenticated_operation_without_effect() {
        let error = MachineError::new(
            "POOL_CAPACITY_EXCEEDED",
            "private-canary",
            true,
            serde_json::json!({"public_v2_operation":{"controller_kind":4,"principal_ref":"sha256:principal","controller_generation":1,"workspace_id":"workspace","idempotency_key":"original-key"}}),
        );
        let status = error_status(&error, "RunService.StartRun");
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        let detail = detail(&status, "capacity-refusal");
        assert_eq!(detail.operation_kind, pb::OperationKind::StartRun as i32);
        assert_eq!(detail.effect, pb::EffectState::None as i32);
        assert_eq!(
            detail.classification,
            pb::FailureClassification::Rejected as i32
        );
        assert_eq!(detail.operation.unwrap().idempotency_key, "original-key");
        assert!(!format!("{status:?}").contains("private-canary"));
    }
}
