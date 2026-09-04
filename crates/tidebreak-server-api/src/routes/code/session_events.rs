//! `WS /sessions/{id}/events?after=` — snapshot → replay → live.
//!
//! Replay is bounded (`MAX_REPLAY_EVENTS`) and says when it dropped history,
//! so a very long session cannot make one connect read its whole life. The
//! live stream also carries frames the journal does not hold: assistant
//! deltas stream and are never written down (record 57), so they arrive
//! marked `transient` with no cursor of their own.
//!
//! A reader who is not the owner holds their claim through an access row or
//! `deployment` visibility (decision 0086), and either can be withdrawn while
//! they watch. Such a socket also listens on that principal's updates channel,
//! so a revoke closes it on the next event rather than at the next reconnect.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use tidebreak_core::db::code::{list_events, MAX_REPLAY_EVENTS};
use tidebreak_core::{Event, OwnerId, SessionId};

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::bus::{CodeLiveUpdate, LiveTail};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Path, Query};
use crate::routes::events::{gateway_auth_revalidation_timer, wait_for_gateway_auth_revalidation};
use crate::state::AppState;

use super::types::{SequencedEventFrame, SessionEventsQuery};

pub async fn session_events(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Query(query): Query<SessionEventsQuery>,
    headers: axum::http::HeaderMap,
    auth_lease: Option<Extension<GatewayAuthLease>>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    // Authorize before upgrading: the per-session channel is keyed by id, so
    // the principal's claim to this session is settled here, on the chat
    // journal's pattern, rather than by filtering frames afterwards.
    let principal = code.owner().clone();
    let (owner, is_owner) = code.event_stream_access(id).await?;
    let auth_lease = auth_lease.map(|Extension(lease)| lease);
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| {
        stream_events(
            socket,
            state,
            owner,
            id,
            query.after,
            auth_lease,
            if is_owner {
                Viewer::Owner
            } else {
                Viewer::Granted(principal)
            },
        )
    }))
}

/// Who is on the other end of an events socket. The owner's own reader
/// counts as looking at the session; a granted reader's and an adapter's
/// follower do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Viewer {
    Owner,
    /// A second principal reading through an access row or `deployment`
    /// visibility (decision 0086). Their reading is not the owner's review,
    /// and their claim can be withdrawn while they watch, so the variant
    /// carries who they are rather than leaving that to a parallel argument.
    Granted(OwnerId),
    Adapter,
}

pub(super) async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    owner: OwnerId,
    session: SessionId,
    after: i64,
    auth_lease: Option<GatewayAuthLease>,
    viewer: Viewer,
) {
    let mut auth_revalidation = gateway_auth_revalidation_timer(auth_lease.as_ref());
    let Some(runtime) = state.code.clone() else {
        return;
    };
    // A granted reader's claim can be withdrawn while they are watching. The
    // revoking route publishes on their updates channel, so this socket learns
    // of it on the same breath rather than by polling the row (decision 0086).
    // The owner's claim never is, and an adapter's rides on its grant, so
    // neither subscribes.
    let granted = match &viewer {
        Viewer::Granted(principal) => Some(principal.clone()),
        Viewer::Owner | Viewer::Adapter => None,
    };
    let mut access_notices = granted
        .as_ref()
        .map(|principal| runtime.bus.subscribe_updates(principal));
    let (mut live, tail) = runtime.bus.attach(session);
    // Only the desktop's own socket means the owner is looking at the
    // session. The adapter's follower is a renderer, not a viewer: its
    // connects and resyncs must not clear `DoneUnreviewed` for the owner.
    if viewer == Viewer::Owner {
        let _ = runtime.mark_session_viewed(&owner, session).await;
    }
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
            _ = next_access_change(access_notices.as_mut(), session) => {
                let principal = granted
                    .as_ref()
                    .expect("only a granted reader subscribes to access notices");
                let still_current = tidebreak_core::db::code::resolve_session_access(
                    &runtime.db,
                    principal,
                    session,
                )
                .await
                .ok()
                .flatten()
                .is_some();
                if !still_current {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
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
                            &SequencedEventFrame {
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
                        &SequencedEventFrame {
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

/// Wait until this session's access list moves under the reader's feet.
///
/// Pends forever when there is no channel, which is the owner's and the
/// adapter's case: neither holds a row that could be revoked. A dropped
/// notice resolves too, because holding a stream open on a claim this socket
/// can no longer vouch for is not something a full channel may cause.
async fn next_access_change(
    notices: Option<&mut broadcast::Receiver<CodeLiveUpdate>>,
    session: SessionId,
) {
    let Some(notices) = notices else {
        return std::future::pending().await;
    };
    loop {
        match notices.recv().await {
            Ok(CodeLiveUpdate::AccessChanged(id)) if id == session => return,
            Ok(_) => continue,
            Err(RecvError::Lagged(_)) => return,
            // The bus outlives the process, so a closed channel is not a
            // revocation. Stop listening rather than close a live stream.
            Err(RecvError::Closed) => return std::future::pending().await,
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
        &SequencedEventFrame {
            seq: last_seq,
            event: Event::AssistantDelta {
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
    session: SessionId,
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
            &SequencedEventFrame {
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
    frame: &SequencedEventFrame,
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
