//! Public managed task reads; execution and retained identity stay semantic-owned.
use super::*;
use crate::delegation_identity::AcceptedTask;
use serde_json::{Value, json};

fn source_error(field: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "managed task source is unavailable",
        false,
        json!({"field":field}),
    )
}

fn accepted(value: &AcceptedTask) -> Result<pb::AcceptedTaskIdentity, MachineError> {
    value.digest()?;
    let bytes = |v: &crate::delegation_identity::BytesRef| pb::BytesRef {
        byte_length: v.byte_length,
        sha256: v.sha256.clone(),
    };
    Ok(pb::AcceptedTaskIdentity {
        schema: value.schema.clone(),
        purpose: value.purpose.clone(),
        task_body_schema: value.task_body_schema.clone(),
        task_body: Some(bytes(&value.task_body)),
        contexts: value
            .contexts
            .iter()
            .map(|v| pb::ContextRef {
                kind: v.kind.clone(),
                identity: v.identity.clone(),
                content: Some(bytes(&v.content)),
                media_type: v.media_type.clone(),
            })
            .collect(),
        images: value
            .images
            .iter()
            .map(|v| {
                Ok(pb::AcceptedImageRef {
                    content: Some(bytes(&v.content)),
                    media_type: v.media_type.clone(),
                    detail: match v.detail.as_str() {
                        "auto" => pb::ImageDetail::Auto,
                        "low" => pb::ImageDetail::Low,
                        "high" => pb::ImageDetail::High,
                        _ => return Err(source_error("image.detail")),
                    } as i32,
                })
            })
            .collect::<Result<_, MachineError>>()?,
        expected_output_schema: value.expected_output_schema.clone(),
        execution_intent: match value.execution_intent.as_str() {
            "read_only" => pb::CreationMode::ReadOnly,
            "canonical_workspace_write" => pb::CreationMode::Writable,
            _ => return Err(source_error("execution_intent")),
        } as i32,
        admitted_model: value.admitted_model.clone(),
        admitted_effort: value.admitted_effort.clone(),
        role_snapshot_digest: value.role_snapshot_digest.clone(),
        agent_configuration_digest: value.agent_configuration_digest.clone(),
    })
}

pub(crate) fn projection(
    scope: pb::ManagedScope,
    value: &Value,
    context: pb::ResponseContext,
) -> Result<pb::CollectSpecialistTaskResponse, MachineError> {
    let task: crate::delegation_service::DelegationTaskObservation =
        serde_json::from_value(value["task"].clone()).map_err(|_| source_error("task"))?;
    let identity: AcceptedTask = serde_json::from_value(value["accepted_task"].clone())
        .map_err(|_| source_error("accepted_task"))?;
    if task.accepted_task_digest != identity.digest()? {
        return Err(source_error("accepted_task_digest"));
    }
    let disposition = match value["disposition"].as_str() {
        Some("idle") => pb::MemberDisposition::Idle,
        Some("busy") => pb::MemberDisposition::Busy,
        Some("released") => pb::MemberDisposition::Released,
        Some("unresolved") => pb::MemberDisposition::Unresolved,
        Some("passivated") => pb::MemberDisposition::Passivated,
        Some("recovery_needed") => pb::MemberDisposition::RecoveryNeeded,
        _ => return Err(source_error("disposition")),
    };
    let mut results = groups::result_rows(value)?;
    if results.len() > 1 {
        return Err(source_error("results"));
    }
    let result = results.pop();
    if result.as_ref().map(|r| r.result_digest.as_str()) != task.result_digest.as_deref() {
        return Err(source_error("result_digest"));
    }
    if let Some(result) = &result {
        let source = result
            .identity
            .as_ref()
            .and_then(|v| v.source.as_ref())
            .ok_or_else(|| source_error("result.source"))?;
        if source.scope_kind != "orchestrated_session"
            || source.scope_ref != scope.session_id
            || source.worker_ref != task.member_ref
            || source.task_ref != task.task_ref
            || source.accepted_task_digest != task.accepted_task_digest
            || source.role_snapshot_digest != identity.role_snapshot_digest
            || source.agent_configuration_digest != identity.agent_configuration_digest
        {
            return Err(source_error("result.source"));
        }
    }
    let root = required(scope.root_primary_run.as_ref(), "root_primary_run")?.clone();
    let text = |name: &str| {
        value[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| source_error(name))
    };
    let role = required(value.get("role_body"), "role_body")?;
    let role_digest = role["sha256"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| source_error("role_body.sha256"))?;
    let facts = pb::DelegationIdentity {
        schema: "dolgorae.delegation-identity/v1".into(),
        scope: Some(pb::DelegationScope {
            scope: Some(pb::delegation_scope::Scope::Managed(scope.clone())),
        }),
        worker_ref: task.member_ref.clone(),
        task_ref: Some(task.task_ref.clone()),
        role_body_sha256: role_digest.into(),
        role_body_byte_length: role["byte_length"]
            .as_u64()
            .ok_or_else(|| source_error("role_body.byte_length"))?,
        role_snapshot_digest: text("role_snapshot_digest")?,
        instruction_identity_digest: text("instruction_identity_digest")?,
        agent_configuration_digest: identity.agent_configuration_digest.clone(),
        accepted_task_digest: Some(task.accepted_task_digest.clone()),
        result_id: result.as_ref().map(|r| r.result_id.clone()),
        result_digest: task.result_digest.clone(),
        rounds: u32::try_from(
            value["rounds"]
                .as_u64()
                .ok_or_else(|| source_error("rounds"))?,
        )
        .map_err(|_| source_error("rounds"))?,
        disposition: disposition as i32,
        source_revision: value["source_revision"]
            .as_u64()
            .ok_or_else(|| source_error("source_revision"))?,
        accepted_operation: None,
    };
    if facts.role_snapshot_digest != identity.role_snapshot_digest {
        return Err(source_error("role_snapshot_digest"));
    }
    let outcome = groups::outcome(&task.state)?;
    if result.as_ref().is_some_and(|r| {
        r.identity
            .as_ref()
            .is_none_or(|i| i.terminal_outcome != outcome)
    }) {
        return Err(source_error("task.outcome"));
    }
    Ok(pb::CollectSpecialistTaskResponse {
        context: Some(context),
        task: Some(pb::TaskProjection {
            task_id: task.task_ref,
            member_id: task.member_ref.clone(),
            outcome,
            identity: Some(accepted(&identity)?),
            accepted_task_digest: task.accepted_task_digest,
            result,
            recovery: None,
        }),
        identity: Some(pb::ManagedDelegationIdentity {
            scope: Some(scope),
            root_primary_run: Some(root.clone()),
            specialist_run: Some(pb::RunRef {
                workspace: root.workspace,
                run_id: task.member_ref,
            }),
            facts: Some(facts),
        }),
    })
}

