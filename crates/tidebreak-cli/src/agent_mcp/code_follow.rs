//! Follow one code-mode turn over the session event socket until it settles.
//!
//! Subscribe before `POST /turns` so a long-lived submit cannot finish in a
//! window this process is not watching. A timeout returns
//! [`TurnStatus::Running`]; a `SubmitTurnResponse::Queued` returns
//! [`TurnStatus::Queued`] without waiting. Reconnect uses
//! [`crate::event_stream::CodeEventStream`]; a socket that cannot be reopened
//! is reconciled through `list_session_turns`.

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tidebreak_core::{
    AgentError, CodeApprovalId, CodeEvent, CodeSessionId, CodeTurnId, CodeTurnStatus, Result,
};

use crate::api::client::Client;
use crate::api::code::{
    is_turn_terminal, CodeApprovalSnapshot, CodeTurnSnapshot, SubmitTurnResponse,
};
use crate::event_stream::{CodeEventStream, CodeStreamNext};

use super::TurnStatus;

/// Default `timeout_seconds` for `code_run_turn` / `code_wait` / `code_decide`.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-session follow cursor so `code_wait` / `code_decide` resume the same turn.
#[derive(Debug, Clone)]
pub(crate) struct CodeFollowState {
    pub turn_id: CodeTurnId,
    pub last_seq: i64,
    pub assistant_text: String,
    pub queued: bool,
}

/// The run-turn / wait / decide return contract, plus queued extras.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodeTurnResult {
    pub status: TurnStatus,
    pub assistant_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<Value>,
    pub events_cursor: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<CodeTurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<i64>,
}

struct FollowGate {
    expected: Option<CodeTurnId>,
    ours: bool,
    seen_start: bool,
}

impl FollowGate {
    fn submit() -> Self {
        Self {
            expected: None,
            ours: false,
            seen_start: false,
        }
    }

    fn attach(turn: CodeTurnId) -> Self {
        Self {
            expected: Some(turn),
            ours: true,
            seen_start: false,
        }
    }

    fn waiting_for(turn: CodeTurnId) -> Self {
        Self {
            expected: Some(turn),
            ours: false,
            seen_start: false,
        }
    }

    fn turn_id(&self) -> Option<CodeTurnId> {
        self.expected
    }

    fn on_ran(&mut self, id: CodeTurnId) {
        if self.ours && self.expected == Some(id) {
            return;
        }
        if self.expected.is_some_and(|bound| bound != id) {
            self.ours = false;
            self.seen_start = false;
        }
        self.expected = Some(id);
    }

    fn on_frame(&mut self, replayed: bool, event: &CodeEvent) -> bool {
        if let CodeEvent::TurnStarted { turn_id } = event {
            let matches_bound = self.expected == Some(*turn_id);
            let live_unbound = self.expected.is_none() && !replayed;
            if matches_bound || live_unbound {
                self.expected = Some(*turn_id);
                self.ours = true;
                self.seen_start = true;
            }
        }
        if self.expected.is_none() || !self.ours {
            return false;
        }
        !(replayed && !self.seen_start)
    }
}

fn running(
    assistant_text: String,
    events_cursor: i64,
    turn_id: Option<CodeTurnId>,
) -> CodeTurnResult {
    CodeTurnResult {
        status: TurnStatus::Running,
        assistant_text,
        pending: None,
        events_cursor,
        turn_id,
        queue_position: None,
    }
}

fn from_turn_status(
    status: CodeTurnStatus,
    assistant_text: String,
    events_cursor: i64,
    turn_id: CodeTurnId,
) -> CodeTurnResult {
    let status = match status {
        CodeTurnStatus::Completed => TurnStatus::Completed,
        CodeTurnStatus::Failed => TurnStatus::Failed,
        CodeTurnStatus::Interrupted => TurnStatus::Cancelled,
        // A parked turn is still in flight from a follower's seat.
        CodeTurnStatus::Running | CodeTurnStatus::Waiting => TurnStatus::Running,
    };
    CodeTurnResult {
        status,
        assistant_text,
        pending: None,
        events_cursor,
        turn_id: Some(turn_id),
        queue_position: None,
    }
}

