//! Public-v2 observation projections over shared semantic history and transport.
use crate::gateway::{EventPage, GatewayTransport};
use crate::gateway_service::{invalid, required};
use crate::gateway_v2::{self, ModernGatewayBackend};
use crate::machine::MachineError;
use crate::protocol::{public_v1 as old, public_v2 as pb};
use base64::Engine as _;
use prost::Message as _;
use serde::{Deserialize, Serialize};
use tonic::{Request, Response, Status};

const PAGE_BYTES: u32 = 4 * 1024 * 1024;

fn invariant() -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "public observation cannot be represented",
        false,
        serde_json::json!({"invariant":"public-v2 observation projection"}),
    )
}
macro_rules! enum_map {
    ($name:ident, $kind:ident) => {
        fn $name(value: i32) -> Result<i32, MachineError> {
            let value = old::$kind::try_from(value).map_err(|_| invariant())?;
            pb::$kind::from_str_name(value.as_str_name())
                .map(|v| v as i32)
                .ok_or_else(invariant)
        }
    };
}
enum_map!(lifecycle, RunLifecycle);
enum_map!(turn_status, TurnStatus);
enum_map!(projection, ProjectionProfile);
enum_map!(interaction_kind, InteractionKind);
enum_map!(interaction_outcome, InteractionOutcome);
enum_map!(writer_authority, WriterAuthorityState);
enum_map!(artifact_kind, ArtifactKind);
enum_map!(artifact_visibility, ArtifactVisibility);
enum_map!(timeline_type, TimelineItemType);
enum_map!(timeline_status, TimelineItemStatus);
enum_map!(interaction_status, InteractionStatus);
enum_map!(image_detail, ImageDetail);
enum_map!(end_reason, StreamEndReason);

fn artifact(v: old::ArtifactRef) -> Result<pb::ArtifactRef, MachineError> {
    Ok(pb::ArtifactRef {
        artifact_id: v.artifact_id,
        kind: artifact_kind(v.kind)?,
        visibility: artifact_visibility(v.visibility)?,
        media_type: v.media_type,
        byte_length: v.byte_length,
        sha256: v.sha256,
    })
}
fn response(v: old::FinalResponse) -> Result<pb::FinalResponse, MachineError> {
    Ok(pb::FinalResponse {
        value: v
            .value
            .map(|v| {
                Ok::<_, MachineError>(match v {
                    old::final_response::Value::InlineUtf8(v) => {
                        pb::final_response::Value::InlineUtf8(v)
                    }
                    old::final_response::Value::Unavailable(v) => {
                        pb::final_response::Value::Unavailable(pb::UnavailableContent {
                            observed_byte_length: v.observed_byte_length,
                            sha256: v.sha256,
                            reason: v.reason,
                        })
                    }
                    old::final_response::Value::Artifact(v) => {
                        pb::final_response::Value::Artifact(artifact(v)?)
                    }
                })
            })
            .transpose()?,
    })
}
fn path(v: old::PathProjection) -> pb::PathProjection {
    pb::PathProjection {
        value: v.value.map(|v| match v {
            old::path_projection::Value::Utf8Path(v) => pb::path_projection::Value::Utf8Path(v),
            old::path_projection::Value::OpaquePath(v) => pb::path_projection::Value::OpaquePath(v),
        }),
    }
}
fn stamp(v: old::ProjectionStamp) -> pb::ProjectionStamp {
    pb::ProjectionStamp {
        captured_head_cursor: v.captured_head_cursor,
        run_state_revision: v.run_state_revision,
        writer_state_revision: v.writer_state_revision,
        interaction_state_revision: v.interaction_state_revision,
    }
}

