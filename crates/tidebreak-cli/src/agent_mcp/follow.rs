//! Follow one chat turn over the event journal until it settles.
//!
//! Given a chat and a turn that has already been posted (or is already in
//! flight), subscribe the events socket and watch until the turn completes,
//! fails, cancels, parks on an interaction, or the caller's timeout elapses.
//! A timeout returns [`TurnStatus::Running`]: the turn is durable on the
//! server and the caller re-checks with `chat_wait` or `chat_status`.
//!
//! Reconnect and durable-transcript fallback come from [`crate::event_stream`],
//! the same path print mode uses.

use std::time::Duration;

use serde_json::json;
use tidebreak_core::{AgentError, CallId, ChatId, Result, TurnId, REQUEST_FOLDER_ACCESS_TOOL};

use crate::api::client::{Client, DurableTurn, DurableTurnStatus};
use crate::api::wire::{
    ApprovalGrantRung, RendererAgentEvent, RendererToolName, ToolActionPreview, ToolApprovalKind,
};
use crate::event_stream::{EventStream, StreamNext};
use crate::print::protocol::Interaction;

use super::{TurnResult, TurnStatus};

/// Default `timeout_seconds` for `chat_run_turn` / `chat_wait` / `chat_decide`.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a folder-access watch asks whether the call has parked.
const FOLDER_POLL: Duration = Duration::from_millis(100);

/// Follow `turn_id` on `chat` until a settle point or `timeout`.
///
/// `after_seq` is the journal cursor to resume from (`0` to replay from the
/// start). `assistant_text` is text already collected on an earlier follow of
/// the same turn; new `TextDelta`s append. A terminal durable record short-
/// circuits the socket.
pub(crate) async fn follow_turn(
    client: &mut Client,
    chat: ChatId,
    turn_id: TurnId,
    after_seq: i64,
    assistant_text: String,
    timeout: Duration,
) -> Result<TurnResult> {
    if let Some(turn) = client.durable_turn(chat, turn_id).await? {
        return Ok(from_durable(turn, assistant_text));
    }
    let mut stream = EventStream::open_after(client, chat, after_seq).await?;
    follow_open_stream(
        client,
        &mut stream,
        chat,
        turn_id,
        after_seq > 0,
        assistant_text,
        timeout,
    )
    .await
}

