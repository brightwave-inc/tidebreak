//! `WS /code/sessions/{id}/events?after=` — snapshot → replay → live.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::db::code::list_events;
use tidebreak_core::CodeSessionId;

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::extract::{Path, Query};
use crate::state::AppState;

use super::require_code;
use super::types::{SequencedCodeEventFrame, SessionEventsQuery};

pub async fn session_events(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
    Query(query): Query<SessionEventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let runtime = require_code(&state)?;
    let _ = runtime.get_session(id).await?;
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after)))
}

async fn stream_events(mut socket: WebSocket, state: AppState, session: CodeSessionId, after: i64) {
    let Some(runtime) = state.code.clone() else {
        return;
    };
    let mut live = runtime.bus.subscribe(session);
    let mut last_seq = after;
    if replay_after(&mut socket, &runtime.db, session, &mut last_seq)
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
                        if replay_after(&mut socket, &runtime.db, session, &mut last_seq)
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
                    if replay_after(&mut socket, &runtime.db, session, &mut last_seq)
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
    session: CodeSessionId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = list_events(store, session, *last_seq)
        .await
        .map_err(|_| ())?;
    for event in events {
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

async fn send_frame(
    socket: &mut WebSocket,
    frame: &SequencedCodeEventFrame,
) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(frame) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
