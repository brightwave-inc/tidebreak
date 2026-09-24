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
use tokio::time::Instant;

use tidebreak_core::db::code::{list_events, list_events_before, MAX_REPLAY_EVENTS};
use tidebreak_core::{CodeGrantId, Event, OwnerId, SessionId};

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code::bus::{CodeLiveUpdate, LiveTail};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::routes::events::{gateway_auth_revalidation_timer, wait_for_gateway_auth_revalidation};
use crate::state::AppState;

use super::types::{SequencedEventFrame, SessionEventsQuery};

/// Events one journal window reads when the caller names no limit.
pub const DEFAULT_JOURNAL_WINDOW: u64 = 400;

/// Query of `GET /sessions/{id}/journal`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionJournalQuery {
    /// Read the events whose sequence number is below this one.
    pub before: i64,
    /// Most events to read, the newest below `before`. From 1 to
    /// `MAX_REPLAY_EVENTS`; 400 when absent.
    #[serde(default)]
    pub limit: Option<u64>,
}

/// `GET /sessions/{id}/journal?before=&limit=` — one window of a session's
/// journal, oldest first, for a reader opening a part of a long session the
/// event socket no longer replays: a search hit on an event older than its
/// last `MAX_REPLAY_EVENTS`.
///
/// The frames are the ones the socket replays, and the same reader may read
/// them: the owner, or someone the session is shared with. The first frame
/// carries `truncated` when older events were left out of the window.
pub async fn session_journal(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Query(query): Query<SessionJournalQuery>,
) -> Result<Json<Vec<SequencedEventFrame>>, ServerError> {
    if query.before < 1 {
        return Err(ServerError::bad_request(
            "before must be a positive journal sequence number",
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_JOURNAL_WINDOW);
    if !(1..=MAX_REPLAY_EVENTS).contains(&limit) {
        return Err(ServerError::bad_request(format!(
            "limit must be between 1 and {MAX_REPLAY_EVENTS}"
        )));
    }
    let principal = code.owner().clone();
    let (owner, is_owner) = code.event_stream_access(id).await?;
    let granted = (!is_owner).then_some(principal);
    let Some(runtime) = state.code.clone() else {
        return Err(ServerError::not_found(format!("session {id} not found")));
    };
    let page = list_events_before(&runtime.db, &owner, id, query.before, limit).await?;
    let mut truncated = page.truncated;
    let mut frames = Vec::with_capacity(page.events.len());
    for event in page.events {
        frames.push(SequencedEventFrame {
            seq: event.seq,
            event: crate::code::session_tree::authorize_event(
                &runtime.db,
                &owner,
                id,
                None,
                granted.as_ref(),
                event.event,
            )
            .await,
            replayed: Some(true),
            transient: None,
            replacement: None,
            truncated: truncated.then(|| {
                truncated = false;
                true
            }),
        });
    }
    Ok(Json(frames))
}

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
    Adapter {
        grant_id: CodeGrantId,
    },
}

impl Viewer {
    fn adapter_grant(&self) -> Option<CodeGrantId> {
        match self {
            Self::Adapter { grant_id } => Some(*grant_id),
            Self::Owner | Self::Granted(_) => None,
        }
    }
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
    // Use the reader's channel for access changes. Owner and adapter sockets
    // also need parent digests: replacing a wait can move its deadline without
    // changing the tree payload that the journal deduplicates.
    let granted = match &viewer {
        Viewer::Granted(principal) => Some(principal.clone()),
        Viewer::Owner | Viewer::Adapter { .. } => None,
    };
    let mut tree_notices = runtime
        .bus
        .subscribe_updates(granted.as_ref().unwrap_or(&owner));
    let mut grant_notices = runtime.grant_revocations().subscribe();
    let (mut live, tail) = runtime.bus.attach(session);
    // Only the desktop's own socket means the owner is looking at the
    // session. The adapter's follower is a renderer, not a viewer: its
    // connects and resyncs must not clear `DoneUnreviewed` for the owner.
    if viewer == Viewer::Owner {
        let _ = runtime.mark_session_viewed(&owner, session).await;
    }
    let mut last_seq = after;
    if replay_after(
        &mut socket,
        &runtime.db,
        &owner,
        session,
        &mut last_seq,
        viewer.adapter_grant(),
        granted.as_ref(),
    )
    .await
    .is_err()
    {
        return;
    }
    if !reader_still_authorized(&runtime.db, granted.as_ref(), session).await {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    if send_live_tail(&mut socket, &tail, last_seq).await.is_err() {
        return;
    }
    // A resumed cursor can be at the journal tail while its cached tree is
    // stale. Restate current state even when replay has no rows to send.
    let Ok(mut tree_deadline) = send_current_tree(
        &mut socket,
        &runtime.db,
        &owner,
        session,
        last_seq,
        viewer.adapter_grant(),
        granted.as_ref(),
    )
    .await
    else {
        return;
    };
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            _ = next_tree_change(&mut tree_notices, session, tree_deadline.is_some()) => {
                let Ok(deadline) = send_current_tree(
                    &mut socket, &runtime.db, &owner, session, last_seq,
                    viewer.adapter_grant(), granted.as_ref(),
                ).await else { break; };
                tree_deadline = deadline;
            },
            () = wait_for_tree_deadline(tree_deadline) => {
                let Ok(deadline) = send_current_tree(
                    &mut socket, &runtime.db, &owner, session, last_seq,
                    viewer.adapter_grant(), granted.as_ref(),
                ).await else { break; };
                tree_deadline = deadline;
            },
            notice = grant_notices.recv(), if granted.is_some() => {
                if matches!(notice, Err(RecvError::Closed)) {
                    break;
                }
                if !reader_still_authorized(&runtime.db, granted.as_ref(), session).await {
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
                    // Revocation may race with an already queued frame or notice.
                    if !reader_still_authorized(&runtime.db, granted.as_ref(), session).await {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
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
                        if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq, viewer.adapter_grant(), granted.as_ref())
                            .await
                            .is_err()
                        {
                            break;
                        }
                        let Ok(deadline) = send_current_tree(
                            &mut socket, &runtime.db, &owner, session, last_seq,
                            viewer.adapter_grant(), granted.as_ref(),
                        ).await else { break; };
                        tree_deadline = deadline;
                        continue;
                    }
                    last_seq = seq;
                    let event = crate::code::session_tree::authorize_event(
                        &runtime.db, &owner, session, viewer.adapter_grant(), granted.as_ref(), event.event,
                    ).await;
                    if matches!(&event, Event::SessionTree { .. }) {
                        tree_deadline = current_tree_deadline(&runtime.db, &owner, session, &event).await;
                    }
                    if send_frame(
                        &mut socket,
                        &SequencedEventFrame {
                            seq,
                            event,
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
                    if replay_after(&mut socket, &runtime.db, &owner, session, &mut last_seq, viewer.adapter_grant(), granted.as_ref())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let Ok(deadline) = send_current_tree(
                        &mut socket, &runtime.db, &owner, session, last_seq,
                        viewer.adapter_grant(), granted.as_ref(),
                    ).await else { break; };
                    tree_deadline = deadline;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Send a current tree without changing the durable replay cursor.
async fn send_current_tree(
    socket: &mut WebSocket,
    store: &tidebreak_core::DbStore,
    owner: &OwnerId,
    session: SessionId,
    last_seq: i64,
    grant_id: Option<CodeGrantId>,
    principal: Option<&OwnerId>,
) -> Result<Option<Instant>, ()> {
    if !reader_still_authorized(store, principal, session).await {
        let _ = socket.send(Message::Close(None)).await;
        return Err(());
    }
    let event = crate::code::session_tree::authorize_event(
        store,
        owner,
        session,
        grant_id,
        principal,
        Event::SessionTree {
            children: Vec::new(),
            wait: None,
        },
    )
    .await;
    let deadline = current_tree_deadline(store, owner, session, &event).await;
    send_frame(
        socket,
        &SequencedEventFrame {
            seq: last_seq,
            event,
            replayed: None,
            transient: Some(true),
            replacement: None,
            truncated: None,
        },
    )
    .await
    .map_err(|_| ())?;
    Ok(deadline)
}

/// Arm only a visible wait. A replacement lease can move this deadline;
/// an expired lease that races the read gets an immediate refresh.
async fn current_tree_deadline(
    store: &tidebreak_core::DbStore,
    owner: &OwnerId,
    session: SessionId,
    event: &Event,
) -> Option<Instant> {
    if !matches!(event, Event::SessionTree { wait: Some(_), .. }) {
        return None;
    }
    Some(wait_refresh_deadline(store, owner, session).await)
}

pub(super) async fn wait_refresh_deadline(
    store: &tidebreak_core::DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Instant {
    let delay = match tidebreak_core::db::code::parent_wait_deadline(store, owner, session).await {
        Ok(deadline) => deadline
            .map(|deadline| (deadline - chrono::Utc::now()).to_std().unwrap_or_default())
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(session = %session, %error, "could not read the parent wait deadline");
            std::time::Duration::from_secs(1)
        }
    };
    Instant::now() + delay
}

pub(super) async fn wait_for_tree_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Recheck child access and waits whose payload stays the same across leases.
async fn next_tree_change(
    notices: &mut broadcast::Receiver<CodeLiveUpdate>,
    session: SessionId,
    active_wait: bool,
) {
    loop {
        match notices.recv().await {
            Ok(CodeLiveUpdate::AccessChanged(_)) => return,
            Ok(CodeLiveUpdate::Digest(digest))
                if digest.session == session && (active_wait || digest.wait.is_some()) =>
            {
                return;
            }
            Ok(_) => continue,
            Err(RecvError::Lagged(_)) => return,
            // The bus outlives the process; closure is not a revocation.
            Err(RecvError::Closed) => return std::future::pending().await,
        }
    }
}

/// Revalidate the reader before sending each replay or live frame.
pub(super) async fn reader_still_authorized(
    store: &tidebreak_core::DbStore,
    principal: Option<&OwnerId>,
    session: SessionId,
) -> bool {
    let Some(principal) = principal else {
        return true;
    };
    tidebreak_core::db::code::resolve_session_access(store, principal, session)
        .await
        .ok()
        .flatten()
        .is_some()
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
    grant_id: Option<CodeGrantId>,
    principal: Option<&OwnerId>,
) -> Result<(), ()> {
    if !reader_still_authorized(store, principal, session).await {
        let _ = socket.send(Message::Close(None)).await;
        return Err(());
    }
    let page = list_events(store, owner, session, *last_seq, MAX_REPLAY_EVENTS)
        .await
        .map_err(|_| ())?;
    let mut truncated = page.truncated;
    for event in page.events {
        if !reader_still_authorized(store, principal, session).await {
            let _ = socket.send(Message::Close(None)).await;
            return Err(());
        }
        *last_seq = event.seq;
        send_frame(
            socket,
            &SequencedEventFrame {
                seq: event.seq,
                event: crate::code::session_tree::authorize_event(
                    store,
                    owner,
                    session,
                    grant_id,
                    principal,
                    event.event,
                )
                .await,
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
    let json = serde_json::to_string(frame).map_err(axum::Error::new)?;
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
