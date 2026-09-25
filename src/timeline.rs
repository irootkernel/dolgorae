//! Shared Controller-authorized timeline projection for Machine CLI and gRPC.

use crate::controller::CredentialCarrier;
use crate::event::{ClientEventData, FinalResponse};
use crate::gateway_event;
use crate::gateway_projection;
use crate::interaction::{Interaction, InteractionStatus};
use crate::ledger::{AuditRecord, ObservedLedger};
use crate::machine::MachineError;
use crate::protocol::public_v1 as pb;
use crate::snapshot::RunSnapshot;
use serde_json::json;
use std::collections::HashMap;
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

pub fn list(
    state_root: &Path,
    snapshot: &RunSnapshot,
    carrier: &CredentialCarrier,
    after: &str,
    limit: u32,
    timeline_version: u32,
    context: pb::ResponseContext,
) -> Result<pb::ListRunTimelineItemsResponse, MachineError> {
    if timeline_version != 1 {
        return Err(MachineError::new(
            "UNSUPPORTED_SCHEMA_VERSION",
            "unsupported Controller timeline version",
            false,
            json!({"schema":"dolgorae.public.v1.TimelineItem","requested":timeline_version,"supported":[1]}),
        ));
    }
    let limit = if limit == 0 { 100 } else { limit };
    if limit > 500 {
        return Err(MachineError::invalid_argument(
            "limit",
            "timeline limit must be at most 500",
        ));
    }
    let head = snapshot.stamp.run_state_revision;
    let after = if after.is_empty() { "0" } else { after };
    let parsed = crate::ledger::parse_event_cursor(after);
    if parsed.is_none_or(|value| value > head) {
        let requested = after.chars().take(256).collect::<String>();
        return Err(crate::ledger::event_cursor_invalid(
            snapshot.manifest.run_id,
            &requested,
            &head.to_string(),
        ));
    }
    snapshot.authorize_current_controller(state_root, carrier, "run.timeline.list")?;
    let records = ObservedLedger::open_run(state_root, snapshot.manifest.run_id, head)?;
    let interactions =
        crate::interaction::durable_interactions_at(state_root, snapshot.manifest.run_id, head)?
            .into_iter()
            .map(|(value, _)| Interaction::parse(value, snapshot.manifest.run_id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|interaction| (interaction.request_id.to_string(), interaction))
            .collect::<HashMap<_, _>>();
    let mut intents = HashMap::new();
    let mut items = Vec::new();
    for record in records.records() {
        if record.kind().as_str() == "idempotency_reserved" {
            let value = record
                .payload_value()
                .map_err(|_| invalid("accepted input intent payload"))?;
            if value.get("operation_id").is_some() {
                let intent: crate::turn::OperationIntent = serde_json::from_value(value)
                    .map_err(|_| invalid("accepted input intent payload"))?;
                if let Some(input) = intent.accepted_input {
                    intents.insert(intent.operation_id, input);
                }
            }
        }
        if record.sequence() <= parsed.expect("validated timeline cursor") {
            continue;
        }
        if let Some(item) = item(state_root, snapshot, record, &intents, &interactions)? {
            items.push(item);
        }
    }
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_after_cursor = more.then(|| {
        items
            .last()
            .expect("a page with a successor has an item")
            .cursor
            .clone()
    });
    snapshot.authorize_current_controller(state_root, carrier, "run.timeline.list")?;
    Ok(pb::ListRunTimelineItemsResponse {
        context: Some(context),
        items,
        captured_head_cursor: head.to_string(),
        next_after_cursor,
        stamp: Some(gateway_projection::stamp(&snapshot.stamp)),
    })
}

fn item(
    state_root: &Path,
    snapshot: &RunSnapshot,
    record: &AuditRecord,
    intents: &HashMap<Uuid, crate::turn::AcceptedUserInput>,
    interactions: &HashMap<String, Interaction>,
) -> Result<Option<pb::TimelineItem>, MachineError> {
    use pb::timeline_item::Content;
    let base = |item_type: pb::TimelineItemType,
                turn_id: String|
     -> Result<pb::TimelineItem, MachineError> {
        Ok(pb::TimelineItem {
            r#type: item_type as i32,
            cursor: record.sequence().to_string(),
            run_id: snapshot.manifest.run_id.to_string(),
            turn_id,
            occurred_at: Some(gateway_event::timestamp(record.timestamp())?),
            provider_item_id: None,
            provider_order: None,
            content: None,
            images: Vec::new(),
            interaction_id: None,
            status: None,
            interaction_kind: None,
            interaction_status: None,
            interaction_safe_title: None,
        })
    };
    if record.kind().as_str() == "turn_started" {
        let value = payload(record, "accepted input turn record")?;
        let operation_id = value["operation_id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| invalid("accepted input operation identity"))?;
        let Some(input) = intents.get(&operation_id) else {
            return Ok(None);
        };
        let turn_id = required_string(&value, "turn_id", "accepted input turn identity")?;
        let mut item = base(pb::TimelineItemType::UserInputAccepted, turn_id)?;
        item.status = Some(pb::TimelineItemStatus::Accepted as i32);
        item.content = Some(match &input.content {
            crate::turn::AcceptedInputContent::Inline { storage } => {
                Content::InlineText(crate::artifact::accepted_input_text(
                    state_root,
                    snapshot.manifest.run_id,
                    storage.artifact_id,
                    &storage.created_at,
                    storage.byte_length,
                    &storage.sha256,
                )?)
            }
            crate::turn::AcceptedInputContent::Artifact { storage } => {
                Content::Artifact(artifact(crate::artifact::ArtifactReference {
                    artifact_id: storage.artifact_id.to_string(),
                    created_at: storage.created_at.clone(),
                    interaction_request_id: None,
                    kind: crate::artifact::ArtifactKind::UserInput,
                    visibility: crate::artifact::ArtifactVisibility::ControllerOnly,
                    media_type: "text/plain; charset=utf-8".to_owned(),
                    byte_length: storage.byte_length,
                    sha256: storage.sha256.clone(),
                }))
            }
        });
        item.images = input
            .images
            .iter()
            .map(|image| pb::ImageInputMetadata {
                ordinal: image.ordinal,
                detail: match image.detail {
                    crate::turn::ImageDetail::Auto => pb::ImageDetail::Auto,
                    crate::turn::ImageDetail::Low => pb::ImageDetail::Low,
                    crate::turn::ImageDetail::High => pb::ImageDetail::High,
                } as i32,
                media_type: image.media_type.clone(),
                byte_length: image.byte_length,
                sha256: image.sha256.clone(),
            })
            .collect();
        return Ok(Some(item));
    }
    let Some(event) = record.client_projection().map(|value| &value.record) else {
        return Ok(None);
    };
    match &event.data {
        ClientEventData::ResponseFinal(response) => {
            let mut item = base(
                pb::TimelineItemType::AssistantResponseFinal,
                event
                    .turn_id
                    .clone()
                    .ok_or_else(|| invalid("final response turn identity"))?,
            )?;
            item.status = Some(pb::TimelineItemStatus::Final as i32);
            item.content = Some(match &response.response {
                FinalResponse::Inline { text } => Content::InlineText(text.clone()),
                FinalResponse::Artifact { artifact: value } => {
                    Content::Artifact(artifact(crate::artifact::ArtifactReference {
                        artifact_id: value.artifact_id.to_string(),
                        created_at: value.created_at.clone(),
                        interaction_request_id: None,
                        kind: crate::artifact::ArtifactKind::FinalResponse,
                        visibility: crate::artifact::ArtifactVisibility::Observer,
                        media_type: value.media_type.clone(),
                        byte_length: value.byte_length,
                        sha256: value.sha256.clone(),
                    }))
                }
            });
            Ok(Some(item))
        }
        ClientEventData::InteractionOpened(value) => {
            interaction_item(record, snapshot, interactions, &value.request_id, true)
        }
        ClientEventData::InteractionResolved(value) => {
            interaction_item(record, snapshot, interactions, &value.request_id, false)
        }
        ClientEventData::TurnStateChanged(value) if record.kind().as_str() == "turn_terminal" => {
            let mut item = base(
                pb::TimelineItemType::TurnTerminal,
                event
                    .turn_id
                    .clone()
                    .ok_or_else(|| invalid("terminal turn identity"))?,
            )?;
            item.status = Some(match value.current.as_str() {
                "completed" => pb::TimelineItemStatus::Completed,
                "failed" => pb::TimelineItemStatus::Failed,
                "interrupted" => pb::TimelineItemStatus::Interrupted,
                "outcome_unknown" => pb::TimelineItemStatus::OutcomeUnknown,
                _ => return Err(invalid("terminal timeline status")),
            } as i32);
            Ok(Some(item))
        }
        _ => Ok(None),
    }
}

fn interaction_item(
    record: &AuditRecord,
    snapshot: &RunSnapshot,
    interactions: &HashMap<String, Interaction>,
    request_id: &str,
    opened: bool,
) -> Result<Option<pb::TimelineItem>, MachineError> {
    let interaction = interactions
        .get(request_id)
        .ok_or_else(|| invalid("timeline interaction identity"))?;
    let safe = interaction.observer_summary(if opened {
        InteractionStatus::Pending
    } else if interaction.status == "stale" {
        InteractionStatus::Stale
    } else {
        InteractionStatus::Resolved
    })?;
    Ok(Some(pb::TimelineItem {
        r#type: if opened {
            pb::TimelineItemType::InteractionOpened
        } else {
            pb::TimelineItemType::InteractionResolved
        } as i32,
        cursor: record.sequence().to_string(),
        run_id: snapshot.manifest.run_id.to_string(),
        turn_id: interaction.turn_id.clone(),
        occurred_at: Some(gateway_event::timestamp(record.timestamp())?),
        provider_item_id: interaction.item_id.clone(),
        provider_order: None,
        content: None,
        images: Vec::new(),
        interaction_id: Some(request_id.to_owned()),
        status: Some(if opened {
            pb::TimelineItemStatus::Opened
        } else {
            pb::TimelineItemStatus::Resolved
        } as i32),
        interaction_kind: Some(gateway_event::interaction_kind(&interaction.kind)? as i32),
        interaction_status: Some(if opened {
            pb::InteractionStatus::Pending
        } else if interaction.status == "stale" {
            pb::InteractionStatus::Stale
        } else {
            pb::InteractionStatus::Resolved
        } as i32),
        interaction_safe_title: Some(safe.safe_title.to_owned()),
    }))
}

fn artifact(value: crate::artifact::ArtifactReference) -> pb::ArtifactRef {
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

fn payload(record: &AuditRecord, invariant: &str) -> Result<serde_json::Value, MachineError> {
    record.payload_value().map_err(|_| invalid(invariant))
}

fn required_string(
    value: &serde_json::Value,
    field: &str,
    invariant: &str,
) -> Result<String, MachineError> {
    value[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(invariant))
}
