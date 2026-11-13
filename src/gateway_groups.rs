//! Public group transport: credentials and execution remain with the semantic owner.
use super::*;
use serde_json::{Value, json};

fn projection_unavailable(field: &str, message: &str) -> MachineError {
    MachineError::new("INTERNAL_ERROR", message, false, json!({"field":field}))
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, MachineError> {
    value[field]
        .as_str()
        .ok_or_else(|| projection_unavailable(field, "authoritative group field is unavailable"))
}

fn bytes(value: &Value) -> Result<pb::BytesRef, MachineError> {
    let digest = text(value, "sha256")?;
    if digest.strip_prefix("sha256:").is_none_or(|hex| {
        hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(projection_unavailable(
            "content.sha256",
            "invalid content identity",
        ));
    }
    Ok(pb::BytesRef {
        byte_length: value["byte_length"]
            .as_u64()
            .ok_or_else(|| projection_unavailable("content", "byte length is unavailable"))?,
        sha256: digest.into(),
    })
}

pub(super) fn outcome(value: &str) -> Result<i32, MachineError> {
    Ok(match value {
        "completed" => pb::TaskOutcome::Completed,
        "failed" => pb::TaskOutcome::Failed,
        "cancelled" => pb::TaskOutcome::Cancelled,
        "expired" => pb::TaskOutcome::Expired,
        "unknown" => pb::TaskOutcome::Unknown,
        "missing" => pb::TaskOutcome::Missing,
        "queued" | "claimed" | "dispatching" => pb::TaskOutcome::Queued,
        "running" => pb::TaskOutcome::Running,
        _ => {
            return Err(projection_unavailable(
                "state",
                "unrepresentable group source state",
            ));
        }
    } as i32)
}

fn context_value(value: &pb::ContextRef) -> Result<Value, MachineError> {
    let content = required(value.content.as_ref(), "context.content")?;
    Ok(
        json!({"kind":value.kind,"identity":value.identity,"content":{"byte_length":content.byte_length,"sha256":content.sha256},"media_type":value.media_type}),
    )
}
fn accepted_task_value(value: &pb::AcceptedTaskIdentity) -> Result<Value, MachineError> {
    let body = required(value.task_body.as_ref(), "identity.task_body")?;
    let intent = match pb::CreationMode::try_from(value.execution_intent) {
        Ok(pb::CreationMode::ReadOnly) => "read_only",
        Ok(pb::CreationMode::Writable) => "canonical_workspace_write",
        _ => return Err(invalid("execution_intent", "unknown execution intent")),
    };
    let images=value.images.iter().map(|image| {
        let content=required(image.content.as_ref(),"images.content")?;
        let detail=match pb::ImageDetail::try_from(image.detail) {Ok(pb::ImageDetail::Auto)=>"auto",Ok(pb::ImageDetail::Low)=>"low",Ok(pb::ImageDetail::High)=>"high",_=>return Err(invalid("images.detail","unknown image detail"))};
        Ok(json!({"content":{"byte_length":content.byte_length,"sha256":content.sha256},"media_type":image.media_type,"detail":detail}))
    }).collect::<Result<Vec<_>,MachineError>>()?;
    Ok(
        json!({"schema":value.schema,"purpose":value.purpose,"task_body_schema":value.task_body_schema,"task_body":{"byte_length":body.byte_length,"sha256":body.sha256},"contexts":value.contexts.iter().map(context_value).collect::<Result<Vec<_>,MachineError>>()?,"images":images,"expected_output_schema":value.expected_output_schema,"execution_intent":intent,"admitted_model":value.admitted_model,"admitted_effort":value.admitted_effort,"role_snapshot_digest":value.role_snapshot_digest,"agent_configuration_digest":value.agent_configuration_digest}),
    )
}

fn source_value(value: &pb::GroupSource) -> Result<Value, MachineError> {
    let state = match pb::TaskOutcome::try_from(value.state)
        .map_err(|_| invalid("sources.state", "unknown source outcome"))?
    {
        pb::TaskOutcome::Completed => "completed",
        pb::TaskOutcome::Failed => "failed",
        pb::TaskOutcome::Cancelled => "cancelled",
        pb::TaskOutcome::Expired => "expired",
        pb::TaskOutcome::Unknown => "unknown",
        pb::TaskOutcome::Missing => "missing",
        _ => {
            return Err(invalid(
                "sources.state",
                "merge sources must be terminal, unknown or missing",
            ));
        }
    };
    Ok(
        json!({"entry_key":value.entry_key,"worker_ref":value.worker_ref,"task_ref":value.task_ref,"role_snapshot_digest":value.role_snapshot_digest,"agent_configuration_digest":value.agent_configuration_digest,"accepted_task_digest":value.accepted_task_digest,"state":state,"result_digest":value.result_digest,"safe_failure_code":value.safe_failure_code}),
    )
}
fn source_projection(value: &Value) -> Result<pb::GroupSource, MachineError> {
    let optional = |key: &str| value[key].as_str().map(str::to_owned);
    Ok(pb::GroupSource {
        entry_key: text(value, "entry_key")?.into(),
        worker_ref: optional("worker_ref"),
        task_ref: optional("task_ref"),
        role_snapshot_digest: optional("role_snapshot_digest"),
        agent_configuration_digest: optional("agent_configuration_digest"),
        accepted_task_digest: optional("accepted_task_digest"),
        state: outcome(text(value, "state")?)?,
        result_digest: optional("result_digest"),
        safe_failure_code: optional("safe_failure_code"),
    })
}

fn projection(value: &Value) -> Result<pb::GroupProjection, MachineError> {
    let response = &value["response"];
    let coverage = &response["coverage"];
    let manifest = &value["manifest"];
    let entries = manifest["entries"]
        .as_array()
        .ok_or_else(|| projection_unavailable("manifest", "retained manifest is unavailable"))?
        .iter()
        .map(|entry| {
            Ok(pb::GroupAssignmentEntry {
                entry_key: text(entry, "entry_key")?.into(),
                role_snapshot_digest: text(entry, "role_snapshot_digest")?.into(),
                agent_configuration_digest: text(entry, "agent_configuration_digest")?.into(),
                accepted_task_digest: text(entry, "accepted_task_digest")?.into(),
            })
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    let sources = coverage["sources"]
        .as_array()
        .ok_or_else(|| {
            projection_unavailable("coverage", "complete group coverage is unavailable")
        })?
        .iter()
        .map(|source| {
            let optional = |key: &str| -> Result<Option<String>, MachineError> {
                if source[key].is_null() {
                    Ok(None)
                } else {
                    text(source, key).map(|s| Some(s.to_owned()))
                }
            };
            Ok(pb::GroupSource {
                entry_key: text(source, "entry_key")?.into(),
                worker_ref: optional("worker_ref")?,
                task_ref: optional("task_ref")?,
                role_snapshot_digest: optional("role_snapshot_digest")?,
                agent_configuration_digest: optional("agent_configuration_digest")?,
                accepted_task_digest: optional("accepted_task_digest")?,
                state: outcome(text(source, "state")?)?,
                result_digest: optional("result_digest")?,
                safe_failure_code: optional("safe_failure_code")?,
            })
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    Ok(pb::GroupProjection {
        group_id: text(coverage, "group_ref")?.into(),
        assignment: Some(pb::GroupAssignmentIdentity {
            schema: text(manifest, "schema")?.into(),
            scope_kind: text(manifest, "scope_kind")?.into(),
            scope_ref: text(manifest, "scope_ref")?.into(),
            entries,
        }),
        group_assignment_digest: text(coverage, "group_assignment_digest")?.into(),
        coverage: sources,
        source_revision: value["source_revision"].as_u64().ok_or_else(|| {
            projection_unavailable("source_revision", "current revision is unavailable")
        })?,
    })
}

fn scope(
    value: &pb::DelegationScope,
) -> Result<
    (
        old::WorkspaceRef,
        Option<uuid::Uuid>,
        &'static str,
        uuid::Uuid,
    ),
    MachineError,
> {
    match required(value.scope.as_ref(), "scope")? {
        pb::delegation_scope::Scope::External(external) => Ok((
            workspace(required(external.workspace.as_ref(), "scope.workspace")?),
            None,
            "external_engagement",
            uuid(&external.engagement_id, "engagement_id")?,
        )),
        pb::delegation_scope::Scope::Managed(managed) => {
            let root = required(managed.root_primary_run.as_ref(), "root_primary_run")?;
            Ok((
                workspace(required(root.workspace.as_ref(), "scope.workspace")?),
                Some(uuid(&root.run_id, "root_primary_run.run_id")?),
                "orchestrated_session",
                uuid(&managed.session_id, "session_id")?,
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn call(
    backend: &dyn ModernGatewayBackend,
    selected: &pb::DelegationScope,
    carrier: &pb::ControllerCarrierRef,
    operation: &str,
    key: Option<&str>,
    revision: Option<u64>,
    mut request: Value,
) -> Result<Value, MachineError> {
    let (reference, primary, kind, aggregate) = scope(selected)?;
    let view = backend.workspace(&reference)?;
    let carrier = backend.controller(&controller(carrier))?;
    request["schema"] = json!("dolgorae.external-specialist-request/v5");
    request["operation"] = json!(operation);
    request["scope_kind"] = json!(kind);
    request["scope_ref"] = json!(aggregate);
    if let Some(key) = key {
        request["idempotency_key"] = json!(key);
        request["expected_revision"] = json!(revision);
    }
    let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
    crate::delegation_service::research_group::public_call(
        backend.home(),
        &view,
        &carrier,
        primary,
        kind,
        aggregate,
        operation,
        key,
        revision,
        &request,
        &port,
    )
}

fn receipt(value: &Value) -> Result<pb::OperationReceipt, MachineError> {
    let retained: crate::delegation_identity::OperationReceipt =
        serde_json::from_value(value["response"]["receipt"].clone())
            .map_err(|_| projection_unavailable("receipt", "retained receipt is unavailable"))?;
    retained
        .validate()
        .map_err(|_| projection_unavailable("receipt", "retained receipt is invalid"))?;
    retained
        .verify_digest()
        .map_err(|_| projection_unavailable("receipt", "retained receipt digest differs"))?;
    let namespace = &retained.namespace;
    let aggregate = match namespace.scope_kind.as_deref() {
        Some("external_engagement") => pb::caller_scope::Aggregate::ExternalEngagementId(
            namespace
                .scope_ref
                .clone()
                .ok_or_else(|| projection_unavailable("scope", "missing aggregate"))?,
        ),
        Some("orchestrated_session") => pb::caller_scope::Aggregate::OrchestratedSessionId(
            namespace
                .scope_ref
                .clone()
                .ok_or_else(|| projection_unavailable("scope", "missing aggregate"))?,
        ),
        _ => return Err(projection_unavailable("scope", "invalid group aggregate")),
    };
    let kind: domain::ControllerKind = serde_json::from_value(value["controller_kind"].clone())
        .map_err(|_| {
            projection_unavailable(
                "controller_kind",
                "authenticated Controller kind unavailable",
            )
        })?;
    Ok(pb::OperationReceipt {
        operation: Some(pb::OperationRef {
            caller_scope: Some(pb::CallerScope {
                controller_kind: project::controller_kind(kind) as i32,
                principal_ref: namespace.controller_principal.clone(),
                controller_generation: namespace.controller_generation,
                workspace_id: namespace.workspace_ref.clone(),
                aggregate: Some(aggregate),
            }),
            operation_kind: match namespace.operation_kind.as_str() {
                "assign_group" => pb::OperationKind::AssignGroup,
                "merge_group_results" => pb::OperationKind::MergeGroupResults,
                "assign_synthesis" => pb::OperationKind::AssignSynthesis,
                _ => {
                    return Err(projection_unavailable(
                        "operation_kind",
                        "unexpected receipt operation",
                    ));
                }
            } as i32,
            idempotency_key: namespace.idempotency_key.clone(),
        }),
        accepted_input_digest: retained.accepted_input_digest,
        receipt_id: retained.operation_ref,
        subjects: Some(lookup_subjects(
            &serde_json::to_value(retained.accepted_subjects)
                .map_err(|_| projection_unavailable("receipt", "unrepresentable subjects"))?,
        )),
        effect: pb::EffectState::Accepted as i32,
        exact_replay: value["exact_replay"]
            .as_bool()
            .ok_or_else(|| projection_unavailable("receipt", "replay observation unavailable"))?,
    })
}

pub(super) fn bounded<T: prost::Message>(response: T) -> Result<T, MachineError> {
    if response.encoded_len() > 4 * 1024 * 1024 {
        return Err(MachineError::new(
            "ARTIFACT_CAPACITY_EXCEEDED",
            "group response exceeds the bounded page size",
            false,
            json!({}),
        ));
    }
    Ok(response)
}

fn accepted_projection_error(mut error: MachineError, value: &Value) -> MachineError {
    let retained = &value["response"]["receipt"];
    let ns = &retained["namespace"];
    let Ok(kind) =
        serde_json::from_value::<domain::ControllerKind>(value["controller_kind"].clone())
    else {
        return error;
    };
    let mut details = error.details.as_object().cloned().unwrap_or_default();
    details.insert("public_v2_operation".into(), json!({
        "controller_kind":project::controller_kind(kind) as i32,
        "principal_ref":ns["controller_principal"],"controller_generation":ns["controller_generation"],
        "workspace_id":ns["workspace_ref"],"scope_kind":ns["scope_kind"],"scope_ref":ns["scope_ref"],
        "idempotency_key":ns["idempotency_key"]}));
    details.insert("public_v2_accepted".into(), json!(true));
    for subject in ["group_ref", "merge_ref", "task_ref"] {
        details.insert(
            subject.into(),
            retained["accepted_subjects"][subject].clone(),
        );
    }
    error.details = Value::Object(details);
    error
}

fn identity_bytes(value: &crate::delegation_identity::BytesRef) -> pb::BytesRef {
    pb::BytesRef {
        byte_length: value.byte_length,
        sha256: value.sha256.clone(),
    }
}

fn content(value: &Value) -> Result<Option<pb::ContentRef>, MachineError> {
    use base64::Engine as _;
    use pb::content_ref::Representation;
    if value.is_null() {
        return Ok(None);
    }
    let reference = bytes(&value["content"])?;
    let representation = if let Some(encoded) = value["inline_base64"].as_str() {
        let body = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| projection_unavailable("content", "invalid inline bytes"))?;
        if body.len() > 1_048_576
            || reference.byte_length != body.len() as u64
            || reference.sha256 != format!("sha256:{}", crate::jcs::sha256_hex(&body))
        {
            return Err(projection_unavailable(
                "content",
                "inline content identity differs",
            ));
        }
        Representation::InlineBytes(body)
    } else if value["artifact"].is_object() {
        let artifact = &value["artifact"];
        let kind = pb::ArtifactKind::from_str_name(text(artifact, "kind")?)
            .filter(|k| *k != pb::ArtifactKind::Unspecified)
            .ok_or_else(|| projection_unavailable("artifact.kind", "unknown artifact kind"))?;
        let visibility = pb::ArtifactVisibility::from_str_name(text(artifact, "visibility")?)
            .filter(|k| *k != pb::ArtifactVisibility::Unspecified)
            .ok_or_else(|| projection_unavailable("artifact.visibility", "unknown visibility"))?;
        Representation::Artifact(pb::ArtifactRef {
            artifact_id: text(artifact, "artifact_id")?.into(),
            kind: kind as i32,
            visibility: visibility as i32,
            media_type: text(value, "media_type")?.into(),
            byte_length: reference.byte_length,
            sha256: reference.sha256["sha256:".len()..].into(),
        })
    } else if value["unavailable"].is_object() {
        Representation::Unavailable(pb::UnavailableContent {
            observed_byte_length: reference.byte_length,
            sha256: reference.sha256["sha256:".len()..].into(),
            reason: text(&value["unavailable"], "reason")?.into(),
        })
    } else {
        return Err(projection_unavailable(
            "content",
            "complete content representation unavailable",
        ));
    };
    let owner = if value["artifact_owner"].is_null() {
        None
    } else {
        let owner = &value["artifact_owner"];
        let workspace = &owner["workspace"];
        Some(pb::RunRef {
            workspace: Some(pb::WorkspaceRef {
                absolute_path: text(workspace, "absolute_path")?.into(),
                expected_workspace_id: text(workspace, "expected_workspace_id")?.into(),
            }),
            run_id: text(owner, "run_id")?.into(),
        })
    };
    if matches!(representation, Representation::Artifact(_)) && owner.is_none() {
        return Err(projection_unavailable(
            "artifact_owner",
            "artifact owner Run is unavailable",
        ));
    }
    Ok(Some(pb::ContentRef {
        content: Some(reference),
        media_type: text(value, "media_type")?.into(),
        representation: Some(representation),
        artifact_owner: owner,
    }))
}

pub(super) fn result_rows(value: &Value) -> Result<Vec<pb::CompleteResult>, MachineError> {
    value["results"]
        .as_array()
        .ok_or_else(|| projection_unavailable("results", "result page unavailable"))?
        .iter()
        .map(|row| {
            let identity: crate::delegation_identity::CompleteResult =
                serde_json::from_value(row["identity"].clone()).map_err(|_| {
                    projection_unavailable("result", "complete result identity unavailable")
                })?;
            let digest = identity.digest().map_err(|_| {
                projection_unavailable("result", "complete result identity is invalid")
            })?;
            if text(row, "result_digest")? != digest {
                return Err(projection_unavailable(
                    "result_digest",
                    "complete result digest differs",
                ));
            }
            let final_response = content(&row["final_response"])?;
            let structured_output = content(&row["structured_output"])?;
            for (expected, actual) in [
                (&identity.final_response, &final_response),
                (&identity.structured_output, &structured_output),
            ] {
                if expected.as_ref().map(identity_bytes)
                    != actual.as_ref().and_then(|c| c.content.clone())
                {
                    return Err(projection_unavailable(
                        "result",
                        "content reference differs from complete identity",
                    ));
                }
            }
            let change_contents = row["change_contents"]
                .as_array()
                .ok_or_else(|| {
                    projection_unavailable(
                        "change_contents",
                        "change content references unavailable",
                    )
                })?
                .iter()
                .map(|v| {
                    content(v)?.ok_or_else(|| {
                        projection_unavailable("change_contents", "missing change content")
                    })
                })
                .collect::<Result<Vec<_>, MachineError>>()?;
            let attachment_contents = row["attachment_contents"]
                .as_array()
                .ok_or_else(|| {
                    projection_unavailable(
                        "attachment_contents",
                        "attachment content references unavailable",
                    )
                })?
                .iter()
                .map(|v| {
                    content(v)?.ok_or_else(|| {
                        projection_unavailable("attachment_contents", "missing attachment content")
                    })
                })
                .collect::<Result<Vec<_>, MachineError>>()?;
            let expected_changes = identity
                .changes
                .iter()
                .flat_map(|c| [c.before.as_ref(), c.after.as_ref()])
                .flatten()
                .map(identity_bytes)
                .collect::<Vec<_>>();
            if expected_changes
                != change_contents
                    .iter()
                    .map(|c| c.content.clone().expect("checked content"))
                    .collect::<Vec<_>>()
                || identity
                    .attachments
                    .iter()
                    .map(|a| identity_bytes(&a.content))
                    .collect::<Vec<_>>()
                    != attachment_contents
                        .iter()
                        .map(|c| c.content.clone().expect("checked content"))
                        .collect::<Vec<_>>()
            {
                return Err(projection_unavailable(
                    "result",
                    "component references differ from complete identity",
                ));
            }
            let source = &identity.source;
            let changes = identity
                .changes
                .iter()
                .map(|change| {
                    let kind = match change.kind.as_str() {
                        "add" => pb::AcceptedChangeKind::Add,
                        "modify" => pb::AcceptedChangeKind::Modify,
                        "delete" => pb::AcceptedChangeKind::Delete,
                        "rename" => pb::AcceptedChangeKind::Rename,
                        _ => {
                            return Err(projection_unavailable(
                                "change.kind",
                                "unknown accepted change kind",
                            ));
                        }
                    };
                    let mode = |value: Option<u64>| {
                        value
                            .map(|v| {
                                u32::try_from(v).map_err(|_| {
                                    projection_unavailable(
                                        "mode",
                                        "accepted mode is not representable",
                                    )
                                })
                            })
                            .transpose()
                    };
                    Ok(pb::ChangeRef {
                        kind: kind as i32,
                        path: change.path.clone(),
                        old_path: change.old_path.clone(),
                        before: change.before.as_ref().map(identity_bytes),
                        after: change.after.as_ref().map(identity_bytes),
                        before_mode: mode(change.before_mode)?,
                        after_mode: mode(change.after_mode)?,
                    })
                })
                .collect::<Result<Vec<_>, MachineError>>()?;
            Ok(pb::CompleteResult {
                result_id: text(row, "result_id")?.into(),
                result_digest: digest,
                identity: Some(pb::CompleteResultIdentity {
                    schema: identity.schema.clone(),
                    source: Some(pb::ResultSource {
                        scope_kind: source.scope_kind.clone(),
                        scope_ref: source.scope_ref.clone(),
                        worker_ref: source.worker_ref.clone(),
                        task_ref: source.task_ref.clone(),
                        role_snapshot_digest: source.role_snapshot_digest.clone(),
                        agent_configuration_digest: source.agent_configuration_digest.clone(),
                        accepted_task_digest: source.accepted_task_digest.clone(),
                    }),
                    terminal_outcome: outcome(&identity.terminal_outcome)?,
                    final_response: identity.final_response.as_ref().map(identity_bytes),
                    structured_output_schema: identity.structured_output_schema.clone(),
                    structured_output: identity.structured_output.as_ref().map(identity_bytes),
                    changes,
                    attachments: identity
                        .attachments
                        .iter()
                        .map(|a| pb::ContextRef {
                            kind: a.kind.clone(),
                            identity: a.identity.clone(),
                            content: Some(identity_bytes(&a.content)),
                            media_type: a.media_type.clone(),
                        })
                        .collect(),
                    safe_failure_code: identity.safe_failure_code.clone(),
                }),
                final_response,
                structured_output,
                change_contents,
                attachment_contents,
            })
        })
        .collect()
}

#[tonic::async_trait]
impl pb::group_service_server::GroupService for GatewayTransport {
    async fn assign_group(
        &self,
        request: Request<pb::AssignGroupRequest>,
    ) -> Result<Response<pb::AssignGroupResponse>, Status> {
        execute!(self, request, "GroupService.AssignGroup", backend, {
            let admission = required(request.admission.as_ref(), "admission")?;
            if admission.trusted_invocation.is_some() {
                return Err(invalid(
                    "trusted_invocation",
                    "public group admission uses its Controller operation identity",
                ));
            }
            let manifest = required(request.assignment.as_ref(), "assignment")?;
            let inputs = request.inputs.iter().map(|input| {
                let content = required(input.content.as_ref(), "inputs.content")?;
                Ok(json!({"entry_key":input.entry_key,"absolute_file_path":input.absolute_file_path,
                    "content":{"byte_length":content.byte_length,"sha256":content.sha256}}))
            }).collect::<Result<Vec<_>, MachineError>>()?;
            let body = json!({"manifest":{"schema":manifest.schema,"scope_kind":manifest.scope_kind,
                "scope_ref":manifest.scope_ref,"entries":manifest.entries.iter().map(|e|json!({
                    "entry_key":e.entry_key,"role_snapshot_digest":e.role_snapshot_digest,
                    "agent_configuration_digest":e.agent_configuration_digest,"accepted_task_digest":e.accepted_task_digest})).collect::<Vec<_>>()},"inputs":inputs});
            let value = call(
                backend,
                required(request.scope.as_ref(), "scope")?,
                required(admission.controller.as_ref(), "controller")?,
                "assign_group",
                Some(&admission.idempotency_key),
                Some(admission.expected_revision),
                body,
            )?;
            (|| {
                bounded(pb::AssignGroupResponse {
                    context: Some(context(backend)),
                    group: Some(projection(&value)?),
                    receipt: Some(receipt(&value)?),
                })
            })()
            .map_err(|e| accepted_projection_error(e, &value))
        })
    }

    async fn await_group(
        &self,
        request: Request<pb::AwaitGroupRequest>,
    ) -> Result<Response<pb::AwaitGroupResponse>, Status> {
        execute!(self, request, "GroupService.AwaitGroup", backend, {
            let predicate = required(request.predicate.as_ref(), "predicate")?;
            let kind = match pb::WaitPredicateKind::try_from(predicate.kind).ok() {
                Some(pb::WaitPredicateKind::Any) => "any",
                Some(pb::WaitPredicateKind::All) => "all",
                Some(pb::WaitPredicateKind::K) => "k",
                _ => return Err(invalid("predicate", "unknown wait predicate")),
            };
            if (kind == "k") != predicate.k.is_some() {
                return Err(invalid(
                    "predicate.k",
                    "k is required only for the k predicate",
                ));
            }
            let mut predicate_body = json!({"kind":kind});
            if let Some(k) = predicate.k {
                predicate_body["k"] = json!(k);
            }
            let value = call(
                backend,
                required(request.scope.as_ref(), "scope")?,
                required(request.controller.as_ref(), "controller")?,
                "await_group",
                None,
                None,
                json!({"group_ref":request.group_id,"predicate":predicate_body,
                    "timeout_ms":request.timeout_ms}),
            )?;
            bounded(pb::AwaitGroupResponse {
                context: Some(context(backend)),
                group: Some(projection(&value)?),
                predicate_satisfied: value["response"]["predicate_satisfied"]
                    .as_bool()
                    .ok_or_else(|| {
                        projection_unavailable(
                            "predicate_satisfied",
                            "wait observation unavailable",
                        )
                    })?,
                timed_out: value["response"]["wait_timed_out"]
                    .as_bool()
                    .ok_or_else(|| {
                        projection_unavailable("wait_timed_out", "wait observation unavailable")
                    })?,
            })
        })
    }

    async fn collect_group(
        &self,
        request: Request<pb::CollectGroupRequest>,
    ) -> Result<Response<pb::CollectGroupResponse>, Status> {
        execute!(self, request, "GroupService.CollectGroup", backend, {
            let limit = if request.limit == 0 {
                64
            } else {
                request.limit
            };
            let value = call(
                backend,
                required(request.scope.as_ref(), "scope")?,
                required(request.controller.as_ref(), "controller")?,
                "collect_group",
                None,
                None,
                json!({"group_ref":request.group_id,"cursor":request.page_cursor,"limit":limit}),
            )?;
            bounded(pb::CollectGroupResponse {
                context: Some(context(backend)),
                group: Some(projection(&value)?),
                results: result_rows(&value)?,
                next_page_cursor: value["response"]["coverage"]["next_cursor"]
                    .as_str()
                    .map(str::to_owned),
                source_manifest: content(&value["source_manifest"])?,
            })
        })
    }

    async fn merge_group_results(
        &self,
        request: Request<pb::MergeGroupResultsRequest>,
    ) -> Result<Response<pb::MergeGroupResultsResponse>, Status> {
        execute!(self, request, "GroupService.MergeGroupResults", backend, {
            let admission = required(request.admission.as_ref(), "admission")?;
            if admission.trusted_invocation.is_some() {
                return Err(invalid(
                    "trusted_invocation",
                    "public merge uses its authenticated Controller",
                ));
            }
            let sources = request
                .sources
                .iter()
                .map(source_value)
                .collect::<Result<Vec<_>, MachineError>>()?;
            let value = call(
                backend,
                required(request.scope.as_ref(), "scope")?,
                required(admission.controller.as_ref(), "controller")?,
                "merge_group_results",
                Some(&admission.idempotency_key),
                Some(admission.expected_revision),
                json!({"group_ref":request.group_id,"sources":sources,"algorithm":request.algorithm}),
            )?;
            let identity = &value["response"]["merged_identity"];
            bounded(pb::MergeGroupResultsResponse {
                context: Some(context(backend)),
                merge_id: text(&value, "merge_id")?.into(),
                identity: Some(pb::MergedFindingsIdentity {
                    schema: text(identity, "schema")?.into(),
                    algorithm: text(identity, "algorithm")?.into(),
                    group_assignment_digest: text(identity, "group_assignment_digest")?.into(),
                    sources: identity["sources"]
                        .as_array()
                        .ok_or_else(|| projection_unavailable("sources", "missing sources"))?
                        .iter()
                        .map(source_projection)
                        .collect::<Result<Vec<_>, MachineError>>()?,
                    merged_output: Some(bytes(&identity["merged_output"])?),
                }),
                merged_findings_digest: text(&value["response"], "merged_digest")?.into(),
                merged_output: content(&value["merged_output"])?,
                receipt: Some(receipt(&value)?),
            })
            .map_err(|error| accepted_projection_error(error, &value))
        })
    }

    async fn assign_synthesis(
        &self,
        request: Request<pb::AssignSynthesisRequest>,
    ) -> Result<Response<pb::AssignSynthesisResponse>, Status> {
        execute!(self, request, "GroupService.AssignSynthesis", backend, {
            let admission = required(request.admission.as_ref(), "admission")?;
            if admission.trusted_invocation.is_some() {
                return Err(invalid(
                    "trusted_invocation",
                    "public synthesis uses its authenticated Controller",
                ));
            }
            let input = required(request.task.as_ref(), "task")?;
            let accepted = required(input.identity.as_ref(), "task.identity")?;
            let expected = accepted_task_value(accepted)?;
            let expected: crate::delegation_identity::AcceptedTask =
                serde_json::from_value(expected)
                    .map_err(|_| invalid("task.identity", "invalid accepted task"))?;
            if expected.digest()? != input.accepted_task_digest {
                return Err(invalid(
                    "accepted_task_digest",
                    "task identity digest differs",
                ));
            }
            let body = match required(input.body.as_ref(), "task.body")? {
                pb::task_input::Body::Structured(body) => {
                    json!({"schema":body.schema,"brief":body.brief,"context":body.context.iter().map(context_value).collect::<Result<Vec<_>,MachineError>>()?,"criteria":body.criteria.iter().map(|criterion|json!({"criterion_id":criterion.criterion_id,"text":criterion.text,"context_ids":criterion.context_ids})).collect::<Vec<_>>(),"constraints":body.constraints})
                }
                _ => {
                    return Err(invalid(
                        "task.body",
                        "AssignSynthesis requires its complete structured task body",
                    ));
                }
            };
            let (workspace_ref, _, _, _) = scope(required(request.scope.as_ref(), "scope")?)?;
            let view = backend.workspace(&workspace_ref)?;
            let state_root = backend.home().workspace_root(&view.workspace_id);
            let (_, primary, scope_kind, scope_ref) =
                scope(required(request.scope.as_ref(), "scope")?)?;
            let authenticated = backend.controller(&controller(required(
                admission.controller.as_ref(),
                "controller",
            )?))?;
            crate::delegation_service::research_group::public_owner_binding(
                &state_root,
                &view.workspace_id,
                &authenticated,
                scope_kind,
                scope_ref,
                primary,
            )?;
            let selected_merge = crate::delegation_service::research_merge::record(
                &crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(&state_root),
                )?,
                uuid(&request.merge_id, "merge_id")?,
            )?;
            if selected_merge["scope_kind"] != scope_kind
                || selected_merge["scope_ref"] != scope_ref.to_string()
                || selected_merge["workspace_id"] != view.workspace_id
            {
                return Err(invalid("merge_id", "merge belongs to another aggregate"));
            }
            if selected_merge["digest"] != request.merged_findings_digest {
                return Err(invalid("merged_findings_digest", "merge identity differs"));
            }
            let value = call(
                backend,
                required(request.scope.as_ref(), "scope")?,
                required(admission.controller.as_ref(), "controller")?,
                "assign_synthesis",
                Some(&admission.idempotency_key),
                Some(admission.expected_revision),
                json!({"member_ref":request.member_id,"merged_findings_digest":request.merged_findings_digest,"merged_input":selected_merge["identity"]["merged_output"],"assignment":{"task_body":body,"accepted_task_identity":expected,"deadline_seconds":input.deadline_seconds}}),
            )?;
            let retained = receipt(&value)?;
            let accepted_task: crate::delegation_identity::AcceptedTask =
                serde_json::from_value(value["accepted_task"].clone()).map_err(|_| {
                    projection_unavailable("accepted_task", "invalid retained task identity")
                })?;
            let state = text(&value["response"]["task"], "state")?;
            let disposition = match value["disposition"].as_str() {
                Some("idle") => pb::MemberDisposition::Idle,
                Some("busy") => pb::MemberDisposition::Busy,
                Some("released") => pb::MemberDisposition::Released,
                Some("unresolved") => pb::MemberDisposition::Unresolved,
                Some("passivated") => pb::MemberDisposition::Passivated,
                Some("recovery_needed") => pb::MemberDisposition::RecoveryNeeded,
                _ => pb::MemberDisposition::Unresolved,
            };
            bounded(pb::AssignSynthesisResponse {
                context: Some(context(backend)),
                task: Some(pb::TaskProjection {
                    task_id: text(&value["response"]["task"], "task_ref")?.into(),
                    member_id: text(&value["response"]["task"], "member_ref")?.into(),
                    outcome: outcome(state)?,
                    identity: Some(accepted.clone()),
                    accepted_task_digest: accepted_task.digest()?,
                    result: result_rows(&value)?.into_iter().next(),
                    recovery: None,
                }),
                receipt: Some(retained.clone()),
                identity: Some(pb::DelegationIdentity {
                    schema: "dolgorae.delegation-identity/v1".into(),
                    scope: request.scope,
                    worker_ref: request.member_id,
                    task_ref: value["response"]["task"]["task_ref"]
                        .as_str()
                        .map(str::to_owned),
                    role_body_sha256: text(&value["role_body"], "sha256")?
                        .trim_start_matches("sha256:")
                        .into(),
                    role_body_byte_length: value["role_body"]["byte_length"].as_u64().ok_or_else(
                        || projection_unavailable("role_body", "missing role body length"),
                    )?,
                    role_snapshot_digest: text(&value, "role_snapshot_digest")?.into(),
                    instruction_identity_digest: text(&value, "instruction_identity_digest")?
                        .into(),
                    agent_configuration_digest: accepted_task.agent_configuration_digest.clone(),
                    accepted_task_digest: Some(accepted_task.digest()?),
                    result_id: value["results"]
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row["result_id"].as_str())
                        .map(str::to_owned),
                    result_digest: value["results"]
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row["result_digest"].as_str())
                        .map(str::to_owned),
                    rounds: u32::try_from(value["rounds"].as_u64().ok_or_else(|| {
                        projection_unavailable("rounds", "missing accepted rounds")
                    })?)
                    .map_err(|_| projection_unavailable("rounds", "round count out of range"))?,
                    disposition: disposition as i32,
                    source_revision: value["source_revision"].as_u64().ok_or_else(|| {
                        projection_unavailable("source_revision", "missing revision")
                    })?,
                    accepted_operation: retained.operation,
                }),
            })
            .map_err(|error| accepted_projection_error(error, &value))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_projection_keeps_missing_identity_and_unknown_outcome() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let value = json!({"manifest":{"schema":"dolgorae.group-assignment/v1",
            "scope_kind":"external_engagement","scope_ref":"original-engagement","entries":[{
                "entry_key":"one","role_snapshot_digest":digest,"agent_configuration_digest":digest,
                "accepted_task_digest":digest}]},"source_revision":4,
            "response":{"coverage":{"group_ref":"original-group","group_assignment_digest":digest,"sources":[{
                "entry_key":"one","worker_ref":null,"task_ref":null,"role_snapshot_digest":digest,
                "agent_configuration_digest":digest,"accepted_task_digest":digest,"state":"unknown",
                "result_digest":null,"safe_failure_code":null}]}}});
        let projected = projection(&value).unwrap();
        assert_eq!(projected.group_id, "original-group");
        assert_eq!(projected.coverage[0].state, pb::TaskOutcome::Unknown as i32);
        assert!(projected.coverage[0].worker_ref.is_none());
        assert!(projected.coverage[0].task_ref.is_none());
        let mut missing = value;
        missing["manifest"] = Value::Null;
        assert_eq!(projection(&missing).unwrap_err().code, "INTERNAL_ERROR");
    }

    #[test]
    fn inline_content_preserves_binary_bytes_and_rejects_changed_payload() {
        use base64::Engine as _;
        let body = [0xff, 0, 1, 0xc3];
        let value = json!({"content":{"byte_length":body.len(),
            "sha256":format!("sha256:{}",crate::jcs::sha256_hex(&body))},
            "inline_base64":base64::engine::general_purpose::STANDARD.encode(body),
            "media_type":"application/octet-stream"});
        assert_eq!(
            content(&value).unwrap().unwrap().representation,
            Some(pb::content_ref::Representation::InlineBytes(body.to_vec()))
        );
        let mut changed = value;
        changed["inline_base64"] = json!("AAAAAA==");
        assert!(content(&changed).is_err());
        changed["inline_base64"] = Value::Null;
        assert!(content(&changed).is_err());
    }
}