pub(crate) fn envelope(v: old::RunEventEnvelope) -> Result<pb::RunEventEnvelope, MachineError> {
    use old::run_event_envelope::Item as Old;
    use pb::run_event_envelope::Item;
    Ok(pb::RunEventEnvelope {
        item: v
            .item
            .map(|item| {
                Ok::<_, MachineError>(match item {
                    Old::Heartbeat(v) => Item::Heartbeat(pb::RunEventHeartbeat {
                        run_id: v.run_id,
                        durable_head_cursor: v.durable_head_cursor,
                        lifecycle: lifecycle(v.lifecycle)?,
                        emitted_at: v.emitted_at,
                    }),
                    Old::StreamEnd(v) => Item::StreamEnd(pb::RunEventStreamEnd {
                        run_id: v.run_id,
                        durable_head_cursor: v.durable_head_cursor,
                        reason: end_reason(v.reason)?,
                    }),
                    Old::DurableEvent(v) => Item::DurableEvent(pb::DurableRunEvent {
                        cursor: v.cursor,
                        event_id: v.event_id,
                        occurred_at: v.occurred_at,
                        workspace_id: v.workspace_id,
                        run_id: v.run_id,
                        thread_id: v.thread_id,
                        turn_id: v.turn_id,
                        server_key: v.server_key,
                        server_epoch: v.server_epoch,
                        replay: v.replay,
                        projection: projection(v.projection)?,
                        projection_version: 1,
                        stamp: v.stamp.map(stamp),
                        event: v.event.map(event).transpose()?,
                    }),
                })
            })
            .transpose()?,
    })
}
fn event(v: old::durable_run_event::Event) -> Result<pb::durable_run_event::Event, MachineError> {
    use old::durable_run_event::Event as Old;
    use pb::durable_run_event::Event;
    Ok::<_, MachineError>(match v {
        Old::RunStateChanged(v) => Event::RunStateChanged(pb::RunStateChanged {
            previous: v.previous.map(lifecycle).transpose()?,
            current: lifecycle(v.current)?,
        }),
        Old::TurnStateChanged(v) => Event::TurnStateChanged(pb::TurnStateChanged {
            previous: v.previous.map(turn_status).transpose()?,
            current: turn_status(v.current)?,
        }),
        Old::FinalResponseAvailable(v) => {
            Event::FinalResponseAvailable(pb::FinalResponseAvailable {
                response: v.response.map(response).transpose()?,
            })
        }
        Old::InteractionOpened(v) => Event::InteractionOpened(pb::InteractionOpenedEvent {
            interaction_id: v.interaction_id,
            kind: interaction_kind(v.kind)?,
        }),
        Old::InteractionResolved(v) => Event::InteractionResolved(pb::InteractionResolvedEvent {
            interaction_id: v.interaction_id,
            outcome: interaction_outcome(v.outcome)?,
        }),
        Old::RuntimeErrorOccurred(v) => Event::RuntimeErrorOccurred(pb::RuntimeErrorOccurred {
            error_code: v.error_code,
            safe_message: v.safe_message,
        }),
        Old::UsageReported(v) => Event::UsageReported(pb::UsageReported {
            input_tokens: v.input_tokens,
            output_tokens: v.output_tokens,
        }),
        Old::WorkspaceChanges(v) => Event::WorkspaceChanges(pb::WorkspaceChanges {
            paths: v.paths.into_iter().map(path).collect(),
            truncated: v.truncated,
        }),
        Old::WriterStateChanged(v) => Event::WriterStateChanged(pb::WriterStateChangedEvent {
            previous: writer_authority(v.previous)?,
            current: writer_authority(v.current)?,
            writer_run_id: v.writer_run_id,
            writer_generation: v.writer_generation,
        }),
        Old::RecoveryRequired(v) => Event::RecoveryRequired(pb::RecoveryRequiredEvent {
            safe_reason: v.safe_reason,
        }),
        Old::CommandStarted(v) => Event::CommandStarted(pb::CommandStarted {
            safe_command: v.safe_command,
        }),
        Old::CommandCompleted(v) => Event::CommandCompleted(pb::CommandCompleted {
            safe_command: v.safe_command,
            exit_status: v.exit_status,
        }),
        Old::DiagnosticReported(v) => Event::DiagnosticReported(pb::DiagnosticReported {
            safe_message: v.safe_message,
        }),
        Old::GenerationChanged(v) => Event::GenerationChanged(pb::GenerationChanged {
            run_generation: v.run_generation,
            server_epoch: v.server_epoch,
        }),
        Old::ReasoningSuppressed(v) => Event::ReasoningSuppressed(pb::ReasoningSuppressed {
            method: v.method,
            byte_length: v.byte_length,
            sha256: v.sha256,
            safe_reason: v.safe_reason,
        }),
    })
}
fn timeline_item(v: old::TimelineItem) -> Result<pb::TimelineItem, MachineError> {
    Ok(pb::TimelineItem {
        r#type: timeline_type(v.r#type)?,
        cursor: v.cursor,
        run_id: v.run_id,
        turn_id: v.turn_id,
        occurred_at: v.occurred_at,
        provider_item_id: v.provider_item_id,
        provider_order: v.provider_order,
        content: v
            .content
            .map(|v| {
                Ok::<_, MachineError>(match v {
                    old::timeline_item::Content::InlineText(v) => {
                        pb::timeline_item::Content::InlineText(v)
                    }
                    old::timeline_item::Content::Artifact(v) => {
                        pb::timeline_item::Content::Artifact(artifact(v)?)
                    }
                })
            })
            .transpose()?,
        images: v
            .images
            .into_iter()
            .map(|v| {
                Ok(pb::ImageInputMetadata {
                    ordinal: v.ordinal,
                    detail: image_detail(v.detail)?,
                    media_type: v.media_type,
                    byte_length: v.byte_length,
                    sha256: v.sha256,
                })
            })
            .collect::<Result<_, MachineError>>()?,
        interaction_id: v.interaction_id,
        status: v.status.map(timeline_status).transpose()?,
        interaction_kind: v.interaction_kind.map(interaction_kind).transpose()?,
        interaction_status: v.interaction_status.map(interaction_status).transpose()?,
        interaction_safe_title: v.interaction_safe_title,
    })
}

