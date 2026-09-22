//! Bounded public observations over a captured durable Run boundary.

use crate::controller::CredentialCarrier;
use crate::domain::RunLifecycle;
use crate::event::EventProjection;
use crate::gateway::EventPage;
use crate::gateway_event;
use crate::gateway_projection;
use crate::interaction::{Interaction, InteractionStatus, MAX_RESPONSE_BYTES, UserInput};
use crate::interaction_payload::{CommandApprovalPayload, FileApprovalPayload, UnsupportedPayload};
use crate::ledger::{LedgerError, ObservedLedger};
use crate::machine::MachineError;
use crate::protocol::public_v1 as pb;
use crate::snapshot::RunSnapshot;
use crate::workspace::LosslessPath;
use serde_json::json;
use std::path::Path;
use uuid::Uuid;

fn invalid(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable observation violates its checked shape",
        false,
        json!({"invariant":invariant}),
    )
}

fn identity(value: &str, field: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|value| value.get_version_num() == 7)
        .ok_or_else(|| MachineError::invalid_argument(field, "identity must be UUIDv7"))
}

fn ledger(state_root: &Path, snapshot: &RunSnapshot) -> Result<ObservedLedger, MachineError> {
    let run_id = snapshot.manifest.run_id;
    let head = snapshot.stamp.run_state_revision;
    ObservedLedger::open_run(state_root, run_id, head)
}

fn conflict(snapshot: &RunSnapshot, operation: &str) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        "refresh authoritative Run and aggregate snapshots",
        false,
        json!({"run_id":snapshot.manifest.run_id,"state":snapshot.projection.lifecycle.as_str(),"operation":operation}),
    )
}

pub fn watch_page(
    state_root: &Path,
    snapshot: &RunSnapshot,
    after: &str,
    projection: i32,
    projection_version: u32,
) -> Result<EventPage, MachineError> {
    if projection_version != 1 {
        return Err(MachineError::new(
            "UNSUPPORTED_SCHEMA_VERSION",
            "unsupported event projection version",
            false,
            json!({"schema":"dolgorae.public.v1.RunEventEnvelope","requested":projection_version,"supported":[1]}),
        ));
    }
    let projection = match pb::ProjectionProfile::try_from(projection) {
        Ok(pb::ProjectionProfile::Minimal) => EventProjection::Minimal,
        Ok(pb::ProjectionProfile::Operational) => EventProjection::Operational,
        Ok(_) => {
            return Err(MachineError::invalid_argument(
                "projection",
                "event projection must be minimal or operational",
            ));
        }
        Err(_) => {
            return Err(MachineError::new(
                "UNSUPPORTED_SCHEMA_VERSION",
                "input enum is not supported by this public protocol",
                false,
                json!({"schema":"dolgorae.public.v1.ProjectionProfile","requested":u32::from_ne_bytes(projection.to_ne_bytes()),"supported":[0,1,2]}),
            ));
        }
    };
    let head = snapshot.stamp.run_state_revision;
    let after = if after.is_empty() { "0" } else { after };
    let parsed = crate::ledger::parse_event_cursor(after);
    if parsed.is_none_or(|value| value > head) {
        return Err(crate::ledger::event_cursor_invalid(
            snapshot.manifest.run_id,
            &after.parse::<u64>().unwrap_or(0).to_string(),
            &head.to_string(),
        ));
    }
    let records = ledger(state_root, snapshot)?;
    let events = records.stamped_events_after(parsed.expect("validated cursor"), projection.clone())
        .map_err(|error| match error {
            LedgerError::MissingEventStamp(_) => conflict(snapshot, "run.events"),
            error => MachineError::new("AUDIT_INTEGRITY_FAILURE", "durable event replay failed", false,
                json!({"run_id":snapshot.manifest.run_id,"sequence":head,"reason":error.to_string()})),
        })?;
    // A full page may have stopped at the count/byte bound. Do not cross a filtered
    // gap until the next page proves there is no selected event inside it.
    let next_cursor = events
        .last()
        .map_or_else(|| head.to_string(), |event| event.record.cursor.clone());
    let events = events
        .iter()
        .map(|event| gateway_event::envelope(event, projection.clone(), true))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EventPage {
        events,
        next_cursor,
        durable_head_cursor: head.to_string(),
        lifecycle: gateway_projection::lifecycle(snapshot.projection.lifecycle) as i32,
        terminal: matches!(
            snapshot.projection.lifecycle,
            RunLifecycle::Closed | RunLifecycle::StartFailed
        ),
    })
}