#[tonic::async_trait]
impl pb::broker_service_server::BrokerService for GatewayTransport {
    async fn collect_specialist_task(
        &self,
        request: Request<pb::CollectSpecialistTaskRequest>,
    ) -> Result<Response<pb::CollectSpecialistTaskResponse>, Status> {
        execute!(
            self,
            request,
            "BrokerService.CollectSpecialistTask",
            backend,
            {
                let scope = required(request.scope.as_ref(), "scope")?;
                let root = required(scope.root_primary_run.as_ref(), "scope.root_primary_run")?;
                let view = backend.workspace_observed(&workspace(required(
                    root.workspace.as_ref(),
                    "root.workspace",
                )?))?;
                let carrier = backend.controller(&controller(required(
                    request.controller.as_ref(),
                    "controller",
                )?))?;
                let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
                let value = crate::delegation_service::research_group::public_collect_managed_task(
                    backend.home(),
                    &view,
                    &carrier,
                    uuid(&root.run_id, "root.run_id")?,
                    uuid(&scope.session_id, "session_id")?,
                    uuid(&request.task_id, "task_id")?,
                    &port,
                )?;
                groups::bounded(projection(scope.clone(), &value, context(backend))?)
            }
        )
    }
    async fn spawn_specialist(
        &self,
        _: Request<pb::SpawnSpecialistRequest>,
    ) -> Result<Response<pb::SpawnSpecialistResponse>, Status> {
        Err(unsupported("BrokerService.SpawnSpecialist"))
    }
    async fn assign_specialist_task(
        &self,
        _: Request<pb::AssignSpecialistTaskRequest>,
    ) -> Result<Response<pb::AssignSpecialistTaskResponse>, Status> {
        Err(unsupported("BrokerService.AssignSpecialistTask"))
    }
    async fn await_specialist_task(
        &self,
        _: Request<pb::AwaitSpecialistTaskRequest>,
    ) -> Result<Response<pb::AwaitSpecialistTaskResponse>, Status> {
        Err(unsupported("BrokerService.AwaitSpecialistTask"))
    }
    async fn cancel_specialist_task(
        &self,
        _: Request<pb::CancelSpecialistTaskRequest>,
    ) -> Result<Response<pb::CancelSpecialistTaskResponse>, Status> {
        Err(unsupported("BrokerService.CancelSpecialistTask"))
    }
    async fn release_specialist(
        &self,
        _: Request<pb::ReleaseSpecialistRequest>,
    ) -> Result<Response<pb::ReleaseSpecialistResponse>, Status> {
        Err(unsupported("BrokerService.ReleaseSpecialist"))
    }
    async fn get_managed_delegation_identity(
        &self,
        _: Request<pb::GetManagedDelegationIdentityRequest>,
    ) -> Result<Response<pb::GetManagedDelegationIdentityResponse>, Status> {
        Err(unsupported("BrokerService.GetManagedDelegationIdentity"))
    }
}
