//! Pure conversion of immutable client events into the checked public stream.

use crate::event::{ClientEventData, EventProjection, FinalResponse, StampedClientEvent};
use crate::gateway_projection;
use crate::machine::MachineError;
use crate::protocol::public_v1 as pb;

fn invalid(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable event cannot be represented by the checked public protocol",
        false,
        serde_json::json!({"invariant": invariant}),
    )
}

/// Convert the audit timestamp without discarding microseconds or accepting normalization.
pub fn timestamp(value: &str) -> Result<prost_types::Timestamp, MachineError> {
    if !crate::audit::is_microsecond_utc_timestamp(value) {
        return Err(invalid("canonical event timestamp"));
    }
    let parsed: prost_types::Timestamp = value
        .parse()
        .map_err(|_| invalid("representable event timestamp"))?;
    if !(-62_135_596_800..=253_402_300_799).contains(&parsed.seconds)
        || !(0..1_000_000_000).contains(&parsed.nanos)
    {
        return Err(invalid("protobuf timestamp range"));
    }
    Ok(parsed)
}

pub fn turn_status(value: &str) -> Result<pb::TurnStatus, MachineError> {
    Ok(match value {
        "reserved" => pb::TurnStatus::Reserved,
        "accepted" => pb::TurnStatus::Accepted,
        "running" => pb::TurnStatus::Running,
        "waiting_interaction" => pb::TurnStatus::WaitingInteraction,
        "interrupting" => pb::TurnStatus::Interrupting,
        "completed" => pb::TurnStatus::Completed,
        "failed" => pb::TurnStatus::Failed,
        "interrupted" => pb::TurnStatus::Interrupted,
        "outcome_unknown" => pb::TurnStatus::OutcomeUnknown,
        _ => return Err(invalid("closed Turn event state")),
    })
}

pub fn interaction_kind(value: &str) -> Result<pb::InteractionKind, MachineError> {
    Ok(match value {
        "command_execution_approval" => pb::InteractionKind::CommandExecutionApproval,
        "file_change_approval" => pb::InteractionKind::FileChangeApproval,
        "permission_request" => pb::InteractionKind::PermissionRequest,
        "user_input" => pb::InteractionKind::UserInput,
        "mcp_elicitation" => pb::InteractionKind::McpElicitation,
        "connector_approval" => pb::InteractionKind::ConnectorApproval,
        "unsupported_request" => pb::InteractionKind::UnsupportedRequest,
        _ => return Err(invalid("closed interaction kind")),
    })
}

pub fn interaction_outcome(value: &str) -> Result<pb::InteractionOutcome, MachineError> {
    Ok(match value {
        "accepted" => pb::InteractionOutcome::Accepted,
        "declined" => pb::InteractionOutcome::Declined,
        "cancelled" => pb::InteractionOutcome::Cancelled,
        "answered" => pb::InteractionOutcome::Answered,
        "stale" => pb::InteractionOutcome::Stale,
        "method_not_found" => pb::InteractionOutcome::MethodNotFound,
        _ => return Err(invalid("closed interaction outcome")),
    })
}

fn run_lifecycle(value: &str) -> Result<i32, MachineError> {
    let value = serde_json::from_value::<crate::domain::RunLifecycle>(serde_json::Value::String(
        value.to_owned(),
    ))
    .map_err(|_| invalid("closed Run event state"))?;
    Ok(gateway_projection::lifecycle(value) as i32)
}

fn writer_authority(value: &str) -> Result<i32, MachineError> {
    let value = serde_json::from_value::<crate::writer::WriterAuthorityState>(
        serde_json::Value::String(value.to_owned()),
    )
    .map_err(|_| invalid("closed Writer event state"))?;
    Ok(gateway_projection::writer_authority(value) as i32)
}

fn final_response(value: &FinalResponse) -> pb::FinalResponse {
    let value = match value {
        FinalResponse::Inline { text } => pb::final_response::Value::InlineUtf8(text.clone()),
        FinalResponse::Artifact { artifact } => {
            pb::final_response::Value::Artifact(pb::ArtifactRef {
                artifact_id: artifact.artifact_id.to_string(),
                kind: pb::ArtifactKind::FinalResponse as i32,
                visibility: pb::ArtifactVisibility::Observer as i32,
                media_type: artifact.media_type.clone(),
                byte_length: artifact.byte_length,
                sha256: artifact.sha256.clone(),
            })
        }
    };
    pb::FinalResponse { value: Some(value) }
}

