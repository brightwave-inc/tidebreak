//! `WS /code/sessions/{id}/events?after=` — snapshot → replay → live.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::db::code::list_events;
use tidebreak_core::{CodeEvent, CodeSessionId, OwnerId, SequencedCodeEvent};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Path, Query};
use crate::state::AppState;

use super::types::{SequencedCodeEventFrame, SessionEventsQuery};

pub async fn session_events(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<CodeSessionId>,
    Query(query): Query<SessionEventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    // Authorize before upgrading: the per-session channel is keyed by id, so
    // the principal's claim to this session is settled here, on the chat
    // journal's pattern, rather than by filtering frames afterwards.
    let _ = code.get_session(id).await?;
    let owner = code.owner().clone();
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, owner, id, query.after)))
}

async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    owner: OwnerId,
    session: CodeSessionId,
    after: i64,
) {
    let Some(runtime) = state.code.clone() else {
        return;
    };
    let mut live = runtime.bus.subscribe(session);
    let _ = runtime.mark_session_viewed(&owner, session).await;
    let mut last_seq = after;
    if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            live_event = live.recv() => match live_event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue;
                    }
                    if event.seq > last_seq.saturating_add(1) {
                        if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = event.seq;
                    if send_frame(
                        &mut socket,
                        &SequencedCodeEventFrame {
                            seq: event.seq,
                            event: event.event,
                            replayed: None,
                        },
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

async fn replay_after(
    socket: &mut WebSocket,
    store: &tidebreak_core::DbStore,
    owner: &OwnerId,
    session: CodeSessionId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = list_events(store, owner, session, *last_seq)
        .await
        .map_err(|_| ())?;
    for event in compact_replay_events(events) {
        *last_seq = event.seq;
        send_frame(
            socket,
            &SequencedCodeEventFrame {
                seq: event.seq,
                event: event.event,
                replayed: Some(true),
            },
        )
        .await
        .map_err(|_| ())?;
    }
    Ok(())
}

/// Collapse adjacent renderer-equivalent text deltas before they cross the
/// WebSocket boundary. The durable journal and live stream remain lossless;
/// replay retains the final sequence cursor for every compacted run.
fn compact_replay_events(events: Vec<SequencedCodeEvent>) -> Vec<SequencedCodeEvent> {
    let mut compacted = Vec::<SequencedCodeEvent>::with_capacity(events.len());
    for event in events {
        if let Some(previous) = compacted.last_mut() {
            let consecutive = previous.seq.saturating_add(1) == event.seq;
            let merged = if consecutive {
                match (&mut previous.event, &event.event) {
                    (
                        CodeEvent::AssistantDelta { text: previous },
                        CodeEvent::AssistantDelta { text: next },
                    )
                    | (
                        CodeEvent::ReasoningDelta { text: previous },
                        CodeEvent::ReasoningDelta { text: next },
                    ) => {
                        previous.push_str(next);
                        true
                    }
                    _ => false,
                }
            } else {
                false
            };
            if merged {
                previous.seq = event.seq;
                continue;
            }
        }
        compacted.push(event);
    }
    compacted
}

async fn send_frame(
    socket: &mut WebSocket,
    frame: &SequencedCodeEventFrame,
) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(frame) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_compaction_preserves_text_boundaries_and_final_sequences() {
        let compacted = compact_replay_events(vec![
            SequencedCodeEvent {
                seq: 1,
                event: CodeEvent::AssistantDelta { text: "hel".into() },
            },
            SequencedCodeEvent {
                seq: 2,
                event: CodeEvent::AssistantDelta { text: "lo".into() },
            },
            SequencedCodeEvent {
                seq: 3,
                event: CodeEvent::ReasoningDelta {
                    text: "think".into(),
                },
            },
            SequencedCodeEvent {
                seq: 4,
                event: CodeEvent::ReasoningDelta { text: "ing".into() },
            },
            SequencedCodeEvent {
                seq: 5,
                event: CodeEvent::TurnInterrupted,
            },
            SequencedCodeEvent {
                seq: 7,
                event: CodeEvent::AssistantDelta {
                    text: "again".into(),
                },
            },
        ]);

        assert_eq!(
            compacted,
            vec![
                SequencedCodeEvent {
                    seq: 2,
                    event: CodeEvent::AssistantDelta {
                        text: "hello".into(),
                    },
                },
                SequencedCodeEvent {
                    seq: 4,
                    event: CodeEvent::ReasoningDelta {
                        text: "thinking".into(),
                    },
                },
                SequencedCodeEvent {
                    seq: 5,
                    event: CodeEvent::TurnInterrupted,
                },
                SequencedCodeEvent {
                    seq: 7,
                    event: CodeEvent::AssistantDelta {
                        text: "again".into(),
                    },
                },
            ]
        );
    }
}
