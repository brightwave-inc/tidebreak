//! Shared event-socket follow: subscribe, reconnect, durable fallback.
//!
//! Print mode and `agent-mcp` watch a chat turn over `/chats/{id}/events`.
//! Code-mode tools watch `/sessions/{id}/events`. The reconnect ladder
//! lives here so those surfaces stay in lockstep; each caller decides what a
//! frame *means*.
//!
//! A newer server can send a frame this build cannot read, such as an event
//! type added after it. Following the turn matters more than that one frame,
//! so every reader here skips it, but none skips it silently: the first skip
//! is reported on stderr as it happens, and a stream that skipped more than one
//! reports the total when it ends. A skipped frame that still carries its
//! sequence moves the cursor past it, so a reconnect does not replay it.

use futures::StreamExt as _;
use tidebreak_core::{AgentError, Result, SessionId, TurnId};
use tokio_tungstenite::tungstenite::Message;

use crate::api::client::{Client, DurableTurn, EventSocket};
use crate::api::code::{
    decode_event_frame, decode_update_notice, SequencedEventFrame, UpdateNotice,
};
use crate::api::wire::{RendererAgentEvent, RendererChatFrame};

/// Frames a stream received and could not read.
#[derive(Debug, Default)]
pub(crate) struct SkippedFrames {
    count: u64,
}

impl SkippedFrames {
    /// Count one unreadable frame, and return the notice to print for it: one
    /// for the first skip, so a skipped stream of deltas does not flood stderr.
    pub(crate) fn skip(&mut self, raw: &str) -> Option<String> {
        self.count += 1;
        (self.count == 1).then(|| {
            format!(
                "tidebreak: skipped {} this version of Tidebreak cannot read; update Tidebreak to see it",
                describe_frame(raw)
            )
        })
    }

    /// How many frames were skipped so far.
    #[cfg(test)]
    fn count(&self) -> u64 {
        self.count
    }

    /// The closing report, when more frames were skipped than the first
    /// notice named.
    pub(crate) fn summary(&self) -> Option<String> {
        (self.count > 1).then(|| {
            format!(
                "tidebreak: skipped {} frames this version of Tidebreak cannot read",
                self.count
            )
        })
    }

    /// Count a skip and print its notice, if it has one.
    fn report(&mut self, raw: &str) {
        if let Some(notice) = self.skip(raw) {
            eprintln!("{notice}");
        }
    }
}

impl Drop for SkippedFrames {
    fn drop(&mut self) {
        if let Some(summary) = self.summary() {
            eprintln!("{summary}");
        }
    }
}

/// Name an unreadable frame by its type, when it states one this terminal
/// can print safely.
fn describe_frame(raw: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok();
    let kind = value
        .as_ref()
        .and_then(|value| {
            value
                .get("event")
                .and_then(|event| event.get("type"))
                .or_else(|| value.get("type"))
                .or_else(|| value.get("metadata"))
        })
        .and_then(serde_json::Value::as_str)
        .filter(|kind| {
            !kind.is_empty()
                && kind.len() <= 64
                && kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        });
    match kind {
        Some(kind) => format!("a `{kind}` frame"),
        None => "a frame".to_owned(),
    }
}

/// The sequence an unreadable frame states, if it states one.
fn raw_seq(raw: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()?
        .get("seq")?
        .as_i64()
}

/// The sequence and parsed body of a frame that states it is a
/// `turn_failed`, when this build could not read the rest of it.
///
/// A failure is the frame a follower cannot do without: skipping it leaves
/// the turn open on an open socket, and print mode waits for it forever. So
/// a failure this build cannot read in full still ends the turn, as a
/// failure of unknown cause.
fn unreadable_turn_failure(raw: &str) -> Option<(i64, serde_json::Value)> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let seq = value.get("seq")?.as_i64()?;
    (value.pointer("/event/type")?.as_str()? == "turn_failed").then_some((seq, value))
}