impl Interaction {
    fn summary(&self, snapshot: &RunSnapshot) -> Result<pb::InteractionSummary, MachineError> {
        let status = match self.status.as_str() {
            "pending" if self.pending_at(snapshot) => pb::InteractionStatus::Pending,
            "pending" | "stale" => pb::InteractionStatus::Stale,
            "resolved" => pb::InteractionStatus::Resolved,
            _ => return Err(invalid("interaction status")),
        };
        self.summary_with_state(
            snapshot.controller.kind,
            snapshot.stamp.interaction_state_revision,
            status,
        )
    }

    fn summary_with_state(
        &self,
        controller_kind: crate::domain::ControllerKind,
        revision: u64,
        status: pb::InteractionStatus,
    ) -> Result<pb::InteractionSummary, MachineError> {
        let safe = self.observer_summary(match status {
            pb::InteractionStatus::Pending => InteractionStatus::Pending,
            pb::InteractionStatus::Resolved => InteractionStatus::Resolved,
            pb::InteractionStatus::Stale => InteractionStatus::Stale,
            _ => return Err(invalid("interaction status")),
        })?;
        Ok(pb::InteractionSummary {
            interaction_id: self.request_id.to_string(),
            run_id: self.run_id.to_string(),
            kind: gateway_event::interaction_kind(&self.kind)? as i32,
            status: status as i32,
            safe_title: safe.safe_title.to_owned(),
            controller_kind: gateway_projection::controller_kind(controller_kind) as i32,
            requires_user_escalation: safe.requires_user_escalation,
            contains_protected_input: safe.contains_protected_input,
            created_at: Some(gateway_event::timestamp(&self.opened_at)?),
            expires_at: None,
            resolved_at: self
                .resolved_at
                .as_deref()
                .map(gateway_event::timestamp)
                .transpose()?,
            state_revision: revision,
        })
    }
}

fn interactions(
    state_root: &Path,
    snapshot: &RunSnapshot,
) -> Result<Vec<Interaction>, MachineError> {
    crate::interaction::durable_interactions_at(
        state_root,
        snapshot.manifest.run_id,
        snapshot.stamp.run_state_revision,
    )?
    .into_iter()
    .map(|(value, _)| Interaction::parse(value, snapshot.manifest.run_id))
    .collect()
}

pub fn list_pending(
    state_root: &Path,
    snapshot: &RunSnapshot,
    context: pb::ResponseContext,
) -> Result<pb::ListPendingInteractionsResponse, MachineError> {
    let items = interactions(state_root, snapshot)?
        .into_iter()
        .filter(|value| value.pending_at(snapshot))
        .map(|value| value.summary(snapshot))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(pb::ListPendingInteractionsResponse {
        context: Some(context),
        items,
        stamp: Some(gateway_projection::stamp(&snapshot.stamp)),
    })
}

fn path(value: &LosslessPath) -> Result<pb::PathProjection, MachineError> {
    gateway_projection::path(value)
}