/// Project one already selected event. Replay status comes from the stream owner.
pub fn envelope(
    value: &StampedClientEvent,
    projection: EventProjection,
    replay: bool,
) -> Result<pb::RunEventEnvelope, MachineError> {
    let record = &value.record;
    let sequence = record
        .cursor
        .parse::<u64>()
        .map_err(|_| invalid("event cursor"))?;
    value
        .validate(record.run_id, sequence, &record.timestamp)
        .map_err(|_| invalid("validated event and historical boundary"))?;
    if projection == EventProjection::Minimal && !record.data.minimal() {
        return Err(invalid("selected event belongs to requested projection"));
    }
    use pb::durable_run_event::Event;
    let event = match &record.data {
        ClientEventData::RunStateChanged(payload) => Event::RunStateChanged(pb::RunStateChanged {
            previous: payload.previous.as_deref().map(run_lifecycle).transpose()?,
            current: run_lifecycle(&payload.current)?,
        }),
        ClientEventData::TurnStateChanged(payload) => {
            Event::TurnStateChanged(pb::TurnStateChanged {
                previous: payload
                    .previous
                    .as_deref()
                    .map(turn_status)
                    .transpose()?
                    .map(|value| value as i32),
                current: turn_status(&payload.current)? as i32,
            })
        }
        ClientEventData::ResponseFinal(payload) => {
            Event::FinalResponseAvailable(pb::FinalResponseAvailable {
                response: Some(final_response(&payload.response)),
            })
        }
        ClientEventData::InteractionOpened(payload) => {
            Event::InteractionOpened(pb::InteractionOpenedEvent {
                interaction_id: payload.request_id.clone(),
                kind: interaction_kind(&payload.interaction_kind)? as i32,
            })
        }
        ClientEventData::InteractionResolved(payload) => {
            Event::InteractionResolved(pb::InteractionResolvedEvent {
                interaction_id: payload.request_id.clone(),
                outcome: interaction_outcome(&payload.outcome)? as i32,
            })
        }
        ClientEventData::RuntimeError(payload) => {
            Event::RuntimeErrorOccurred(pb::RuntimeErrorOccurred {
                error_code: payload.error_code.clone(),
                safe_message: payload.message.clone(),
            })
        }
        ClientEventData::UsageReported(payload) => Event::UsageReported(pb::UsageReported {
            input_tokens: payload.input_tokens,
            output_tokens: payload.output_tokens,
        }),
        ClientEventData::WorkspaceChanges(payload) => {
            Event::WorkspaceChanges(pb::WorkspaceChanges {
                paths: payload
                    .paths
                    .iter()
                    .map(gateway_projection::path)
                    .collect::<Result<_, _>>()?,
                truncated: payload.truncated,
            })
        }
        ClientEventData::WriterStateChanged(payload) => {
            Event::WriterStateChanged(pb::WriterStateChangedEvent {
                previous: writer_authority(&payload.previous)?,
                current: writer_authority(&payload.current)?,
                writer_run_id: payload.writer_run_id.map(|id| id.to_string()),
                writer_generation: payload.writer_generation,
            })
        }
        ClientEventData::RecoveryRequired(payload) => {
            Event::RecoveryRequired(pb::RecoveryRequiredEvent {
                safe_reason: payload.reason.clone(),
            })
        }
        ClientEventData::CommandStarted(payload) => Event::CommandStarted(pb::CommandStarted {
            safe_command: payload.command.clone(),
        }),
        ClientEventData::CommandCompleted(payload) => {
            Event::CommandCompleted(pb::CommandCompleted {
                safe_command: payload.command.clone(),
                exit_status: payload
                    .exit_status
                    .map(i32::try_from)
                    .transpose()
                    .map_err(|_| invalid("command exit status int32 range"))?,
            })
        }
        ClientEventData::DiagnosticReported(payload) => {
            Event::DiagnosticReported(pb::DiagnosticReported {
                safe_message: payload.message.clone(),
            })
        }
        ClientEventData::GenerationChanged(payload) => {
            Event::GenerationChanged(pb::GenerationChanged {
                run_generation: payload.run_generation,
                server_epoch: payload.server_epoch,
            })
        }
        ClientEventData::ReasoningSuppressed(payload) => {
            Event::ReasoningSuppressed(pb::ReasoningSuppressed {
                method: payload.method.clone(),
                byte_length: payload.byte_length,
                sha256: payload.sha256.clone(),
                safe_reason: payload.reason.clone(),
            })
        }
    };
    Ok(pb::RunEventEnvelope {
        item: Some(pb::run_event_envelope::Item::DurableEvent(
            pb::DurableRunEvent {
                cursor: record.cursor.clone(),
                event_id: record.event_id.to_string(),
                occurred_at: Some(timestamp(&record.timestamp)?),
                workspace_id: record.workspace_id.clone(),
                run_id: record.run_id.to_string(),
                thread_id: record.thread_id.clone(),
                turn_id: record.turn_id.clone(),
                server_key: record.server_key.clone(),
                server_epoch: record.server_epoch,
                replay,
                projection: match projection {
                    EventProjection::Minimal => pb::ProjectionProfile::Minimal,
                    EventProjection::Operational => pb::ProjectionProfile::Operational,
                } as i32,
                projection_version: record.event_schema_version,
                stamp: Some(gateway_projection::stamp(&value.stamp)),
                event: Some(event),
            },
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProjectionStamp;
    use crate::event::*;
    use prost::Message;
    use uuid::Uuid;

    fn stamped(data: ClientEventData) -> StampedClientEvent {
        StampedClientEvent {
            record: ClientEventRecord {
                schema_version: 1,
                event_schema_version: 1,
                cursor: "19".to_owned(),
                event_id: Uuid::now_v7(),
                timestamp: "2026-09-09T12:34:56.123456Z".to_owned(),
                workspace_id: "a".repeat(64),
                run_id: Uuid::now_v7(),
                thread_id: Some("thread".to_owned()),
                turn_id: Some("turn".to_owned()),
                server_key: "b".repeat(64),
                server_epoch: 7,
                data,
            },
            stamp: ProjectionStamp {
                captured_head_cursor: "19".to_owned(),
                run_state_revision: 19,
                writer_state_revision: 12,
                interaction_state_revision: 17,
            },
        }
    }

    #[test]
    fn timestamp_preserves_microseconds_across_epoch_and_calendar_boundaries() {
        for (source, seconds, nanos) in [
            ("1970-01-01T00:00:00.000000Z", 0, 0),
            ("1969-12-31T23:59:59.999999Z", -1, 999_999_000),
            ("2000-02-29T00:00:00.123456Z", 951_782_400, 123_456_000),
            ("0001-01-01T00:00:00.000000Z", -62_135_596_800, 0),
            ("9999-12-31T23:59:59.999999Z", 253_402_300_799, 999_999_000),
        ] {
            assert_eq!(
                timestamp(source).unwrap(),
                prost_types::Timestamp { seconds, nanos }
            );
        }
        for source in [
            "0000-01-01T00:00:00.000000Z",
            "1900-02-29T00:00:00.000000Z",
            "2000-02-30T00:00:00.000000Z",
            "2000-02-29T24:00:00.000000Z",
            "2000-02-29T00:00:60.000000Z",
            "2000-02-29T00:00:00Z",
            "2000-02-29T00:00:00.1234567Z",
            "2000-02-29T00:00:00.123456+00:00",
        ] {
            assert!(timestamp(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn every_checked_event_variant_round_trips_with_exact_metadata_and_payload() {
        use pb::durable_run_event::Event;
        let cases = vec![
            (
                ClientEventData::RunStateChanged(RunStateEventPayload {
                    previous: Some("idle".into()),
                    current: "running".into(),
                }),
                Event::RunStateChanged(pb::RunStateChanged {
                    previous: Some(pb::RunLifecycle::Idle as i32),
                    current: pb::RunLifecycle::Running as i32,
                }),
            ),
            (
                ClientEventData::TurnStateChanged(TurnStateEventPayload {
                    previous: None,
                    current: "completed".into(),
                }),
                Event::TurnStateChanged(pb::TurnStateChanged {
                    previous: None,
                    current: pb::TurnStatus::Completed as i32,
                }),
            ),
            (
                ClientEventData::ResponseFinal(ResponseEventPayload {
                    response: FinalResponse::Inline {
                        text: "done".into(),
                    },
                }),
                Event::FinalResponseAvailable(pb::FinalResponseAvailable {
                    response: Some(pb::FinalResponse {
                        value: Some(pb::final_response::Value::InlineUtf8("done".into())),
                    }),
                }),
            ),
            (
                ClientEventData::InteractionOpened(InteractionOpenedPayload {
                    request_id: "42".into(),
                    interaction_kind: "user_input".into(),
                }),
                Event::InteractionOpened(pb::InteractionOpenedEvent {
                    interaction_id: "42".into(),
                    kind: pb::InteractionKind::UserInput as i32,
                }),
            ),
            (
                ClientEventData::InteractionResolved(InteractionResolvedPayload {
                    request_id: "42".into(),
                    outcome: "answered".into(),
                }),
                Event::InteractionResolved(pb::InteractionResolvedEvent {
                    interaction_id: "42".into(),
                    outcome: pb::InteractionOutcome::Answered as i32,
                }),
            ),
            (
                ClientEventData::RuntimeError(RuntimeErrorPayload {
                    error_code: "TRANSPORT_FAILURE".into(),
                    message: "ended".into(),
                }),
                Event::RuntimeErrorOccurred(pb::RuntimeErrorOccurred {
                    error_code: "TRANSPORT_FAILURE".into(),
                    safe_message: "ended".into(),
                }),
            ),
            (
                ClientEventData::UsageReported(UsagePayload {
                    input_tokens: 17,
                    output_tokens: 9,
                }),
                Event::UsageReported(pb::UsageReported {
                    input_tokens: 17,
                    output_tokens: 9,
                }),
            ),
            (
                ClientEventData::WorkspaceChanges(WorkspaceChangesPayload {
                    paths: vec![crate::workspace::LosslessPath::Bytes {
                        bytes: "/w==".into(),
                    }],
                    truncated: true,
                }),
                Event::WorkspaceChanges(pb::WorkspaceChanges {
                    paths: vec![pb::PathProjection {
                        value: Some(pb::path_projection::Value::OpaquePath(vec![255])),
                    }],
                    truncated: true,
                }),
            ),
            (
                ClientEventData::WriterStateChanged(WriterEventPayload {
                    previous: "active".into(),
                    current: "none".into(),
                    writer_run_id: None,
                    writer_generation: 3,
                }),
                Event::WriterStateChanged(pb::WriterStateChangedEvent {
                    previous: pb::WriterAuthorityState::Active as i32,
                    current: pb::WriterAuthorityState::None as i32,
                    writer_run_id: None,
                    writer_generation: 3,
                }),
            ),
            (
                ClientEventData::RecoveryRequired(RecoveryEventPayload {
                    reason: "turn_outcome_unknown".into(),
                }),
                Event::RecoveryRequired(pb::RecoveryRequiredEvent {
                    safe_reason: "turn_outcome_unknown".into(),
                }),
            ),
            (
                ClientEventData::CommandStarted(CommandStartedPayload {
                    command: vec!["ls".into()],
                }),
                Event::CommandStarted(pb::CommandStarted {
                    safe_command: vec!["ls".into()],
                }),
            ),
            (
                ClientEventData::CommandCompleted(CommandCompletedPayload {
                    command: vec!["false".into()],
                    exit_status: Some(-1),
                }),
                Event::CommandCompleted(pb::CommandCompleted {
                    safe_command: vec!["false".into()],
                    exit_status: Some(-1),
                }),
            ),
            (
                ClientEventData::DiagnosticReported(DiagnosticPayload {
                    message: "bounded note".into(),
                }),
                Event::DiagnosticReported(pb::DiagnosticReported {
                    safe_message: "bounded note".into(),
                }),
            ),
            (
                ClientEventData::GenerationChanged(GenerationPayload {
                    run_generation: 3,
                    server_epoch: 7,
                }),
                Event::GenerationChanged(pb::GenerationChanged {
                    run_generation: 3,
                    server_epoch: 7,
                }),
            ),
            (
                ClientEventData::ReasoningSuppressed(ReasoningSuppressionPayload {
                    method: "item/reasoning/delta".into(),
                    byte_length: 31,
                    sha256: "c".repeat(64),
                    reason: "reasoning_content_not_retained".into(),
                }),
                Event::ReasoningSuppressed(pb::ReasoningSuppressed {
                    method: "item/reasoning/delta".into(),
                    byte_length: 31,
                    sha256: "c".repeat(64),
                    safe_reason: "reasoning_content_not_retained".into(),
                }),
            ),
        ];
        assert_eq!(cases.len(), 15);
        for (source, expected) in cases {
            let source = stamped(source);
            let envelope = envelope(&source, EventProjection::Operational, true).unwrap();
            assert_eq!(
                pb::RunEventEnvelope::decode(envelope.encode_to_vec().as_slice()).unwrap(),
                envelope
            );
            let Some(pb::run_event_envelope::Item::DurableEvent(event)) = envelope.item else {
                panic!("expected durable event");
            };
            assert_eq!(event.event, Some(expected));
            assert_eq!(event.cursor, source.record.cursor);
            assert_eq!(event.event_id, source.record.event_id.to_string());
            assert_eq!(event.run_id, source.record.run_id.to_string());
            assert_eq!(event.stamp, Some(gateway_projection::stamp(&source.stamp)));
            assert_eq!(event.occurred_at.unwrap().nanos, 123_456_000);
            assert!(event.replay);
            assert_eq!(event.projection, pb::ProjectionProfile::Operational as i32);
        }
    }

    #[test]
    fn final_artifact_retains_public_reference_without_promoting_integrity_claims() {
        let mut source = stamped(ClientEventData::DiagnosticReported(DiagnosticPayload {
            message: "temporary".into(),
        }));
        let artifact_id = Uuid::now_v7();
        source.record.data = ClientEventData::ResponseFinal(ResponseEventPayload {
            response: FinalResponse::Artifact {
                artifact: Box::new(ArtifactMetadata {
                    schema_version: 1,
                    artifact_id,
                    run_id: source.record.run_id,
                    kind: "final_response".into(),
                    visibility: "observer".into(),
                    interaction_request_id: None,
                    media_type: "text/markdown".into(),
                    byte_length: 1_048_577,
                    sha256: "c".repeat(64),
                    created_at: source.record.timestamp.clone(),
                    retention: "run_lifetime".into(),
                    integrity: "unverified".into(),
                }),
            },
        });
        let converted = envelope(&source, EventProjection::Minimal, false).unwrap();
        let Some(pb::run_event_envelope::Item::DurableEvent(pb::DurableRunEvent {
            event:
                Some(pb::durable_run_event::Event::FinalResponseAvailable(pb::FinalResponseAvailable {
                    response:
                        Some(pb::FinalResponse {
                            value: Some(pb::final_response::Value::Artifact(artifact)),
                        }),
                })),
            ..
        })) = converted.item
        else {
            panic!("expected artifact event");
        };
        assert_eq!(artifact.artifact_id, artifact_id.to_string());
        assert_eq!(artifact.kind, pb::ArtifactKind::FinalResponse as i32);
        assert_eq!(artifact.visibility, pb::ArtifactVisibility::Observer as i32);
        assert_eq!(artifact.media_type, "text/markdown");
        assert_eq!(artifact.byte_length, 1_048_577);
        assert_eq!(artifact.sha256, "c".repeat(64));
    }

    #[test]
    fn rejects_unrepresentable_values_and_keeps_absent_exit_status_absent() {
        let mut source = stamped(ClientEventData::CommandCompleted(CommandCompletedPayload {
            command: vec!["cmd".into()],
            exit_status: Some(i64::from(i32::MAX) + 1),
        }));
        let error = envelope(&source, EventProjection::Operational, false).unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert!(error.details.get("invariant").is_some());
        if let ClientEventData::CommandCompleted(payload) = &mut source.record.data {
            payload.exit_status = None;
        }
        let projected = envelope(&source, EventProjection::Operational, false).unwrap();
        assert!(matches!(
            projected.item,
            Some(pb::run_event_envelope::Item::DurableEvent(
                pb::DurableRunEvent {
                    event: Some(pb::durable_run_event::Event::CommandCompleted(
                        pb::CommandCompleted {
                            exit_status: None,
                            ..
                        }
                    )),
                    replay: false,
                    ..
                }
            ))
        ));
        assert!(envelope(&source, EventProjection::Minimal, false).is_err());
        source.stamp.writer_state_revision = u64::MAX;
        assert!(envelope(&source, EventProjection::Operational, false).is_err());
        assert!(turn_status("future_status").is_err());
        assert!(interaction_kind("future_kind").is_err());
        assert!(interaction_outcome("future_outcome").is_err());
        assert!(writer_authority("future_authority").is_err());
        assert!(run_lifecycle("future_lifecycle").is_err());
    }
}