/// Follow an already-open socket. The caller subscribes before posting so the
/// turn cannot start in a window this process is not watching.
pub(crate) async fn follow_open_stream(
    client: &mut Client,
    stream: &mut EventStream,
    chat: ChatId,
    turn_id: TurnId,
    mut ours: bool,
    mut assistant_text: String,
    timeout: Duration,
) -> Result<TurnResult> {
    let mut folder_watch: Option<CallId> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(running(assistant_text, stream.last_seq()));
        }

        let frame = tokio::select! {
            biased;
            frame = stream.next(client, chat, turn_id) => frame?,
            () = tokio::time::sleep(remaining) => {
                return Ok(running(assistant_text, stream.last_seq()));
            }
            () = tokio::time::sleep(FOLDER_POLL), if folder_watch.is_some() => {
                if let Some(call_id) = folder_watch {
                    if folder_access_parked(client, chat, call_id).await? {
                        return Ok(TurnResult {
                            status: TurnStatus::NeedsHostConsent,
                            assistant_text,
                            pending: Some(json!({
                                "type": "host_consent",
                                "tool": REQUEST_FOLDER_ACCESS_TOOL,
                                "call_id": call_id,
                            })),
                            events_cursor: stream.last_seq(),
                        });
                    }
                }
                continue;
            }
        };

        let event = match frame {
            StreamNext::Frame(_raw, event) => *event,
            StreamNext::Ignore => continue,
            StreamNext::Durable(turn) => {
                return Ok(from_durable(turn, assistant_text));
            }
        };

        if !ours {
            if !matches!(&event, RendererAgentEvent::TurnStarted { turn_id: id } if *id == turn_id)
            {
                continue;
            }
            ours = true;
        }

        match event {
            RendererAgentEvent::TextDelta { text } => assistant_text.push_str(&text),
            RendererAgentEvent::ToolCallStarted { call_id, name } => {
                if name == RendererToolName::RequestFolderAccess {
                    folder_watch = Some(call_id);
                }
            }
            RendererAgentEvent::ToolCallCompleted { call_id, .. } => {
                if folder_watch == Some(call_id) {
                    folder_watch = None;
                }
            }
            RendererAgentEvent::ApprovalRequired {
                call_id,
                action,
                approval,
                grant_rungs,
                preview,
                ..
            } => {
                return Ok(TurnResult {
                    status: TurnStatus::NeedsApproval,
                    assistant_text,
                    pending: Some(pending_approval(
                        call_id,
                        action,
                        approval,
                        grant_rungs,
                        preview,
                    )),
                    events_cursor: stream.last_seq(),
                });
            }
            RendererAgentEvent::PlanProposed { call_id, .. } => {
                if let Some(interaction) = pending_plan(client, chat, Some(call_id)).await? {
                    return Ok(TurnResult {
                        status: TurnStatus::NeedsPlanDecision,
                        assistant_text,
                        pending: Some(pending_from_interaction(&interaction)),
                        events_cursor: stream.last_seq(),
                    });
                }
            }
            RendererAgentEvent::UserQuestionsAsked { call_id, .. } => {
                if let Some(interaction) = pending_questions(client, chat, Some(call_id)).await? {
                    return Ok(TurnResult {
                        status: TurnStatus::NeedsAnswer,
                        assistant_text,
                        pending: Some(pending_from_interaction(&interaction)),
                        events_cursor: stream.last_seq(),
                    });
                }
            }
            RendererAgentEvent::TurnCompleted { .. } => {
                return Ok(TurnResult {
                    status: TurnStatus::Completed,
                    assistant_text,
                    pending: None,
                    events_cursor: stream.last_seq(),
                });
            }
            RendererAgentEvent::TurnFailed { .. } | RendererAgentEvent::TurnRefused { .. } => {
                return Ok(TurnResult {
                    status: TurnStatus::Failed,
                    assistant_text,
                    pending: None,
                    events_cursor: stream.last_seq(),
                });
            }
            RendererAgentEvent::TurnCancelled { .. } => {
                return Ok(TurnResult {
                    status: TurnStatus::Cancelled,
                    assistant_text,
                    pending: None,
                    events_cursor: stream.last_seq(),
                });
            }
            _ => {}
        }
    }
}

fn running(assistant_text: String, events_cursor: i64) -> TurnResult {
    TurnResult {
        status: TurnStatus::Running,
        assistant_text,
        pending: None,
        events_cursor,
    }
}

fn from_durable(turn: DurableTurn, assistant_text: String) -> TurnResult {
    let status = match turn.status {
        DurableTurnStatus::Completed => TurnStatus::Completed,
        DurableTurnStatus::Failed => TurnStatus::Failed,
        DurableTurnStatus::Cancelled => TurnStatus::Cancelled,
    };
    let assistant_text = if turn.content.is_empty() {
        assistant_text
    } else {
        turn.content
    };
    TurnResult {
        status,
        assistant_text,
        pending: None,
        events_cursor: turn.last_event_seq,
    }
}

fn pending_approval(
    call_id: CallId,
    action: RendererToolName,
    approval: ToolApprovalKind,
    grant_rungs: Vec<ApprovalGrantRung>,
    preview: Option<ToolActionPreview>,
) -> serde_json::Value {
    json!({
        "type": "approval",
        "call_id": call_id,
        "action": action,
        "approval": approval,
        "grant_rungs": grant_rungs,
        "preview": preview,
    })
}