fn interaction_payload(
    interaction: &Interaction,
) -> Result<pb::controller_interaction::Payload, MachineError> {
    crate::interaction_payload::validate(interaction)?;
    use pb::controller_interaction::Payload;
    Ok(match interaction.kind.as_str() {
        "command_execution_approval" => {
            let value: CommandApprovalPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("command approval payload"))?;
            Payload::CommandApproval(pb::CommandApprovalInteraction {
                title: value.title,
                message: value.message,
                command: value.command,
                cwd: Some(path(&value.cwd)?),
                reason: value.reason,
            })
        }
        "file_change_approval" => {
            let value: FileApprovalPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("file approval payload"))?;
            use pb::file_change_approval_interaction::Representation;
            let representation = match (value.changes, value.change_artifact) {
                (Some(changes), None) => {
                    let items = changes
                        .into_iter()
                        .map(|change| {
                            let kind = match change.kind.as_str() {
                                "add" => pb::FileChangeKind::Add,
                                "update" => pb::FileChangeKind::Update,
                                "delete" => pb::FileChangeKind::Delete,
                                _ => return Err(invalid("file change kind")),
                            };
                            Ok(pb::FileChangeProjection {
                                path: Some(path(&change.path)?),
                                kind: kind as i32,
                                unified_diff: change.diff,
                                move_path: change.move_path.as_ref().map(path).transpose()?,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Representation::InlineChanges(pb::InlineFileChanges { items })
                }
                (None, Some(artifact)) => Representation::ChangeArtifact(artifact_projection(
                    crate::artifact::change_artifact(artifact)?,
                )),
                _ => return Err(invalid("exactly one complete file-change representation")),
            };
            Payload::FileChangeApproval(pb::FileChangeApprovalInteraction {
                title: value.title,
                message: value.message,
                reason: value.reason,
                snapshot_sha256: value.snapshot_sha256,
                snapshot_revision: value.snapshot_revision,
                representation: Some(representation),
            })
        }
        "user_input" => {
            let value: UserInput = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("user input payload"))?;
            let questions = value
                .questions
                .into_iter()
                .map(|question| {
                    let options = question.options.map(|options| {
                        let items = options
                            .into_iter()
                            .map(|option| pb::InteractionOption {
                                label: option.label,
                                description: option.description,
                            })
                            .collect();
                        pb::InteractionOptions { items }
                    });
                    Ok(pb::InteractionQuestion {
                        id: question.id,
                        header: question.header,
                        question: question.question,
                        allows_other: question.is_other,
                        is_secret: question.is_secret,
                        options,
                    })
                })
                .collect::<Result<Vec<_>, MachineError>>()?;
            Payload::UserInput(pb::UserInputInteraction {
                is_blocking: value.is_blocking,
                questions,
            })
        }
        "unsupported_request" => {
            let value: UnsupportedPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("unsupported interaction payload"))?;
            let kind = match value.method.as_str() {
                "item/permissions/requestApproval" => pb::InteractionKind::PermissionRequest,
                "mcpServer/elicitation/request" => pb::InteractionKind::McpElicitation,
                _ => return Err(invalid("recognized unsupported method")),
            };
            Payload::Unsupported(pb::UnsupportedInteraction {
                original_kind: kind as i32,
                reason: pb::UnsupportedInteractionReason::RecognizedUnsupported as i32,
            })
        }
        _ => return Err(invalid("closed normalized interaction kind")),
    })
}

