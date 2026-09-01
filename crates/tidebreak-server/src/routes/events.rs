//! Route handlers extracted from the parent `routes` module.

use std::collections::HashMap;
use std::future::pending;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, Interval, MissedTickBehavior};

use tidebreak_core::{AgentEvent, ChatId, SequencedEvent, Store, TurnId};

use crate::auth::{offered_handshake_subprotocol, GatewayAuthLease, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::event_projection::{RendererChatFrame, RendererChatMetadata, RendererSequencedEvent};
use crate::extract::{Path, Query};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

const GATEWAY_AUTH_REVALIDATION_INTERVAL: Duration = Duration::from_secs(60);

pub(in crate::routes) fn gateway_auth_revalidation_timer(
    lease: Option<&GatewayAuthLease>,
) -> Option<Interval> {
    lease.map(|_| {
        let mut interval = tokio::time::interval_at(
            Instant::now() + GATEWAY_AUTH_REVALIDATION_INTERVAL,
            GATEWAY_AUTH_REVALIDATION_INTERVAL,
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        interval
    })
}

pub(in crate::routes) async fn wait_for_gateway_auth_revalidation(timer: &mut Option<Interval>) {
    match timer {
        Some(timer) => {
            timer.tick().await;
        }
        None => pending::<()>().await,
    }
}

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
/// `Sec-WebSocket-Protocol` (`tidebreak-token.<token>`). When the client offered
/// `tidebreak-v1`, this handler selects it in the upgrade response so the
/// browser accepts the handshake (RFC 6455).
pub async fn chat_events(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    auth_lease: Option<Extension<GatewayAuthLease>>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    store.require_chat(id).await?;
    let auth_lease = auth_lease.map(|Extension(lease)| lease);
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after, auth_lease)))
}

