//! `WS /code/sessions/{id}/events?after=` — snapshot → replay → live.
//!
//! Replay is bounded (`MAX_REPLAY_EVENTS`) and says when it dropped history,
//! so a very long session cannot make one connect read its whole life. The
//! live stream also carries frames the journal does not hold: assistant
//! deltas stream and are never written down (record 57), so they arrive
//! marked `transient` with no cursor of their own.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::db::code::{list_events, MAX_REPLAY_EVENTS};
use tidebreak_core::{CodeEvent, CodeSessionId, OwnerId};

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::bus::LiveTail;
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
    let (mut live, tail) = runtime.bus.attach(session);
    let _ = runtime.mark_session_viewed(&owner, session).await;
    let mut last_seq = after;
    if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }
    if send_live_tail(&mut socket, &tail, last_seq).await.is_err() {
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
                    // A live-only event carries no journal position. Stamp it
                    // with this socket's cursor so a client that resumes from
                    // the last `seq` it saw asks for the right place. Drop it
                    // when the journal has moved past the point it streamed
                    // behind: the replay that read that far already carries
                    // the message stating the same text, and applying the
                    // fragment on top would say it twice.
                    let Some(seq) = event.seq else {
                        if !transient_is_current(event.cursor, last_seq) {
                            continue;
                        }
                        if send_frame(
                            &mut socket,
                            &SequencedCodeEventFrame {
                                seq: last_seq,
                                event: event.event,
                                replayed: None,
                                transient: Some(true),
                                replacement: None,
                                truncated: None,
                            },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    };
                    if seq <= last_seq {
                        continue;
                    }
                    if seq > last_seq.saturating_add(1) {
                        if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = seq;
                    if send_frame(
                        &mut socket,
                        &SequencedCodeEventFrame {
                            seq,
                            event: event.event,
                            replayed: None,
                            transient: None,
                            replacement: None,
                            truncated: None,
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

/// Hand a fresh reader the assistant text that has streamed but is not yet
/// written down.
///
/// Without this, a client that connects mid-answer sees the sentence from
/// wherever it happened to arrive. The frame is a replacement because a
/// reconnect may already hold a prefix while also missing text that streamed
/// during the disconnect. The tail is only trustworthy while replay stays
/// behind the position it was captured at: a replay that reads further has
/// already picked up the event that retired it.
async fn send_live_tail(
    socket: &mut WebSocket,
    tail: &LiveTail,
    last_seq: i64,
) -> Result<(), axum::Error> {
    if tail.assistant.is_empty() || last_seq > tail.cursor {
        return Ok(());
    }
    send_frame(
        socket,
        &SequencedCodeEventFrame {
            seq: last_seq,
            event: CodeEvent::AssistantDelta {
                text: tail.assistant.clone(),
            },
            replayed: None,
            transient: Some(true),
            replacement: Some(true),
            truncated: None,
        },
    )
    .await
}

async fn replay_after(
    socket: &mut WebSocket,
    store: &tidebreak_core::DbStore,
    owner: &OwnerId,
    session: CodeSessionId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let page = list_events(store, owner, session, *last_seq, MAX_REPLAY_EVENTS)
        .await
        .map_err(|_| ())?;
    let mut truncated = page.truncated;
    for event in page.events {
        *last_seq = event.seq;
        send_frame(
            socket,
            &SequencedCodeEventFrame {
                seq: event.seq,
                event: event.event,
                replayed: Some(true),
                transient: None,
                replacement: None,
                // Only the first frame of a capped window carries the flag:
                // it is the one the dropped history sits in front of.
                truncated: truncated.then(|| {
                    truncated = false;
                    true
                }),
            },
        )
        .await
        .map_err(|_| ())?;
    }
    Ok(())
}

/// Is a live-only event still worth delivering to a socket at `last_seq`?
///
/// `cursor` is where the journal stood when the event streamed. A socket
/// whose replay has read past that point already holds the event that
/// superseded it — the `assistant_message` restating the whole answer, the
/// tool call that ended the run, the turn's own end — so delivering the
/// fragment on top would repeat text the reader already has.
///
/// This is reachable on every connect: the receiver is subscribed before the
/// replay query runs, so anything published during the replay is queued
/// behind frames the replay itself may already have sent.
fn transient_is_current(cursor: i64, last_seq: i64) -> bool {
    cursor >= last_seq
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
    use super::transient_is_current;

    /// A delta published while the socket was still replaying, whose text the
    /// replay then covered, must be dropped rather than applied twice.
    #[test]
    fn a_delta_the_replay_overtook_is_not_delivered_again() {
        // Streamed at cursor 40; the reader replayed through 41, which is the
        // message stating the same words.
        assert!(!transient_is_current(40, 41));
        // Streamed at the position the reader stopped at: still the live tail.
        assert!(transient_is_current(41, 41));
        // Streamed after the reader's cursor: plainly still ahead of it.
        assert!(transient_is_current(42, 41));
    }
}