fn pending_from_interaction(interaction: &Interaction) -> serde_json::Value {
    match interaction {
        Interaction::Approval {
            call_id,
            action,
            approval,
            grant_rungs,
            preview,
        } => pending_approval(
            *call_id,
            *action,
            *approval,
            grant_rungs.clone(),
            preview.clone(),
        ),
        Interaction::Plan {
            call_id,
            title,
            plan,
        } => json!({
            "type": "plan",
            "call_id": call_id,
            "title": title,
            "plan": plan,
        }),
        Interaction::Questions { call_id, questions } => json!({
            "type": "questions",
            "call_id": call_id,
            "questions": questions,
        }),
    }
}

async fn pending_plan(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> Result<Option<Interaction>> {
    let pending = client.list_pending_plans(chat).await?;
    Ok(pending
        .into_iter()
        .find(|plan| call_id.is_none_or(|id| plan.call_id == id))
        .map(Interaction::from_plan))
}

async fn pending_questions(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> Result<Option<Interaction>> {
    let pending = client.list_pending_questions(chat).await?;
    Ok(pending
        .into_iter()
        .find(|block| call_id.is_none_or(|id| block.call_id == id))
        .map(Interaction::from_questions))
}

async fn folder_access_parked(client: &Client, chat: ChatId, call_id: CallId) -> Result<bool> {
    let pending = client.list_pending_folder_access(chat).await?;
    let want = call_id.to_string();
    Ok(pending.iter().any(|row| {
        row.get("call_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == want)
    }))
}

/// Collect raw journal frames after `after_seq`, up to `max_events`.
///
/// Replay is drained, then a short idle ends the read so a live socket does
/// not hang the tool. Failures to open the socket surface as errors; a socket
/// that closes mid-read returns whatever was collected.
pub(crate) async fn collect_events(
    client: &mut Client,
    chat: ChatId,
    after_seq: i64,
    max_events: usize,
) -> Result<(Vec<serde_json::Value>, i64)> {
    let transcript = client.chat_transcript(chat).await?;
    let end = transcript
        .get("last_event_seq")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if after_seq >= end || max_events == 0 {
        return Ok((Vec::new(), after_seq.max(end)));
    }
    let mut stream = EventStream::open_after(client, chat, after_seq).await?;
    let mut events = Vec::new();
    let idle = Duration::from_secs(2);
    loop {
        if events.len() >= max_events || stream.last_seq() >= end {
            break;
        }
        let raw = tokio::select! {
            raw = stream.recv() => raw,
            () = tokio::time::sleep(idle) => break,
        };
        let Some(raw) = raw else {
            break;
        };
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(value) if value.get("seq").is_some() => events.push(value),
            Ok(_) => {}
            Err(_) => events.push(json!({ "raw": raw })),
        }
        if stream.last_seq() >= end {
            break;
        }
    }
    Ok((events, stream.last_seq()))
}

/// Apply a print-protocol decision over HTTP. The caller follows afterwards.
pub(crate) async fn apply_decision(
    client: &Client,
    chat: ChatId,
    decision: &Decision,
) -> Result<()> {
    match decision {
        Decision::Approval {
            call_id,
            approve,
            reason,
            grant,
        } => {
            client
                .decide_approval(chat, *call_id, *approve, reason, *grant)
                .await
        }
        Decision::Plan {
            call_id,
            accept,
            feedback,
            permission_mode,
        } => {
            client
                .decide_plan(
                    chat,
                    *call_id,
                    *accept,
                    feedback.as_deref(),
                    permission_mode.as_deref(),
                )
                .await
        }
        Decision::Questions { call_id, body } => {
            client.answer_questions(chat, *call_id, body.clone()).await
        }
    }
}

/// A decision object as `chat_decide` accepts it.
///
/// Mirrors the print stdin protocol (`type` + `decision`/`answers`) and also
/// the compact `{type:"approval", call_id, approve, feedback?}` shape.
pub(crate) enum Decision {
    Approval {
        call_id: CallId,
        approve: bool,
        reason: String,
        grant: Option<ApprovalGrantRung>,
    },
    Plan {
        call_id: CallId,
        accept: bool,
        feedback: Option<String>,
        permission_mode: Option<String>,
    },
    Questions {
        call_id: CallId,
        body: serde_json::Value,
    },
}

impl Decision {
    pub(crate) fn parse(value: &serde_json::Value) -> std::result::Result<Self, String> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "decision must include type".to_owned())?;
        let call_id = value
            .get("call_id")
            .ok_or_else(|| "decision must include call_id".to_owned())
            .and_then(|id| {
                serde_json::from_value::<CallId>(id.clone())
                    .map_err(|error| format!("call_id is not a UUID: {error}"))
            })?;
        match kind {
            "approval" => {
                let approve = parse_approve(value)?;
                let reason = value
                    .get("feedback")
                    .or_else(|| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("rejected by the driver")
                    .to_owned();
                let grant = value
                    .get("grant")
                    .cloned()
                    .filter(|grant| !grant.is_null())
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| format!("grant is not a known rung: {error}"))?;
                Ok(Self::Approval {
                    call_id,
                    approve,
                    reason,
                    grant,
                })
            }
            "plan" => {
                let accept = parse_plan_accept(value)?;
                let feedback = value
                    .get("feedback")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let permission_mode = value
                    .get("permission_mode")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                Ok(Self::Plan {
                    call_id,
                    accept,
                    feedback,
                    permission_mode,
                })
            }
            "questions" => {
                let answers = value
                    .get("answers")
                    .cloned()
                    .ok_or_else(|| "a questions decision must include answers".to_owned())?;
                Ok(Self::Questions {
                    call_id,
                    body: json!({ "answers": answers }),
                })
            }
            other => Err(format!("unknown decision type {other:?}")),
        }
    }
}