fn pending_approval(approval: &CodeApprovalSnapshot) -> Value {
    json!({
        "type": "approval",
        "approval_id": approval.id,
        "kind": approval.kind,
        "turn_id": approval.turn_id,
        "harness_raw_json": approval.harness_raw_json,
        "state": approval.state,
    })
}

fn reconcile_assistant_text(streamed: &mut String, complete: &str) {
    if complete.starts_with(streamed.as_str()) || streamed.starts_with(complete) {
        streamed.clear();
        streamed.push_str(complete);
        return;
    }
    streamed.clear();
    streamed.push_str(complete);
}

async fn pending_for_turn(
    client: &Client,
    session: CodeSessionId,
    turn_id: Option<CodeTurnId>,
) -> Result<Option<CodeApprovalSnapshot>> {
    let pending = client.list_approvals(Some(session), true).await?;
    Ok(pending
        .into_iter()
        .find(|approval| turn_id.is_none_or(|id| approval.turn_id == id)))
}

async fn turn_by_id(
    client: &Client,
    session: CodeSessionId,
    turn_id: CodeTurnId,
) -> Result<Option<CodeTurnSnapshot>> {
    let turns = client.list_session_turns(session).await?;
    Ok(turns.into_iter().find(|turn| turn.id == turn_id))
}

async fn running_turn_id(client: &Client, session: CodeSessionId) -> Result<Option<CodeTurnId>> {
    let turns = client.list_session_turns(session).await?;
    Ok(turns
        .into_iter()
        .rev()
        .find(|turn| turn.status == CodeTurnStatus::Running)
        .map(|turn| turn.id))
}

async fn timeout_result(
    client: &Client,
    session: CodeSessionId,
    gate: &FollowGate,
    assistant_text: String,
    events_cursor: i64,
) -> Result<CodeTurnResult> {
    let turn_id = match gate.turn_id() {
        Some(id) => Some(id),
        None => running_turn_id(client, session).await?,
    };
    Ok(running(assistant_text, events_cursor, turn_id))
}

async fn reconcile_lost_socket(
    client: &Client,
    session: CodeSessionId,
    turn_id: Option<CodeTurnId>,
    assistant_text: String,
    events_cursor: i64,
    stream_error: AgentError,
) -> Result<CodeTurnResult> {
    let Some(turn_id) = turn_id else {
        return Err(stream_error);
    };
    if let Some(turn) = turn_by_id(client, session, turn_id).await? {
        // A parked turn is open, not finished: fall through to the pending
        // approval, which is the only thing that can release it.
        if !turn.status.is_open() {
            return Ok(from_turn_status(
                turn.status,
                assistant_text,
                events_cursor,
                turn_id,
            ));
        }
    }
    if let Some(approval) = pending_for_turn(client, session, Some(turn_id)).await? {
        return Ok(CodeTurnResult {
            status: TurnStatus::NeedsApproval,
            assistant_text,
            pending: Some(pending_approval(&approval)),
            events_cursor,
            turn_id: Some(turn_id),
            queue_position: None,
        });
    }
    let queued = client.list_queued_code_turns(session).await?;
    if let Some(row) = queued.queued.iter().find(|row| row.id == turn_id) {
        return Ok(CodeTurnResult {
            status: TurnStatus::Queued,
            assistant_text,
            pending: None,
            events_cursor,
            turn_id: Some(turn_id),
            queue_position: Some(row.position),
        });
    }
    Ok(running(assistant_text, events_cursor, Some(turn_id)))
}