pub(crate) fn watch_page(
    backend: &dyn ModernGatewayBackend,
    request: pb::WatchRunEventsRequest,
) -> Result<EventPage, MachineError> {
    if request.projection_version != 1 {
        return Err(schema("event projection", request.projection_version));
    }
    // The shared semantic reader owns capture, historical stamps, cursor
    // validation and safe filtering. No legacy-adapter admission is applied.
    let (root, snapshot, _) =
        gateway_v2::snapshot(backend, required(request.run.as_ref(), "run")?)?;
    let mut page = crate::gateway_observation::watch_page(
        &root,
        &snapshot,
        &request.after_cursor,
        request.projection,
        1,
    )?;
    for value in &mut page.events {
        if let Some(old::run_event_envelope::Item::DurableEvent(event)) = &mut value.item {
            event.projection_version = 1;
        }
    }
    Ok(page)
}
fn schema(name: &str, requested: u32) -> MachineError {
    MachineError::new(
        "UNSUPPORTED_SCHEMA_VERSION",
        "unsupported public observation version",
        false,
        serde_json::json!({"schema":name,"requested":requested,"supported":[1]}),
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageCursor {
    version: u32,
    workspace_id: String,
    run_id: uuid::Uuid,
    controller_id: uuid::Uuid,
    controller_generation: u64,
    head: u64,
    after: u64,
}
fn cursor_mac(payload: &[u8], key: &str) -> String {
    let mut bytes = b"dolgorae-timeline-page-v1\0".to_vec();
    bytes.extend_from_slice(payload);
    bytes.push(0);
    bytes.extend_from_slice(key.as_bytes());
    crate::jcs::sha256_hex(&bytes)
}
fn encode_cursor(v: &PageCursor, key: &str) -> Result<String, MachineError> {
    let payload = serde_json::to_vec(v).map_err(|_| invariant())?;
    Ok(format!(
        "{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload),
        cursor_mac(&payload, key)
    ))
}
fn invalid_cursor() -> MachineError {
    MachineError::new(
        "EVENT_CURSOR_INVALID",
        "timeline cursor is malformed or belongs to another capture",
        false,
        serde_json::json!({}),
    )
}
fn decode_cursor(token: &str, key: &str) -> Result<PageCursor, MachineError> {
    if token.len() > 2048 {
        return Err(invalid_cursor());
    }
    let (payload, mac) = token.split_once('.').ok_or_else(invalid_cursor)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| invalid_cursor())?;
    if mac != cursor_mac(&bytes, key) {
        return Err(invalid_cursor());
    }
    let value: PageCursor = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    if value.version != 1 || value.after > value.head {
        return Err(invalid_cursor());
    }
    Ok(value)
}

pub(crate) fn list(
    backend: &dyn ModernGatewayBackend,
    request: pb::ListRunTimelineItemsRequest,
) -> Result<pb::ListRunTimelineItemsResponse, MachineError> {
    if request.timeline_version != 1 {
        return Err(schema("Controller timeline", request.timeline_version));
    }
    let limit = if request.limit == 0 {
        100
    } else {
        request.limit
    };
    if limit > 500 {
        return Err(invalid("limit", "timeline limit must be at most 500"));
    }
    let maximum = if request.maximum_encoded_bytes == 0 {
        PAGE_BYTES
    } else {
        request.maximum_encoded_bytes
    };
    if maximum > PAGE_BYTES {
        return Err(invalid(
            "maximum_encoded_bytes",
            "timeline budget must be at most 4 MiB",
        ));
    }
    let latest = match pb::TimelineDirection::try_from(request.direction) {
        Ok(pb::TimelineDirection::Unspecified | pb::TimelineDirection::Forward) => false,
        Ok(pb::TimelineDirection::Latest) => true,
        Err(_) => {
            return Err(MachineError::new(
                "UNSUPPORTED_SCHEMA_VERSION",
                "input enum is not supported by this public protocol",
                false,
                serde_json::json!({"schema":"dolgorae.public.v2.TimelineDirection",
                    "requested":u32::from_ne_bytes(request.direction.to_ne_bytes()),
                    "supported":[0,1,2]}),
            ));
        }
    };
    if latest && !request.after_cursor.is_empty() {
        return Err(invalid(
            "after_cursor",
            "latest requires an empty initial cursor",
        ));
    }
    let reference = required(request.run.as_ref(), "run")?;
    let (root, snapshot) = gateway_v2::protected_snapshot(backend, reference)?;
    let carrier_ref = request.controller.as_ref().ok_or_else(|| {
        MachineError::interaction_full_payload_requires_controller(
            snapshot.manifest.run_id,
            "run.timeline.list",
        )
    })?;
    let carrier = backend.controller(&gateway_v2::controller(carrier_ref))?;
    snapshot.authorize_current_controller(&root, &carrier, "run.timeline.list")?;
    let key = &snapshot.controller_authority()?.capability_sha256;
    let mut cursor = PageCursor {
        version: 1,
        workspace_id: snapshot.manifest.workspace_id.clone(),
        run_id: snapshot.manifest.run_id,
        controller_id: snapshot.controller.controller_id,
        controller_generation: snapshot.controller.generation,
        head: snapshot.stamp.run_state_revision,
        after: 0,
    };
    if !request.after_cursor.is_empty() {
        if let Some(after) = crate::ledger::parse_event_cursor(&request.after_cursor) {
            if after > cursor.head {
                return Err(crate::ledger::event_cursor_invalid(
                    cursor.run_id,
                    &request.after_cursor,
                    &cursor.head.to_string(),
                ));
            }
            cursor.after = after;
        } else {
            cursor = decode_cursor(&request.after_cursor, key)?;
            if cursor.workspace_id != snapshot.manifest.workspace_id
                || cursor.run_id != snapshot.manifest.run_id
                || cursor.controller_id != snapshot.controller.controller_id
                || cursor.controller_generation != snapshot.controller.generation
                || cursor.head > snapshot.stamp.run_state_revision
            {
                return Err(invalid_cursor());
            }
        }
    }
    let items = crate::timeline::entries_at(&root, &snapshot, cursor.after, cursor.head)?;
    let mut response = pb::ListRunTimelineItemsResponse {
        context: Some(gateway_v2::context(backend)),
        items: Vec::new(),
        captured_head_cursor: cursor.head.to_string(),
        next_after_cursor: None,
        stamp: Some(pb::ProjectionStamp {
            captured_head_cursor: snapshot.stamp.captured_head_cursor.clone(),
            run_state_revision: snapshot.stamp.run_state_revision,
            writer_state_revision: snapshot.stamp.writer_state_revision,
            interaction_state_revision: snapshot.stamp.interaction_state_revision,
        }),
        encoded_byte_length: 0,
        complete_page: true,
    };
    let count = items.len();
    if latest {
        for item in items.into_iter().rev().take(limit as usize) {
            let alternate = item.artifact.map(artifact).transpose()?;
            response.items.insert(0, timeline_item(item.item)?);
            fit_item(&mut response, 0, alternate, maximum);
            if response.encoded_byte_length > maximum {
                response.items.remove(0);
                if response.items.is_empty() {
                    return Err(capacity());
                }
                break;
            }
        }
    } else {
        for (index, item) in items.into_iter().enumerate() {
            let previous = response.next_after_cursor.clone();
            cursor.after =
                crate::ledger::parse_event_cursor(&item.item.cursor).ok_or_else(invariant)?;
            let alternate = item.artifact.map(artifact).transpose()?;
            response.items.push(timeline_item(item.item)?);
            response.next_after_cursor = if index + 1 < count {
                Some(encode_cursor(&cursor, key)?)
            } else {
                None
            };
            let index = response.items.len() - 1;
            fit_item(&mut response, index, alternate, maximum);
            if response.encoded_byte_length > maximum {
                response.items.pop();
                response.next_after_cursor = previous;
                if response.items.is_empty() {
                    return Err(capacity());
                }
                break;
            }
            if response.items.len() == limit as usize {
                break;
            }
        }
    }
    size(&mut response);
    if response.encoded_byte_length > maximum {
        return Err(capacity());
    }
    snapshot.authorize_current_controller(&root, &carrier, "run.timeline.list")?;
    Ok(response)
}
fn fit_item(
    response: &mut pb::ListRunTimelineItemsResponse,
    index: usize,
    alternate: Option<pb::ArtifactRef>,
    maximum: u32,
) {
    size(response);
    if response.encoded_byte_length > maximum
        && let Some(reference) = alternate
    {
        response.items[index].content = Some(pb::timeline_item::Content::Artifact(reference));
        size(response);
    }
}

fn capacity() -> MachineError {
    MachineError::new(
        "SOURCE_TOO_LARGE",
        "timeline page cannot fit its encoded byte budget",
        false,
        serde_json::json!({"bound":"timeline_page_bytes"}),
    )
}
fn size(response: &mut pb::ListRunTimelineItemsResponse) {
    response.encoded_byte_length = 0;
    loop {
        let size = response.encoded_len() as u32;
        if response.encoded_byte_length == size {
            return;
        }
        response.encoded_byte_length = size;
    }
}

#[tonic::async_trait]
impl pb::observation_service_server::ObservationService for GatewayTransport {
    type WatchRunEventsStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<pb::RunEventEnvelope, Status>> + Send>,
    >;
    async fn watch_run_events(
        &self,
        request: Request<pb::WatchRunEventsRequest>,
    ) -> Result<Response<Self::WatchRunEventsStream>, Status> {
        use tokio_stream::StreamExt as _;
        let method = "ObservationService.WatchRunEvents";
        let deadline =
            crate::gateway::rpc_deadline_with(&request, method, gateway_v2::error_status)?;
        gateway_v2::checked_context(request.get_ref().context.as_ref())
            .map_err(|e| gateway_v2::error_status(&e, method))?;
        let stream = self.watch_v2(request.into_inner(), deadline).await?;
        Ok(Response::new(Box::pin(stream.map(move |v| {
            v.and_then(|v| envelope(v).map_err(|e| gateway_v2::error_status(&e, method)))
        }))))
    }
    async fn list_run_timeline_items(
        &self,
        request: Request<pb::ListRunTimelineItemsRequest>,
    ) -> Result<Response<pb::ListRunTimelineItemsResponse>, Status> {
        let method = "ObservationService.ListRunTimelineItems";
        let deadline =
            crate::gateway::rpc_deadline_with(&request, method, gateway_v2::error_status)?;
        gateway_v2::checked_context(request.get_ref().context.as_ref())
            .map_err(|e| gateway_v2::error_status(&e, method))?;
        let request = request.into_inner();
        self.execute_v2(method, deadline, move |source| {
            list(source.modern()?, request)
        })
        .await
        .map(Response::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuation_rejects_forged_aggregate_metadata_even_with_recomputed_mac() {
        let cursor = PageCursor {
            version: 1,
            workspace_id: "a".repeat(64),
            run_id: uuid::Uuid::now_v7(),
            controller_id: uuid::Uuid::now_v7(),
            controller_generation: 1,
            head: 10,
            after: 4,
        };
        let token = encode_cursor(&cursor, "controller-key").unwrap();
        assert_eq!(decode_cursor(&token, "controller-key").unwrap().head, 10);
        let mut value = serde_json::to_value(&cursor).unwrap();
        value["stamp"] =
            serde_json::json!({"writer_state_revision":9,"interaction_state_revision":9});
        let payload = serde_json::to_vec(&value).unwrap();
        let forged = format!(
            "{}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload),
            cursor_mac(&payload, "controller-key")
        );
        assert_eq!(
            decode_cursor(&forged, "controller-key").err().unwrap().code,
            "EVENT_CURSOR_INVALID"
        );
    }
    #[test]
    fn public_event_projection_version_is_independent_of_persisted_schema() {
        let source = old::RunEventEnvelope {
            item: Some(old::run_event_envelope::Item::DurableEvent(
                old::DurableRunEvent {
                    projection_version: 2,
                    projection: old::ProjectionProfile::Operational as i32,
                    event: Some(old::durable_run_event::Event::InteractionOpened(
                        old::InteractionOpenedEvent {
                            interaction_id: "approval".into(),
                            kind: old::InteractionKind::McpElicitation as i32,
                        },
                    )),
                    ..Default::default()
                },
            )),
        };
        let result = envelope(source).unwrap();
        let Some(pb::run_event_envelope::Item::DurableEvent(event)) = result.item else {
            panic!("expected event");
        };
        assert_eq!(event.projection_version, 1);
        let Some(pb::durable_run_event::Event::InteractionOpened(event)) = event.event else {
            panic!("expected interaction");
        };
        assert_eq!(event.kind, pb::InteractionKind::McpElicitation as i32);
    }
}