fn parse_approve(value: &serde_json::Value) -> std::result::Result<bool, String> {
    if let Some(approve) = value.get("approve") {
        return approve
            .as_bool()
            .ok_or_else(|| "approve must be a boolean".to_owned());
    }
    match value.get("decision").and_then(serde_json::Value::as_str) {
        Some("approve") => Ok(true),
        Some("reject") => Ok(false),
        Some(other) => Err(format!(
            "approval decision must be approve or reject, not {other:?}"
        )),
        None => Err("an approval decision needs approve or decision".to_owned()),
    }
}

fn parse_plan_accept(value: &serde_json::Value) -> std::result::Result<bool, String> {
    if let Some(accept) = value.get("accept") {
        return accept
            .as_bool()
            .ok_or_else(|| "accept must be a boolean".to_owned());
    }
    match value.get("decision").and_then(serde_json::Value::as_str) {
        Some("accept") => Ok(true),
        Some("reject") => Ok(false),
        Some(other) => Err(format!(
            "plan decision must be accept or reject, not {other:?}"
        )),
        None => Err("a plan decision needs accept or decision".to_owned()),
    }
}

/// Look up the turn a parked interaction belongs to when this process has not
/// followed it yet.
pub(crate) async fn turn_id_for_decision(
    client: &Client,
    chat: ChatId,
    decision: &Decision,
) -> Result<TurnId> {
    let call_id = match decision {
        Decision::Approval { call_id, .. }
        | Decision::Plan { call_id, .. }
        | Decision::Questions { call_id, .. } => *call_id,
    };
    let want = call_id.to_string();
    let approvals = client.list_pending_approvals(chat).await?;
    if let Some(row) = approvals.iter().find(|row| {
        row.get("call_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == want)
    }) {
        if let Some(turn_id) = row
            .get("turn_id")
            .and_then(|value| serde_json::from_value::<TurnId>(value.clone()).ok())
        {
            return Ok(turn_id);
        }
    }
    let plans = client.list_pending_plans(chat).await?;
    if let Some(plan) = plans.into_iter().find(|plan| plan.call_id == call_id) {
        return Ok(plan.turn_id);
    }
    let questions = client.list_pending_questions(chat).await?;
    if let Some(block) = questions.into_iter().find(|block| block.call_id == call_id) {
        return Ok(block.turn_id);
    }
    Err(AgentError::msg(format!(
        "no pending interaction named {call_id} on chat {chat}"
    )))
}