/// What one text frame on a chat socket turned out to be.
pub(crate) enum ChatRead {
    /// A journaled turn event.
    Event(Box<RendererAgentEvent>),
    /// A frame that is not a turn event, such as chat metadata.
    Other,
    /// A frame this build could not read. It was counted and reported.
    Skipped,
}

/// Reads chat frames: keeps the resume cursor and counts what it skips.
#[derive(Debug, Default)]
pub(crate) struct ChatFrames {
    last_seq: i64,
    skipped: SkippedFrames,
}

impl ChatFrames {
    fn after(after: i64) -> Self {
        Self {
            last_seq: after,
            skipped: SkippedFrames::default(),
        }
    }

    pub(crate) fn read(&mut self, text: &str) -> ChatRead {
        match serde_json::from_str::<RendererChatFrame>(text) {
            Ok(RendererChatFrame::Event(frame)) => {
                self.last_seq = frame.seq;
                ChatRead::Event(Box::new(frame.event))
            }
            Ok(RendererChatFrame::Metadata(_)) => ChatRead::Other,
            Err(_) if unreadable_turn_failure(text).is_some() => {
                let (seq, value) = unreadable_turn_failure(text).expect("checked above");
                self.last_seq = self.last_seq.max(seq);
                ChatRead::Event(Box::new(RendererAgentEvent::TurnFailed {
                    category: tidebreak_core::TurnFailureCategory::Unknown,
                    failure: None,
                    detail: value
                        .pointer("/event/detail")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    model: None,
                }))
            }
            Err(_) => {
                self.skipped.report(text);
                if let Some(seq) = raw_seq(text) {
                    self.last_seq = self.last_seq.max(seq);
                }
                ChatRead::Skipped
            }
        }
    }

    /// Advance the cursor for a frame a caller passes on raw, whether or not
    /// this build can read it.
    fn observe(&mut self, text: &str) {
        match serde_json::from_str::<RendererChatFrame>(text) {
            Ok(RendererChatFrame::Event(frame)) => self.last_seq = frame.seq,
            Ok(RendererChatFrame::Metadata(_)) => {}
            Err(_) => {
                if let Some(seq) = raw_seq(text) {
                    self.last_seq = self.last_seq.max(seq);
                }
            }
        }
    }

    pub(crate) fn last_seq(&self) -> i64 {
        self.last_seq
    }

    #[cfg(test)]
    fn skipped(&self) -> u64 {
        self.skipped.count()
    }
}

/// Reads code-session frames: keeps the resume cursor and counts what it
/// skips.
#[derive(Debug, Default)]
pub(crate) struct CodeFrames {
    last_seq: i64,
    skipped: SkippedFrames,
}

impl CodeFrames {
    pub(crate) fn after(after: i64) -> Self {
        Self {
            last_seq: after,
            skipped: SkippedFrames::default(),
        }
    }

    /// The frame, or `None` when this build could not read it. A skipped
    /// frame is counted and reported, and still moves the cursor when it
    /// states its sequence.
    pub(crate) fn read(&mut self, text: &str) -> Option<SequencedEventFrame> {
        match decode_event_frame(text) {
            Ok(frame) => {
                self.last_seq = frame.seq;
                Some(frame)
            }
            Err(_) if unreadable_turn_failure(text).is_some() => {
                let (seq, value) = unreadable_turn_failure(text).expect("checked above");
                self.last_seq = self.last_seq.max(seq);
                let message = value
                    .pointer("/event/error/message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the turn failed; this version of Tidebreak cannot read why");
                Some(SequencedEventFrame {
                    seq,
                    event: tidebreak_core::Event::TurnFailed {
                        error: tidebreak_core::BoundedError::new(message),
                        detail: None,
                    },
                    replayed: None,
                    transient: None,
                    replacement: None,
                    truncated: None,
                })
            }
            Err(_) => {
                self.skipped.report(text);
                if let Some(seq) = raw_seq(text) {
                    self.last_seq = self.last_seq.max(seq);
                }
                None
            }
        }
    }

