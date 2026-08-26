//! Shared chat event-socket follow: subscribe, reconnect, durable fallback.
//!
//! Print mode and `agent-mcp` both watch one turn over `/chats/{id}/events`.
//! The reconnect ladder and the durable-transcript fallback live here so those
//! surfaces stay in lockstep; each caller decides what a frame *means*.

use futures::StreamExt as _;
use tidebreak_core::{AgentError, ChatId, Result, TurnId};
use tokio_tungstenite::tungstenite::Message;

use crate::api::client::{Client, DurableTurn, EventSocket};
use crate::api::wire::{ChatFrame, ClientEvent};

/// Attempts to re-open the event socket after it closes mid-turn before giving
/// up. The retries cover a transient hiccup — an accept-loop stumble when the
/// server is in-process, a dropped connection when it is not.
pub(crate) const RECONNECT_DELAYS: [std::time::Duration; 8] = [
    std::time::Duration::from_millis(250),
    std::time::Duration::from_millis(500),
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(4),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(6),
    std::time::Duration::from_secs(6),
];

/// The event socket plus the cursor a reconnect resumes from.
pub(crate) struct EventStream {
    socket: EventSocket,
    last_seq: i64,
}

pub(crate) enum StreamNext {
    Frame(String, ClientEvent),
    Durable(DurableTurn),
    Ignore,
}

impl EventStream {
    pub(crate) async fn open(client: &Client, chat: ChatId) -> Result<Self> {
        Self::open_after(client, chat, 0).await
    }

    pub(crate) async fn open_after(client: &Client, chat: ChatId, after: i64) -> Result<Self> {
        Ok(Self {
            socket: client.open_events(chat, after).await?,
            last_seq: after,
        })
    }

    pub(crate) fn last_seq(&self) -> i64 {
        self.last_seq
    }

    /// One journaled frame, or `None` if the socket closed or produced a
    /// non-text payload. Does not reconnect — callers that are draining a
    /// replay (not following a live turn) stop here rather than climbing the
    /// reconnect ladder.
    pub(crate) async fn recv(&mut self) -> Option<String> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(ChatFrame::Event(frame)) = serde_json::from_str::<ChatFrame>(&text) {
                    self.last_seq = frame.seq;
                }
                Some(text.to_string())
            }
            _ => None,
        }
    }

    /// The next journaled frame, a durable terminal fallback, or an ignorable
    /// payload such as metadata, a ping, or undecodable text.
    pub(crate) async fn next(
        &mut self,
        client: &mut Client,
        chat: ChatId,
        turn_id: TurnId,
    ) -> Result<StreamNext> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let Ok(ChatFrame::Event(frame)) = serde_json::from_str::<ChatFrame>(&text) else {
                    return Ok(StreamNext::Ignore);
                };
                self.last_seq = frame.seq;
                Ok(StreamNext::Frame(text.to_string(), frame.event))
            }
            Some(Ok(_)) => Ok(StreamNext::Ignore),
            Some(Err(_)) | None => match self.reconnect(client, chat, turn_id).await? {
                Some(turn) => Ok(StreamNext::Durable(turn)),
                None => Ok(StreamNext::Ignore),
            },
        }
    }

    async fn reconnect(
        &mut self,
        client: &mut Client,
        chat: ChatId,
        turn_id: TurnId,
    ) -> Result<Option<DurableTurn>> {
        let mut last = None;
        for delay in RECONNECT_DELAYS {
            tokio::time::sleep(delay).await;
            if let Err(error) = client.refresh_attach_endpoint() {
                last = Some(error);
                continue;
            }
            match client.open_events(chat, self.last_seq).await {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(None);
                }
                Err(error) => last = Some(error),
            }
        }
        if client.refresh_attach_endpoint().is_ok() {
            match client.durable_turn(chat, turn_id).await {
                Ok(Some(turn)) => return Ok(Some(turn)),
                Ok(None) => {}
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::msg(format!(
            "the event stream closed mid-turn and could not be reopened{}",
            last.map(|error| format!(": {error}")).unwrap_or_default()
        )))
    }
}
