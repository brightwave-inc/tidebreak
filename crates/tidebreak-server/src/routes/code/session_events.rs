//! `WS /code/sessions/{id}/events?after=` — snapshot → replay → live.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::db::code::list_events;
use tidebreak_core::{CodeSessionId, OwnerId};

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Path, Query};
use crate::routes::events::{gateway_auth_revalidation_timer, wait_for_gateway_auth_revalidation};
use crate::state::AppState;

use super::types::{SequencedCodeEventFrame, SessionEventsQuery};

pub async fn session_events(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<CodeSessionId>,
    Query(query): Query<SessionEventsQuery>,
    headers: axum::http::HeaderMap,
    auth_lease: Option<Extension<GatewayAuthLease>>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    // Authorize before upgrading: the per-session channel is keyed by id, so
    // the principal's claim to this session is settled here, on the chat
    // journal's pattern, rather than by filtering frames afterwards.
    let _ = code.get_session(id).await?;
    let owner = code.owner().clone();
    let auth_lease = auth_lease.map(|Extension(lease)| lease);
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade
        .on_upgrade(move |socket| stream_events(socket, state, owner, id, query.after, auth_lease)))
}

async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    owner: OwnerId,
    session: CodeSessionId,
    after: i64,
    auth_lease: Option<GatewayAuthLease>,
) {
    let mut auth_revalidation = gateway_auth_revalidation_timer(auth_lease.as_ref());
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
            _ = wait_for_gateway_auth_revalidation(&mut auth_revalidation) => {
                let invalid = match auth_lease.as_ref() {
                    Some(lease) => !lease.revalidate(&state).await,
                    None => false,
                };
                if invalid {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
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