    pub(crate) fn last_seq(&self) -> i64 {
        self.last_seq
    }

    #[cfg(test)]
    fn skipped(&self) -> u64 {
        self.skipped.count()
    }
}

/// Reads `/updates` notices and counts what it skips. Notices carry no
/// sequence, so there is no cursor to move.
#[derive(Debug, Default)]
pub(crate) struct UpdateNotices {
    skipped: SkippedFrames,
}

impl UpdateNotices {
    /// The notice, or `None` when this build could not read it.
    pub(crate) fn read(&mut self, text: &str) -> Option<UpdateNotice> {
        match decode_update_notice(text) {
            Ok(notice) => Some(notice),
            Err(_) => {
                self.skipped.report(text);
                None
            }
        }
    }

    #[cfg(test)]
    fn skipped(&self) -> u64 {
        self.skipped.count()
    }
}

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
    frames: ChatFrames,
}

pub(crate) enum StreamNext {
    /// Boxed: an event carries tool previews and dwarfs the other arms.
    Frame(String, Box<RendererAgentEvent>),
    Durable(DurableTurn),
    Ignore,
}

impl EventStream {
    pub(crate) async fn open(client: &Client, chat: SessionId) -> Result<Self> {
        Self::open_after(client, chat, 0).await
    }

    pub(crate) async fn open_after(client: &Client, chat: SessionId, after: i64) -> Result<Self> {
        Ok(Self {
            socket: client.open_events(chat, after).await?,
            frames: ChatFrames::after(after),
        })
    }

    pub(crate) fn last_seq(&self) -> i64 {
        self.frames.last_seq()
    }