async fn apply_ours(
    client: &Client,
    session: CodeSessionId,
    stream: &CodeEventStream,
    gate: &FollowGate,
    assistant_text: &mut String,
    frame: &crate::api::code::SequencedCodeEventFrame,
) -> Result<Option<CodeTurnResult>> {
    match &frame.event {
        CodeEvent::AssistantDelta { text } => {
            if frame.replacement == Some(true) {
                reconcile_assistant_text(assistant_text, text);
            } else {
                assistant_text.push_str(text);
            }
            Ok(None)
        }
        CodeEvent::AssistantMessage { text, .. } => {
            reconcile_assistant_text(assistant_text, text);
            Ok(None)
        }
        CodeEvent::TurnStarted { .. } => {
            assistant_text.clear();
            Ok(None)
        }
        CodeEvent::ApprovalRequested { approval_id } => {
            let pending = match pending_for_turn(client, session, gate.turn_id()).await? {
                Some(pending) => pending_approval(&pending),
                None => json!({
                    "type": "approval",
                    "approval_id": approval_id,
                    "turn_id": gate.turn_id(),
                }),
            };
            Ok(Some(CodeTurnResult {
                status: TurnStatus::NeedsApproval,
                assistant_text: assistant_text.clone(),
                pending: Some(pending),
                events_cursor: stream.last_seq(),
                turn_id: gate.turn_id(),
                queue_position: None,
            }))
        }
        event if is_turn_terminal(event) => {
            let status = match event {
                CodeEvent::TurnCompleted { .. } => TurnStatus::Completed,
                CodeEvent::TurnInterrupted => TurnStatus::Cancelled,
                _ => TurnStatus::Failed,
            };
            Ok(Some(CodeTurnResult {
                status,
                assistant_text: assistant_text.clone(),
                pending: None,
                events_cursor: stream.last_seq(),
                turn_id: gate.turn_id(),
                queue_position: None,
            }))
        }
        _ => Ok(None),
    }
}

/// Post `message` after the socket is open, then follow until a settle, a
/// park, a queue receipt, or `timeout`.
pub(crate) async fn run_turn(
    client: &mut Client,
    session: CodeSessionId,
    message: &str,
    timeout: Duration,
) -> Result<CodeTurnResult> {
    let mut stream = CodeEventStream::open(client, session).await?;
    let submit_client = client.clone();
    let mut submit = std::pin::pin!(submit_client.submit_turn(session, message));
    let mut submit_done = false;
    let mut gate = FollowGate::submit();
    let mut assistant_text = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return timeout_result(client, session, &gate, assistant_text, stream.last_seq()).await;
        }
        let frame = tokio::select! {
            biased;
            frame = stream.next(client, session) => frame,
            submitted = async {
                if submit_done {
                    std::future::pending::<Result<SubmitTurnResponse>>().await
                } else {
                    submit.as_mut().await
                }
            } => {
                submit_done = true;
                match submitted? {
                    SubmitTurnResponse::Ran(turn) => gate.on_ran(turn.id),
                    SubmitTurnResponse::Queued(row) => {
                        return Ok(CodeTurnResult {
                            status: TurnStatus::Queued,
                            assistant_text,
                            pending: None,
                            events_cursor: stream.last_seq(),
                            turn_id: Some(row.id),
                            queue_position: Some(row.position),
                        });
                    }
                }
                continue;
            }
            () = tokio::time::sleep(remaining) => {
                return timeout_result(client, session, &gate, assistant_text, stream.last_seq())
                    .await;
            }
        };
        match handle_frame(
            client,
            session,
            &mut stream,
            &mut gate,
            &mut assistant_text,
            frame,
        )
        .await?
        {
            Some(result) => return Ok(result),
            None => continue,
        }
    }
}

/// Re-follow an in-flight or queued turn.
pub(crate) async fn wait_turn(
    client: &mut Client,
    session: CodeSessionId,
    follow: CodeFollowState,
    timeout: Duration,
) -> Result<CodeTurnResult> {
    if let Some(result) = settled_or_parked(client, session, &follow).await? {
        return Ok(result);
    }
    let mut gate = if follow.queued {
        FollowGate::waiting_for(follow.turn_id)
    } else {
        FollowGate::attach(follow.turn_id)
    };
    let mut stream = CodeEventStream::open_after(client, session, follow.last_seq).await?;
    follow_open_stream(
        client,
        &mut stream,
        session,
        &mut gate,
        follow.assistant_text,
        timeout,
    )
    .await
}

