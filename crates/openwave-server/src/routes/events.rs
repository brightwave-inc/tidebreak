//! Route handlers extracted from the parent `routes` module.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use openwave_core::{AgentEvent, ChatId, SequencedEvent, Store};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::event_projection::{RendererChatFrame, RendererChatMetadata, RendererSequencedEvent};
use crate::extract::{Path, Query};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// Query for `GET /chats/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Resume after this journal sequence number; `0` (the default) replays from
    /// the start.
    #[serde(default)]
    pub after: i64,
}

/// `GET /chats/{id}/events` (WebSocket) — stream a chat's turn events.
///
/// On connect the client gets **snapshot → replay → live**: journaled events with
/// `seq > after` are replayed, then live events stream as they occur. Subscribing
/// to the live tail *before* replaying, dropping any live event whose `seq` was
/// already replayed, and replaying whenever the live cursor jumps means nothing
/// is missed or duplicated across the handoff or a worker-ownership race. `404`
/// if the chat doesn't exist.
///
/// Auth is checked by the bearer middleware. Browser clients authenticate via
/// `Sec-WebSocket-Protocol` (`openwave-token.<token>`). When the client offered
/// `openwave-v1`, this handler selects it in the upgrade response so the
/// browser accepts the handshake (RFC 6455).
pub async fn chat_events(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let chat = store.require_chat(id).await?;
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after, chat.title)))
}

/// Serve one client's event stream for `chat`: replay from the journal, then live.
async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    chat: ChatId,
    after: i64,
    title: Option<String>,
) {
    // Subscribe before replaying, so an event emitted during replay is buffered on
    // the live channel rather than lost in the gap between the two.
    let mut live = state.events.subscribe(chat);
    // Metadata rides the same socket but not the same order: it has no sequence
    // and nothing replays it.
    let mut metadata = state.events.subscribe_metadata(chat);

    // The name is state rather than an event, so the socket opens by stating it
    // and every reconnect restates it. Nothing retains a notice for a client that
    // was not listening yet, and the common case is exactly that: a new chat's
    // first turn can name it before the renderer finishes connecting. A client
    // that already knows this name does nothing with it.
    if let Some(title) = title {
        if send_frame(
            &mut socket,
            &RendererChatFrame::Metadata(RendererChatMetadata::Titled { title }),
        )
        .await
        .is_err()
        {
            return;
        }
    }

    // Replay everything the client hasn't seen yet from the durable journal.
    let mut last_seq = after;
    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            // Watch the socket so a client disconnect ends the task promptly.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            notice = metadata.recv() => match notice {
                Ok(notice) => {
                    let frame = RendererChatFrame::Metadata((&notice).into());
                    if send_frame(&mut socket, &frame).await.is_err() {
                        break;
                    }
                }
                // Nothing to catch up on: the durable value is what a fresh read
                // returns, so a dropped notice costs a client nothing but motion.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            live_event = live.recv() => match live_event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue; // already covered by replay
                    }
                    if event.seq > last_seq.saturating_add(1) {
                        // Durable commits can be published out of order across
                        // lease owners. Fill the gap from the journal before
                        // accepting this live tail; replay includes the current
                        // event because publication always follows commit.
                        if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = event.seq;
                    let model = state
                        .store
                        .list_turn_runs(chat)
                        .await
                        .ok()
                        .and_then(|turns| turns.into_iter().find(|turn| !turn.status.is_terminal()))
                        .map(|turn| turn.model);
                    if send_event(&mut socket, &event, model.as_deref()).await.is_err() {
                        break;
                    }
                }
                // Fell behind the live buffer. Rather than drop the client, catch
                // up from the journal (durable truth) and resume live — the seq
                // dedup above absorbs any overlap. A long/fast turn can outrun the
                // 256-slot buffer, so this keeps an ordinary client connected.
                Err(RecvError::Lagged(_)) => {
                    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
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

/// Send journaled events with `seq > *last_seq` to the socket, advancing
/// `*last_seq`. `Err(())` means the connection should end (send or store failure).
async fn replay_after(
    socket: &mut WebSocket,
    store: &dyn Store,
    chat: ChatId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = store.list_events(chat, *last_seq).await.map_err(|_| ())?;
    let turn_models = store
        .list_turn_runs(chat)
        .await
        .map_err(|_| ())?
        .into_iter()
        .map(|turn| (turn.id, turn.model))
        .collect::<std::collections::HashMap<_, _>>();
    let mut active_turn_id = None;
    for event in events {
        *last_seq = event.seq;
        if let AgentEvent::TurnStarted { turn_id } = &event.event {
            active_turn_id = Some(*turn_id);
        }
        let model = active_turn_id.and_then(|turn_id| turn_models.get(&turn_id));
        send_event(socket, &event, model.map(String::as_str))
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

/// Send one journaled event as a frame.
async fn send_event(
    socket: &mut WebSocket,
    event: &SequencedEvent,
    model: Option<&str>,
) -> Result<(), axum::Error> {
    send_frame(
        socket,
        &RendererChatFrame::Event(Box::new(
            RendererSequencedEvent::from(event).with_turn_model(model),
        )),
    )
    .await
}

/// Send one frame as JSON text. A frame that fails to serialize is skipped
/// rather than sent empty (which a client couldn't decode).
async fn send_frame(socket: &mut WebSocket, frame: &RendererChatFrame) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(frame) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