    /// One journaled frame, or `None` if the socket closed or produced a
    /// non-text payload. Does not reconnect — callers that are draining a
    /// replay (not following a live turn) stop here rather than climbing the
    /// reconnect ladder. The frame is passed on raw, so one this build cannot
    /// read is not skipped here; it still moves the cursor.
    pub(crate) async fn recv(&mut self) -> Option<String> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => {
                self.frames.observe(&text);
                Some(text.to_string())
            }
            _ => None,
        }
    }

    /// The next journaled frame, a durable terminal fallback, or an ignorable
    /// payload such as metadata, a ping, or a frame this build cannot read
    /// (counted and reported, not dropped silently).
    pub(crate) async fn next(
        &mut self,
        client: &mut Client,
        chat: SessionId,
        turn_id: TurnId,
    ) -> Result<StreamNext> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => match self.frames.read(&text) {
                ChatRead::Event(event) => Ok(StreamNext::Frame(text.to_string(), event)),
                ChatRead::Other | ChatRead::Skipped => Ok(StreamNext::Ignore),
            },
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
        chat: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<DurableTurn>> {
        let mut last = None;
        for delay in RECONNECT_DELAYS {
            tokio::time::sleep(delay).await;
            if let Err(error) = client.refresh_attach_endpoint().await {
                last = Some(error);
                continue;
            }
            match client.open_events(chat, self.frames.last_seq()).await {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(None);
                }
                Err(error) => last = Some(error),
            }
        }
        if client.refresh_attach_endpoint().await.is_ok() {
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

/// The code-mode session event socket plus the cursor a reconnect resumes from.
pub(crate) struct CodeEventStream {
    socket: EventSocket,
    frames: CodeFrames,
}

pub(crate) enum CodeStreamNext {
    Frame(Box<SequencedEventFrame>),
    Ignore,
}

impl CodeEventStream {
    pub(crate) async fn open(client: &Client, session: SessionId) -> Result<Self> {
        Self::open_after(client, session, 0).await
    }

    pub(crate) async fn open_after(
        client: &Client,
        session: SessionId,
        after: i64,
    ) -> Result<Self> {
        Ok(Self {
            socket: client.open_code_events(session, after).await?,
            frames: CodeFrames::after(after),
        })
    }

    pub(crate) fn last_seq(&self) -> i64 {
        self.frames.last_seq()
    }

    /// The next sequenced frame, or an ignorable payload. A frame this build
    /// cannot read is counted and reported. A closed socket climbs
    /// [`RECONNECT_DELAYS`] and resumes from [`Self::last_seq`]. The caller
    /// reconciles via `list_session_turns` when this returns an error.
    pub(crate) async fn next(
        &mut self,
        client: &mut Client,
        session: SessionId,
    ) -> Result<CodeStreamNext> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => match self.frames.read(&text) {
                Some(frame) => Ok(CodeStreamNext::Frame(Box::new(frame))),
                None => Ok(CodeStreamNext::Ignore),
            },
            Some(Ok(_)) => Ok(CodeStreamNext::Ignore),
            Some(Err(_)) | None => {
                self.reconnect(client, session).await?;
                Ok(CodeStreamNext::Ignore)
            }
        }
    }

    async fn reconnect(&mut self, client: &mut Client, session: SessionId) -> Result<()> {
        let mut last = None;
        for delay in RECONNECT_DELAYS {
            tokio::time::sleep(delay).await;
            if let Err(error) = client.refresh_attach_endpoint().await {
                last = Some(error);
                continue;
            }
            match client
                .open_code_events(session, self.frames.last_seq())
                .await
            {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::msg(format!(
            "the code event stream closed mid-turn and could not be reopened{}",
            last.map(|error| format!(": {error}")).unwrap_or_default()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: &str = r#"{"seq":6,"event":{"type":"text_delta","text":"hi"}}"#;
    const FUTURE: &str = r#"{"seq":5,"event":{"type":"some_future_event","detail":1}}"#;

    /// The bug this replaces: an unreadable frame vanished with no trace, and
    /// a reconnect replayed it because the cursor never moved past it.
    #[test]
    fn a_chat_frame_this_build_cannot_read_is_counted_and_passed() {
        let mut frames = ChatFrames::after(4);
        assert!(matches!(frames.read(FUTURE), ChatRead::Skipped));
        assert_eq!(frames.skipped(), 1);
        assert_eq!(
            frames.last_seq(),
            5,
            "the cursor moves past a skipped frame"
        );

        assert!(matches!(
            frames.read(KNOWN),
            ChatRead::Event(event) if matches!(*event, RendererAgentEvent::TextDelta { .. })
        ));
        assert_eq!(frames.last_seq(), 6);
        assert_eq!(frames.skipped(), 1);

        // Metadata is not an event, and not a skip either.
        assert!(matches!(
            frames.read(r#"{"metadata":"titled","title":"A chat"}"#),
            ChatRead::Other
        ));
        assert_eq!(frames.skipped(), 1);
    }

    /// A frame passed on raw is not a skip, but it still moves the cursor, so
    /// a drain that stops at the journal's end does not wait on it.
    #[test]
    fn a_raw_chat_frame_moves_the_cursor_either_way() {
        let mut frames = ChatFrames::after(0);
        frames.observe(FUTURE);
        assert_eq!(frames.last_seq(), 5);
        frames.observe(KNOWN);
        assert_eq!(frames.last_seq(), 6);
        assert_eq!(frames.skipped(), 0);
    }

    /// A failure from a newer server reads: a category or engine this build
    /// does not know degrades inside the frame, and a failure it cannot read
    /// at all still ends the turn instead of leaving print mode waiting.
    #[test]
    fn a_failure_this_build_cannot_fully_read_still_ends_the_turn() {
        let mut frames = ChatFrames::after(0);
        let newer = r#"{"seq":7,"event":{"type":"turn_failed","category":"unknown","failure":{"category":"quota_exceeded","engine":"an_engine_from_next_year"}}}"#;
        match frames.read(newer) {
            ChatRead::Event(event) => match *event {
                RendererAgentEvent::TurnFailed { failure, .. } => assert_eq!(
                    failure,
                    Some(tidebreak_core::TurnFailure::new(
                        tidebreak_core::TurnFailureCategory::Unknown
                    ))
                ),
                other => panic!("{other:?}"),
            },
            _ => panic!("a newer failure must read"),
        }
        let unreadable = r#"{"seq":8,"event":{"type":"turn_failed","category":7,"detail":"anthropic returned 529"}}"#;
        match frames.read(unreadable) {
            ChatRead::Event(event) => match *event {
                RendererAgentEvent::TurnFailed {
                    category, detail, ..
                } => {
                    assert_eq!(category, tidebreak_core::TurnFailureCategory::Unknown);
                    assert_eq!(detail.as_deref(), Some("anthropic returned 529"));
                }
                other => panic!("{other:?}"),
            },
            _ => panic!("an unreadable failure must still end the turn"),
        }
        assert_eq!((frames.skipped(), frames.last_seq()), (0, 8));

        let mut code = CodeFrames::after(0);
        let newer = r#"{"seq":3,"event":{"type":"turn_failed","error":{"message":"limit","failure":{"category":"quota_exceeded","engine":"an_engine_from_next_year"}}}}"#;
        assert!(matches!(
            code.read(newer).map(|frame| frame.event),
            Some(tidebreak_core::Event::TurnFailed { error, .. })
                if error.category() == Some(tidebreak_core::TurnFailureCategory::Unknown)
        ));
        let unreadable = r#"{"seq":4,"event":{"type":"turn_failed","error":{"message":"boom","failure":{"category":7}}}}"#;
        assert!(matches!(
            code.read(unreadable).map(|frame| frame.event),
            Some(tidebreak_core::Event::TurnFailed { error, .. }) if error.message == "boom"
        ));
        let retrying = r#"{"seq":5,"event":{"type":"turn_retrying","category":"quota_exceeded","attempt":2,"max_attempts":5,"retry_at":"2026-08-20T15:05:54Z"}}"#;
        assert!(matches!(
            code.read(retrying).map(|frame| frame.event),
            Some(tidebreak_core::Event::TurnRetrying { category, .. })
                if category == tidebreak_core::TurnFailureCategory::Unknown
        ));
        assert_eq!((code.skipped(), code.last_seq()), (0, 5));
    }

    #[test]
    fn code_frames_and_notices_count_what_they_skip() {
        let mut frames = CodeFrames::after(0);
        assert!(frames
            .read(r#"{"seq":3,"event":{"type":"some_future_event"}}"#)
            .is_none());
        assert_eq!((frames.skipped(), frames.last_seq()), (1, 3));
        assert!(frames
            .read(r#"{"seq":4,"event":{"type":"turn_interrupted"}}"#)
            .is_some());
        assert_eq!((frames.skipped(), frames.last_seq()), (1, 4));

        let mut notices = UpdateNotices::default();
        assert!(notices.read(r#"{"type":"some_future_notice"}"#).is_none());
        assert!(notices
            .read(r#"{"type":"snapshot","sessions":[]}"#)
            .is_some());
        assert_eq!(notices.skipped(), 1);
    }

    /// One notice for the first skip, naming the frame, and a total at the end
    /// only when there was more than one.
    #[test]
    fn skips_are_reported_once_and_then_totalled() {
        let mut skipped = SkippedFrames::default();
        assert_eq!(
            skipped.skip(FUTURE).as_deref(),
            Some(
                "tidebreak: skipped a `some_future_event` frame this version of Tidebreak \
                 cannot read; update Tidebreak to see it"
            )
        );
        assert_eq!(skipped.summary(), None);
        assert_eq!(skipped.skip(FUTURE), None);
        assert_eq!(
            skipped.summary().as_deref(),
            Some("tidebreak: skipped 2 frames this version of Tidebreak cannot read")
        );
        // Nothing but a plain type name reaches the terminal.
        assert_eq!(
            describe_frame(r#"{"seq":1,"event":{"type":"\u001b[31mred"}}"#),
            "a frame"
        );
        assert_eq!(describe_frame("not json"), "a frame");
        assert_eq!(
            describe_frame(r#"{"type":"future_notice"}"#),
            "a `future_notice` frame"
        );
    }
}