/// Apply an approval decision, then follow to the next settle point.
pub(crate) async fn decide_and_follow(
    client: &mut Client,
    session: CodeSessionId,
    approval_id: CodeApprovalId,
    approve: bool,
    feedback: Option<&str>,
    follow: Option<CodeFollowState>,
    timeout: Duration,
) -> Result<CodeTurnResult> {
    let turn_id = match follow.as_ref() {
        Some(follow) => follow.turn_id,
        None => {
            let pending = client.list_approvals(Some(session), true).await?;
            pending
                .into_iter()
                .find(|row| row.id == approval_id)
                .map(|row| row.turn_id)
                .ok_or_else(|| {
                    AgentError::msg(format!(
                        "no pending approval {approval_id} on session {session}"
                    ))
                })?
        }
    };
    let after_seq = follow.as_ref().map(|follow| follow.last_seq).unwrap_or(0);
    let assistant_text = follow
        .as_ref()
        .map(|follow| follow.assistant_text.clone())
        .unwrap_or_default();
    let mut stream = CodeEventStream::open_after(client, session, after_seq).await?;
    client
        .decide_code_approval(approval_id, approve, feedback)
        .await?;
    let mut gate = FollowGate::attach(turn_id);
    follow_open_stream(
        client,
        &mut stream,
        session,
        &mut gate,
        assistant_text,
        timeout,
    )
    .await
}

async fn settled_or_parked(
    client: &Client,
    session: CodeSessionId,
    follow: &CodeFollowState,
) -> Result<Option<CodeTurnResult>> {
    if let Some(turn) = turn_by_id(client, session, follow.turn_id).await? {
        if !turn.status.is_open() {
            return Ok(Some(from_turn_status(
                turn.status,
                follow.assistant_text.clone(),
                follow.last_seq,
                follow.turn_id,
            )));
        }
        if let Some(approval) = pending_for_turn(client, session, Some(follow.turn_id)).await? {
            return Ok(Some(CodeTurnResult {
                status: TurnStatus::NeedsApproval,
                assistant_text: follow.assistant_text.clone(),
                pending: Some(pending_approval(&approval)),
                events_cursor: follow.last_seq,
                turn_id: Some(follow.turn_id),
                queue_position: None,
            }));
        }
        return Ok(None);
    }
    let queued = client.list_queued_code_turns(session).await?;
    if queued.queued.iter().any(|row| row.id == follow.turn_id) {
        return Ok(None);
    }
    Ok(None)
}

async fn follow_open_stream(
    client: &mut Client,
    stream: &mut CodeEventStream,
    session: CodeSessionId,
    gate: &mut FollowGate,
    mut assistant_text: String,
    timeout: Duration,
) -> Result<CodeTurnResult> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return timeout_result(client, session, gate, assistant_text, stream.last_seq()).await;
        }
        let frame = tokio::select! {
            biased;
            frame = stream.next(client, session) => frame,
            () = tokio::time::sleep(remaining) => {
                return timeout_result(client, session, gate, assistant_text, stream.last_seq())
                    .await;
            }
        };
        match handle_frame(client, session, stream, gate, &mut assistant_text, frame).await? {
            Some(result) => return Ok(result),
            None => continue,
        }
    }
}

async fn handle_frame(
    client: &mut Client,
    session: CodeSessionId,
    stream: &mut CodeEventStream,
    gate: &mut FollowGate,
    assistant_text: &mut String,
    frame: Result<CodeStreamNext>,
) -> Result<Option<CodeTurnResult>> {
    let frame = match frame {
        Ok(CodeStreamNext::Frame(frame)) => frame,
        Ok(CodeStreamNext::Ignore) => return Ok(None),
        Err(error) => {
            return Ok(Some(
                reconcile_lost_socket(
                    client,
                    session,
                    gate.turn_id(),
                    assistant_text.clone(),
                    stream.last_seq(),
                    error,
                )
                .await?,
            ));
        }
    };
    if !gate.on_frame(frame.replayed == Some(true), &frame.event) {
        return Ok(None);
    }
    apply_ours(client, session, stream, gate, assistant_text, &frame).await
}