/// Serve one client's event stream for `chat`: replay from the journal, then live.
async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    chat: ChatId,
    after: i64,
    auth_lease: Option<GatewayAuthLease>,
) {
    let mut auth_revalidation = gateway_auth_revalidation_timer(auth_lease.as_ref());
    // Subscribe before replaying, so an event emitted during replay is buffered on
    // the live channel rather than lost in the gap between the two.
    let mut live = state.events.subscribe(chat);
    // Metadata rides the same socket but not the same order: it has no sequence
    // and nothing replays it.
    let mut metadata = state.events.subscribe_metadata(chat);

    // Subscribe before reading the durable name. If titling commits between the
    // route's existence check and this read, the read sees it; if it commits
    // after the read, the subscribed metadata channel sees its notice. The two
    // may both deliver the same title, which clients deliberately deduplicate,
    // but there is no ordering in which both miss it.
    //
    // The name is state rather than an event, so every reconnect restates it.
    // Nothing retains a notice for a client that was not listening yet, and the
    // common case is exactly that: a new chat's first turn can name it before
    // the renderer finishes connecting.
    let title = match state.store.get_chat(chat).await {
        Ok(Some(chat)) => chat.title,
        Ok(None) | Err(_) => return,
    };
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
    let mut turn_models = TurnModelCache::default();
    if replay_after(
        &mut socket,
        &*state.store,
        chat,
        &mut last_seq,
        &mut turn_models,
    )
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
                        if replay_after(
                            &mut socket,
                            &*state.store,
                            chat,
                            &mut last_seq,
                            &mut turn_models,
                        )
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = event.seq;
                    if turn_models.needs_live_refresh(&event.event) {
                        let _ = turn_models.refresh(&*state.store, chat).await;
                    }
                    if send_event(&mut socket, &event, turn_models.active_model(), false).await.is_err() {
                        break;
                    }
                }
                // Fell behind the live buffer. Rather than drop the client, catch
                // up from the journal (durable truth) and resume live — the seq
                // dedup above absorbs any overlap. A long/fast turn can outrun the
                // 256-slot buffer, so this keeps an ordinary client connected.
                Err(RecvError::Lagged(_)) => {
                    if replay_after(
                        &mut socket,
                        &*state.store,
                        chat,
                        &mut last_seq,
                        &mut turn_models,
                    )
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
    turn_models: &mut TurnModelCache,
) -> Result<(), ()> {
    let events = store.list_events(chat, *last_seq).await.map_err(|_| ())?;
    if turn_models.is_empty() {
        turn_models.refresh(store, chat).await?;
    }
    let mut active_turn_id = None;
    for event in events {
        *last_seq = event.seq;
        if let AgentEvent::TurnStarted { turn_id } = &event.event {
            active_turn_id = Some(*turn_id);
        }
        if let Some(turn_id) = active_turn_id {
            if !turn_models.contains(turn_id) {
                turn_models.refresh(store, chat).await?;
            }
        }
        let model = active_turn_id.and_then(|turn_id| turn_models.model_for(turn_id));
        send_event(socket, &event, model, true)
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

/// Turn → model mapping for one WebSocket session. Refreshed on turn
/// boundaries (and on a cache miss), not per streamed delta.
#[derive(Debug, Default)]
struct TurnModelCache {
    by_id: HashMap<TurnId, String>,
    active_model: Option<String>,
}

impl TurnModelCache {
    fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    fn contains(&self, turn_id: TurnId) -> bool {
        self.by_id.contains_key(&turn_id)
    }

    fn model_for(&self, turn_id: TurnId) -> Option<&str> {
        self.by_id.get(&turn_id).map(String::as_str)
    }

    fn active_model(&self) -> Option<&str> {
        self.active_model.as_deref()
    }

    fn needs_live_refresh(&self, event: &AgentEvent) -> bool {
        self.by_id.is_empty() || is_turn_boundary(event)
    }

    fn replace_from_turns(&mut self, turns: impl IntoIterator<Item = (TurnId, String, bool)>) {
        self.by_id.clear();
        self.active_model = None;
        for (id, model, terminal) in turns {
            if self.active_model.is_none() && !terminal {
                self.active_model = Some(model.clone());
            }
            self.by_id.insert(id, model);
        }
    }

    async fn refresh(&mut self, store: &dyn Store, chat: ChatId) -> Result<(), ()> {
        let turns = store.list_turn_runs(chat).await.map_err(|_| ())?;
        self.replace_from_turns(
            turns
                .into_iter()
                .map(|turn| (turn.id, turn.model, turn.status.is_terminal())),
        );
        Ok(())
    }
}

fn is_turn_boundary(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnRefused { .. }
            | AgentEvent::TurnFailed { .. }
            | AgentEvent::TurnCancelled { .. }
    )
}

/// Send one journaled event as a frame.
async fn send_event(
    socket: &mut WebSocket,
    event: &SequencedEvent,
    model: Option<&str>,
    replayed: bool,
) -> Result<(), axum::Error> {
    let projected = RendererSequencedEvent::from(event).with_turn_model(model);
    let projected = if replayed {
        projected.mark_replayed()
    } else {
        projected
    };
    send_frame(socket, &RendererChatFrame::Event(Box::new(projected))).await
}

/// Send one frame as JSON text. A frame that fails to serialize is skipped
/// rather than sent empty (which a client couldn't decode).
async fn send_frame(socket: &mut WebSocket, frame: &RendererChatFrame) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(frame) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::Usage;

    #[test]
    fn deltas_do_not_need_a_refresh_once_the_cache_is_warm() {
        let mut cache = TurnModelCache::default();
        let turn = TurnId::new();
        cache.replace_from_turns([(turn, "model-a".into(), false)]);
        assert!(!cache.needs_live_refresh(&AgentEvent::TextDelta { text: "hi".into() }));
        assert!(!cache.needs_live_refresh(&AgentEvent::ReasoningDelta {
            text: "think".into()
        }));
        assert!(cache.needs_live_refresh(&AgentEvent::TurnStarted { turn_id: turn }));
        assert!(cache.needs_live_refresh(&AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: tidebreak_core::StopReason::EndTurn,
        }));
    }

    #[test]
    fn turn_boundary_refresh_keeps_prior_turn_models() {
        let mut cache = TurnModelCache::default();
        let first = TurnId::new();
        let second = TurnId::new();
        cache.replace_from_turns([(first, "model-a".into(), false)]);
        assert_eq!(cache.active_model(), Some("model-a"));
        assert_eq!(cache.model_for(first), Some("model-a"));

        cache.replace_from_turns([
            (first, "model-a".into(), true),
            (second, "model-b".into(), false),
        ]);
        assert_eq!(cache.model_for(first), Some("model-a"));
        assert_eq!(cache.model_for(second), Some("model-b"));
        assert_eq!(cache.active_model(), Some("model-b"));
    }
}
