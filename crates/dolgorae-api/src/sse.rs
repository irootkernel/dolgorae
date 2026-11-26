//! Server-Sent Events (`/v2/sessions/{session_id}/events`) with exclusive
//! `Last-Event-ID` replay, non-advancing heartbeats, and no-silent-gap lag recovery.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use dolgorae_core::{CallerContext, CoreEngine, CoreError, Cursor, ReplayResult, SessionEvent};
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;
use tokio_stream::wrappers::ReceiverStream;

/// Parses `Last-Event-ID` header (preferred) or `?after=` query strictly as a decimal `Cursor`.
pub fn parse_after_cursor(
    headers: &HeaderMap,
    after_query: Option<&str>,
) -> Result<Option<Cursor>, CoreError> {
    if let Some(hdr) = headers.get("last-event-id") {
        let raw = hdr
            .to_str()
            .map_err(|_| CoreError::bad_request("Last-Event-ID header must be valid ASCII"))?
            .trim();
        if !raw.is_empty() {
            return Ok(Some(Cursor::parse(raw)?));
        }
    }
    if let Some(q) = after_query {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            return Ok(Some(Cursor::parse(trimmed)?));
        }
    }
    Ok(None)
}

/// Builds a live + replayed SSE stream for one session.
///
/// Guarantees:
/// - Subscribes to the live channel BEFORE replaying durable events.
/// - Durable events emit `id: <cursor>`; ephemeral events (`text_delta`,
///   `tool_delta`), `heartbeat`, and `refresh_required` NEVER emit `id:`.
/// - On `RecvError::Lagged(_)`, re-queries durable replay after `last_delivered_cursor`
///   and either catches up cleanly or emits `refresh_required` and ends the stream.
pub async fn build_session_sse(
    engine: Arc<CoreEngine>,
    caller: CallerContext,
    session_id: String,
    after: Option<Cursor>,
    heartbeat_interval: Duration,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, CoreError> {
    // For live-only subscribers (`after == None`), capture `head_cursor` before
    // subscribing so lag recovery replays only post-subscribe events instead of
    // replaying historical events from cursor 0.
    let initial_head = if after.is_none() {
        Some(engine.get_session(&caller, &session_id).await?.head_cursor)
    } else {
        None
    };

    let (initial_replay, mut live_rx) =
        engine.subscribe_events(&caller, &session_id, after).await?;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(8);

    tokio::spawn(async move {
        let mut last_delivered: Option<Cursor> = after.or(initial_head);

        match initial_replay {
            ReplayResult::RefreshRequired { scopes } => {
                let _ = tx.send(Ok(refresh_required_event(&scopes))).await;
                return;
            }
            ReplayResult::Events(events) => {
                for ev in events {
                    if let Some(c) = ev.cursor {
                        last_delivered = Some(c);
                    }
                    if tx.send(Ok(session_event_to_sse(&ev))).await.is_err() {
                        return;
                    }
                }
            }
        }

        let mut heartbeat =
            tokio::time::interval(heartbeat_interval.max(Duration::from_millis(20)));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Consume immediate initial tick so heartbeats fire after one interval.
        heartbeat.tick().await;

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let hb = Event::default().event("heartbeat").data("{}");
                    if tx.send(Ok(hb)).await.is_err() {
                        break;
                    }
                }
                recv = live_rx.recv() => {
                    match recv {
                        Ok(ev) => {
                            if let Some(c) = ev.cursor {
                                if last_delivered.is_some_and(|prev| c <= prev) {
                                    continue;
                                }
                                last_delivered = Some(c);
                            }
                            if tx.send(Ok(session_event_to_sse(&ev))).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                session_id = %session_id,
                                skipped,
                                "SSE subscriber lagged; recovering from durable store"
                            );
                            let catchup_after = last_delivered.unwrap_or(Cursor(0));
                            match engine
                                .subscribe_events(&caller, &session_id, Some(catchup_after))
                                .await
                            {
                                Ok((ReplayResult::Events(missed), new_rx)) => {
                                    for ev in missed {
                                        if let Some(c) = ev.cursor {
                                            if last_delivered.is_some_and(|prev| c <= prev) {
                                                continue;
                                            }
                                            last_delivered = Some(c);
                                        }
                                        if tx.send(Ok(session_event_to_sse(&ev))).await.is_err() {
                                            return;
                                        }
                                    }
                                    live_rx = new_rx;
                                }
                                Ok((ReplayResult::RefreshRequired { scopes }, _)) => {
                                    let _ = tx.send(Ok(refresh_required_event(&scopes))).await;
                                    break;
                                }
                                Err(err) => {
                                    tracing::warn!(session_id = %session_id, %err, "SSE lag recovery failed");
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

fn session_event_to_sse(ev: &SessionEvent) -> Event {
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_owned());
    let mut frame = Event::default().event(ev.event_type.clone()).data(data);
    if let Some(cursor) = ev.cursor {
        frame = frame.id(cursor.to_string());
    }
    frame
}

fn refresh_required_event(scopes: &[String]) -> Event {
    let data = json!({ "scopes": scopes }).to_string();
    Event::default().event("refresh_required").data(data)
}