pub fn get_interaction(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: &str,
    carrier: &CredentialCarrier,
    context: pb::ResponseContext,
) -> Result<pb::GetControllerInteractionResponse, MachineError> {
    let id = identity(id, "interaction_id")?;
    snapshot.authorize_current_controller(state_root, carrier, "run.interaction.get")?;
    let interaction = interactions(state_root, snapshot)?
        .into_iter()
        .find(|value| value.request_id == id)
        .ok_or_else(|| {
            MachineError::interaction_not_found(
                snapshot.manifest.run_id,
                id,
                "interaction is not present in this Run",
            )
        })?;
    let payload = interaction_payload(&interaction)?;
    let mut summary = interaction.summary(snapshot)?;
    if summary.status == pb::InteractionStatus::Stale as i32 && summary.resolved_at.is_none() {
        let records = ledger(state_root, snapshot)?;
        let boundary = records
            .records()
            .iter()
            .find(|record| record.sequence() == snapshot.stamp.interaction_state_revision)
            .ok_or_else(|| conflict(snapshot, "run.interaction.get"))?;
        summary.resolved_at = Some(gateway_event::timestamp(boundary.timestamp())?);
    }
    let decisions = interaction
        .available_decisions
        .iter()
        .map(|decision| match decision.as_str() {
            "accept_once" => Ok(pb::InteractionDecision::AcceptOnce as i32),
            "decline" => Ok(pb::InteractionDecision::Decline as i32),
            "cancel" => Ok(pb::InteractionDecision::Cancel as i32),
            _ => Err(invalid("closed interaction decision")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Revalidate after reading protected material, before it can leave this helper.
    snapshot.authorize_current_controller(state_root, carrier, "run.interaction.get")?;
    Ok(pb::GetControllerInteractionResponse {
        context: Some(context),
        interaction: Some(pb::ControllerInteraction {
            summary: Some(summary),
            response_schema_id: interaction.response_schema,
            stamp: Some(gateway_projection::stamp(&snapshot.stamp)),
            payload: Some(payload),
            decisions,
        }),
    })
}

pub fn resolve_operation(
    request: &mut pb::ResolveInteractionRequest,
) -> Result<crate::semantic::RunMutationOperation, MachineError> {
    let bytes = zeroize::Zeroizing::new(std::mem::take(&mut request.response_json));
    identity(&request.interaction_id, "interaction_id")?;
    if request.idempotency_key.is_empty() || request.idempotency_key.len() > 256 {
        return Err(MachineError::invalid_argument(
            "idempotency_key",
            "idempotency key must contain 1 to 256 UTF-8 bytes",
        ));
    }
    if bytes.len() > MAX_RESPONSE_BYTES {
        let run_id = request
            .run
            .as_ref()
            .ok_or_else(|| MachineError::invalid_argument("run", "Run reference is required"))?
            .run_id
            .clone();
        return Err(crate::interaction::response_too_large(
            run_id,
            &request.interaction_id,
            bytes.len(),
            "interaction response exceeds 1 MiB",
        ));
    }
    let response = crate::semantic::decode_response(&bytes, "response_json")?;
    Ok(crate::semantic::RunMutationOperation::Respond {
        request_id: request.interaction_id.clone(),
        idempotency_key: request.idempotency_key.clone(),
        response,
    })
}

fn artifact_projection(value: crate::artifact::ArtifactReference) -> pb::ArtifactRef {
    pb::ArtifactRef {
        artifact_id: value.artifact_id,
        kind: match value.kind {
            crate::artifact::ArtifactKind::FinalResponse => pb::ArtifactKind::FinalResponse,
            crate::artifact::ArtifactKind::FileChangeDiff => pb::ArtifactKind::FileChangeDiff,
            crate::artifact::ArtifactKind::UserInput => pb::ArtifactKind::UserInput,
        } as i32,
        visibility: match value.visibility {
            crate::artifact::ArtifactVisibility::Observer => pb::ArtifactVisibility::Observer,
            crate::artifact::ArtifactVisibility::ControllerOnly => {
                pb::ArtifactVisibility::ControllerOnly
            }
        } as i32,
        media_type: value.media_type,
        byte_length: value.byte_length,
        sha256: value.sha256,
    }
}
pub fn artifact_metadata(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: &str,
    carrier: Option<&CredentialCarrier>,
    context: pb::ResponseContext,
) -> Result<pb::GetArtifactResponse, MachineError> {
    let value = crate::artifact::metadata(state_root, snapshot, id, carrier)?;
    Ok(pb::GetArtifactResponse {
        context: Some(context),
        artifact: Some(artifact_projection(value.artifact)),
        maximum_chunk_size: value.maximum_chunk_size,
    })
}
pub fn artifact_chunk(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: &str,
    range: (u64, u32),
    carrier: Option<&CredentialCarrier>,
    context: pb::ResponseContext,
) -> Result<pb::ReadArtifactChunkResponse, MachineError> {
    let value = crate::artifact::chunk(state_root, snapshot, id, range, carrier)?;
    Ok(pb::ReadArtifactChunkResponse {
        context: Some(context),
        artifact_id: value.artifact_id,
        offset: value.offset,
        length: value.length,
        data: value.data,
        eof: value.eof,
        total_byte_length: value.total_byte_length,
        sha256: value.sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message as _;
    use serde_json::Value;

    fn protected_interaction() -> Interaction {
        serde_json::from_value(json!({"schema_version":1,"request_id":Uuid::now_v7(),"run_id":Uuid::now_v7(),"controller_id":Uuid::now_v7(),
            "control_mode":"direct_interactive","thread_id":"thread","turn_id":"turn","item_id":null,"run_generation":1,"server_epoch":1,
            "kind":"user_input","status":"pending","payload":{"is_blocking":true,"questions":[{"id":"PRIVATE-ID","header":"PRIVATE-HEADER","question":"PRIVATE-QUESTION",
                "is_other":true,"is_secret":true,"options":[{"label":"PRIVATE-LABEL","description":"PRIVATE-DESCRIPTION"}]}]},
            "available_decisions":[],"response_schema":"dolgorae.interaction.user-input/v1","opened_at":"2026-09-09T00:00:00.123456Z","resolved_at":null,"resolution":null})).unwrap()
    }

    #[test]
    fn observer_summary_carries_only_safe_titles_while_controller_payload_remains_typed() {
        let interaction = protected_interaction();
        let summary = interaction
            .summary_with_state(
                crate::domain::ControllerKind::InteractiveClient,
                19,
                pb::InteractionStatus::Pending,
            )
            .unwrap();
        assert_eq!(summary.safe_title, "Protected input requested");
        assert!(summary.contains_protected_input && summary.requires_user_escalation);
        assert_eq!(summary.state_revision, 19);
        assert!(
            !summary
                .encode_to_vec()
                .windows(b"PRIVATE".len())
                .any(|bytes| bytes == b"PRIVATE")
        );
        let pb::controller_interaction::Payload::UserInput(payload) =
            interaction_payload(&interaction).unwrap()
        else {
            panic!("expected typed questions")
        };
        assert_eq!(payload.questions[0].question, "PRIVATE-QUESTION");
        assert!(payload.questions[0].is_secret && payload.questions[0].allows_other);
        assert_eq!(
            payload.questions[0].options.as_ref().unwrap().items[0].label,
            "PRIVATE-LABEL"
        );
        for status in [
            pb::InteractionStatus::Resolved,
            pb::InteractionStatus::Stale,
        ] {
            let summary = interaction
                .summary_with_state(crate::domain::ControllerKind::InteractiveClient, 20, status)
                .unwrap();
            assert!(summary.contains_protected_input);
            assert!(!summary.requires_user_escalation);
            assert!(
                !summary
                    .encode_to_vec()
                    .windows(b"PRIVATE".len())
                    .any(|bytes| bytes == b"PRIVATE")
            );
        }
        let mut unexpected = protected_interaction();
        unexpected.payload["answers"] = json!({"secret":"NEVER-ACCEPT-PERSISTED-ANSWER"});
        assert!(interaction_payload(&unexpected).is_err());
        assert!(
            unexpected
                .observer_summary(InteractionStatus::Pending)
                .is_err()
        );
    }

    #[test]
    fn controller_payloads_preserve_closed_approval_and_unsupported_variants() {
        let mut interaction = protected_interaction();
        interaction.kind = "command_execution_approval".into();
        interaction.response_schema = "dolgorae.interaction.command-approval/v1".into();
        interaction.available_decisions =
            vec!["accept_once".into(), "decline".into(), "cancel".into()];
        interaction.payload = json!({"title":"approve","message":"command","command":["ls"],"cwd":{"$dolgorae_path_bytes":"/w=="},"reason":null});
        let pb::controller_interaction::Payload::CommandApproval(command) =
            interaction_payload(&interaction).unwrap()
        else {
            panic!("expected command")
        };
        assert_eq!(command.command, ["ls"]);
        assert!(
            matches!(command.cwd.unwrap().value,Some(pb::path_projection::Value::OpaquePath(bytes)) if bytes==[255])
        );
        interaction.kind = "file_change_approval".into();
        interaction.response_schema = "dolgorae.interaction.file-change-approval/v1".into();
        interaction.payload = json!({"title":"approve","message":"change","reason":null,"snapshot_sha256":"a".repeat(64),"snapshot_revision":8,"truncated":false,
            "changes":[{"path":"a","kind":"update","diff":"+line","move_path":"b"}],"change_artifact":null});
        let pb::controller_interaction::Payload::FileChangeApproval(file) =
            interaction_payload(&interaction).unwrap()
        else {
            panic!("expected file")
        };
        assert_eq!(file.snapshot_revision, 8);
        assert!(
            matches!(file.representation,Some(pb::file_change_approval_interaction::Representation::InlineChanges(changes)) if changes.items[0].move_path.is_some())
        );
        interaction.payload["snapshot_revision"] = json!(9_007_199_254_740_992_u64);
        let error = interaction_payload(&interaction).unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(error.details["invariant"], "file snapshot completeness");
        interaction.payload["snapshot_revision"] = json!(8);
        interaction.payload["truncated"] = json!(true);
        let error = interaction_payload(&interaction).unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(error.details["invariant"], "file snapshot completeness");
        interaction.payload["truncated"] = json!(false);
        interaction.payload["change_artifact"] = json!({"artifact_id":Uuid::now_v7(),"sha256":"b".repeat(64),"media_type":"text/x-diff","byte_length":65537,"truncated":false});
        assert!(interaction_payload(&interaction).is_err());
        interaction.payload["changes"] = Value::Null;
        let pb::controller_interaction::Payload::FileChangeApproval(file) =
            interaction_payload(&interaction).unwrap()
        else {
            panic!("expected file artifact")
        };
        assert!(
            matches!(file.representation,Some(pb::file_change_approval_interaction::Representation::ChangeArtifact(reference)) if reference.visibility==pb::ArtifactVisibility::ControllerOnly as i32)
        );
        interaction.kind = "unsupported_request".into();
        interaction.status = "resolved".into();
        interaction.available_decisions.clear();
        interaction.response_schema = "dolgorae.interaction.unsupported/v1".into();
        interaction.payload =
            json!({"method":"mcpServer/elicitation/request","reason":"recognized_unsupported"});
        let pb::controller_interaction::Payload::Unsupported(value) =
            interaction_payload(&interaction).unwrap()
        else {
            panic!("expected unsupported")
        };
        assert_eq!(
            value.original_kind,
            pb::InteractionKind::McpElicitation as i32
        );
        interaction.payload["method"] = json!("unknown/provider/method");
        assert!(interaction_payload(&interaction).is_err());

        let mut oversized = protected_interaction();
        oversized.payload["questions"][0]["question"] =
            json!("x".repeat(crate::interaction::MAX_PAYLOAD_BYTES));
        let error = interaction_payload(&oversized).unwrap_err();
        assert_eq!(error.code, "INTERACTION_PAYLOAD_TOO_LARGE");
        assert!(
            error.details["observed_bytes"].as_u64().unwrap()
                > crate::interaction::MAX_PAYLOAD_BYTES as u64
        );
    }

    #[test]
    fn resolve_body_is_bounded_duplicate_safe_and_removed_from_the_request_buffer() {
        let mut request = pb::ResolveInteractionRequest {
            interaction_id: Uuid::now_v7().to_string(),
            idempotency_key: "key".into(),
            response_json: br#"{"answers":{"q":["protected"]}}"#.to_vec(),
            ..Default::default()
        };
        let operation = resolve_operation(&mut request).unwrap();
        assert!(request.response_json.is_empty());
        let crate::semantic::RunMutationOperation::Respond { mut response, .. } = operation else {
            panic!("expected response")
        };
        assert_eq!(response["answers"]["q"][0], "protected");
        crate::turn::zeroize_protected_json(&mut response);
        request.response_json = br#"{"decision":"accept_once","decision":"decline"}"#.to_vec();
        assert_eq!(
            resolve_operation(&mut request).err().unwrap().code,
            "INVALID_ARGUMENT"
        );
        assert!(request.response_json.is_empty());
        request.run = Some(pb::RunRef {
            run_id: Uuid::now_v7().to_string(),
            ..Default::default()
        });
        request.response_json = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        assert_eq!(
            resolve_operation(&mut request).err().unwrap().code,
            "INTERACTION_RESPONSE_TOO_LARGE"
        );
        assert!(request.response_json.is_empty());
    }
}
