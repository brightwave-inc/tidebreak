//! Chat-mode MCP tools. Reads are [`ApprovalClass::ReadOnly`]; mutations are
//! [`ApprovalClass::Sensitive`].

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tidebreak_core::{
    ApprovalClass, ChatId, Result, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry,
    ToolSpec, TurnId,
};

use crate::event_stream::EventStream;

use super::follow::{
    apply_decision, collect_events, follow_open_stream, follow_turn, turn_id_for_decision,
    Decision, DEFAULT_TIMEOUT,
};
use super::{AgentMcp, FollowState, TurnResult};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_EVENTS: usize = 200;
const MESSAGE_TAIL: usize = 8;

/// Register every chat tool on `registry`.
pub(crate) fn register(registry: &mut ToolRegistry, state: Arc<AgentMcp>) {
    registry.register(Box::new(ChatCreateTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatListTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatStatusTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatRunTurnTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatWaitTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatDecideTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatEventsTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatSteerTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatCancelTool { state }));
}

fn turn_output(result: &TurnResult) -> ToolOutput {
    let data = serde_json::to_value(result).unwrap_or(Value::Null);
    ToolOutput::text(format!(
        "status: {}{}",
        result.status.as_str(),
        if result.assistant_text.is_empty() {
            String::new()
        } else {
            format!("\n{}", result.assistant_text)
        }
    ))
    .with_data(data)
}

fn fail(message: impl Into<String>) -> Result<ToolOutput> {
    Ok(ToolOutput::error(message.into()))
}

fn fail_args(message: impl Into<String>) -> Result<ToolOutput> {
    Ok(ToolOutput::failed(
        ToolErrorCategory::InvalidArguments,
        message.into(),
    ))
}

fn required_chat_id(args: &Value) -> std::result::Result<ChatId, ToolOutput> {
    let Some(value) = args.get("chat_id").and_then(Value::as_str) else {
        return Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "chat_id is required",
        ));
    };
    ChatId::from_str(value).map_err(|_| {
        ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "chat_id must be a UUID",
        )
    })
}

fn timeout_from(args: &Value) -> std::result::Result<Duration, ToolOutput> {
    match args.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT),
        Some(value) => {
            let Some(seconds) = value.as_u64() else {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "timeout_seconds must be a positive integer",
                ));
            };
            if seconds == 0 {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "timeout_seconds must be at least 1",
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

async fn remember(state: &AgentMcp, chat: ChatId, turn_id: TurnId, result: &TurnResult) {
    state.follows.lock().await.insert(
        chat,
        FollowState {
            turn_id,
            last_seq: result.events_cursor,
            assistant_text: result.assistant_text.clone(),
        },
    );
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn chat_id_property() -> Value {
    json!({
        "type": "string",
        "description": "Chat UUID.",
    })
}

fn timeout_property() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "default": DEFAULT_TIMEOUT_SECS,
        "description": "Seconds to follow the turn before returning status running. The turn keeps going server-side.",
    })
}

// ---------------------------------------------------------------------------
// chat_create
// ---------------------------------------------------------------------------

struct ChatCreateTool {
    state: Arc<AgentMcp>,
}

fn chat_create_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_create".into(),
        description: "Create a chat. Optionally pin a catalog model and a permission mode (plan, ask, auto, or allow)."
            .into(),
        input_schema: object_schema(
            json!({
                "model": {
                    "type": "string",
                    "description": "Catalog key to pin on the new chat.",
                },
                "permission_mode": {
                    "type": "string",
                    "enum": ["plan", "ask", "auto", "allow"],
                    "description": "Permission mode stored on the chat.",
                },
            }),
            &[],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatCreateTool {
    fn spec(&self) -> ToolSpec {
        chat_create_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let model = args.get("model").and_then(Value::as_str);
        let permission_mode = args.get("permission_mode").and_then(Value::as_str);
        if let Some(mode) = permission_mode {
            if !matches!(mode, "plan" | "ask" | "auto" | "allow") {
                return fail_args("permission_mode must be plan, ask, auto, or allow");
            }
        }
        let client = self.state.client.lock().await;
        let chat = match client.create_chat().await {
            Ok(chat) => chat,
            Err(error) => return fail(error.to_string()),
        };
        if let Some(model) = model {
            if let Err(error) = client.set_chat_model(chat, Some(model)).await {
                return fail(error.to_string());
            }
        }
        if let Some(mode) = permission_mode {
            if let Err(error) = client.set_chat_permission_mode(chat, Some(mode)).await {
                return fail(error.to_string());
            }
        }
        let data = json!({ "chat_id": chat });
        Ok(ToolOutput::text(format!("chat {chat}")).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_list
// ---------------------------------------------------------------------------

struct ChatListTool {
    state: Arc<AgentMcp>,
}

fn chat_list_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_list".into(),
        description: "List chats, most recently active first.".into(),
        input_schema: object_schema(json!({}), &[]),
    }
}

#[async_trait::async_trait]
impl Tool for ChatListTool {
    fn spec(&self) -> ToolSpec {
        chat_list_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let client = self.state.client.lock().await;
        let chats = match client.list_chats().await {
            Ok(chats) => chats,
            Err(error) => return fail(error.to_string()),
        };
        let summaries: Vec<Value> = chats
            .iter()
            .map(|chat| {
                json!({
                    "chat_id": chat.id,
                    "title": chat.title,
                    "model": chat.model,
                    "permission_mode": chat.permission_mode,
                    "created_at": chat.created_at,
                })
            })
            .collect();
        let data = json!({ "chats": summaries });
        Ok(ToolOutput::text(format!("{} chat(s)", summaries.len())).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_status
// ---------------------------------------------------------------------------

struct ChatStatusTool {
    state: Arc<AgentMcp>,
}

fn chat_status_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_status".into(),
        description:
            "Run state, a tail of recent messages, and pending approvals, plans, and questions."
                .into(),
        input_schema: object_schema(json!({ "chat_id": chat_id_property() }), &["chat_id"]),
    }
}

#[async_trait::async_trait]
impl Tool for ChatStatusTool {
    fn spec(&self) -> ToolSpec {
        chat_status_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let client = self.state.client.lock().await;
        let summary = match client.get_chat(chat).await {
            Ok(summary) => summary,
            Err(error) => return fail(error.to_string()),
        };
        let transcript = match client.chat_transcript(chat).await {
            Ok(transcript) => transcript,
            Err(error) => return fail(error.to_string()),
        };
        let approvals = match client.list_pending_approvals(chat).await {
            Ok(approvals) => approvals,
            Err(error) => return fail(error.to_string()),
        };
        let plans = match client.list_pending_plans(chat).await {
            Ok(plans) => plans
                .into_iter()
                .map(|plan| {
                    json!({
                        "call_id": plan.call_id,
                        "title": plan.title,
                        "plan": plan.plan,
                    })
                })
                .collect::<Vec<Value>>(),
            Err(error) => return fail(error.to_string()),
        };
        let questions = match client.list_pending_questions(chat).await {
            Ok(questions) => questions
                .into_iter()
                .map(|block| {
                    json!({
                        "call_id": block.call_id,
                        "questions": block.questions.into_iter().map(|question| {
                            json!({
                                "id": question.id,
                                "header": question.header,
                                "question": question.question,
                                "question_type": question.question_type,
                                "allow_free_form": question.allow_free_form,
                                "options": question.options.into_iter().map(|option| {
                                    json!({
                                        "id": option.id,
                                        "label": option.label,
                                        "description": option.description,
                                    })
                                }).collect::<Vec<Value>>(),
                            })
                        }).collect::<Vec<Value>>(),
                    })
                })
                .collect::<Vec<Value>>(),
            Err(error) => return fail(error.to_string()),
        };
        let folder = match client.list_pending_folder_access(chat).await {
            Ok(folder) => folder,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);

        let messages = transcript
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tail: Vec<Value> = messages
            .iter()
            .rev()
            .take(MESSAGE_TAIL)
            .rev()
            .map(|message| {
                json!({
                    "id": message.get("id"),
                    "role": message.get("role"),
                    "content": message.get("content"),
                })
            })
            .collect();

        let follow = self.state.follows.lock().await.get(&chat).cloned();
        let terminal = transcript
            .get("terminal_turns")
            .and_then(Value::as_array)
            .and_then(|turns| turns.last());
        let run_state = if !approvals.is_empty() {
            "needs_approval"
        } else if !plans.is_empty() {
            "needs_plan_decision"
        } else if !questions.is_empty() {
            "needs_answer"
        } else if !folder.is_empty() {
            "needs_host_consent"
        } else if let Some(follow) = &follow {
            let finished = transcript
                .get("terminal_turns")
                .and_then(Value::as_array)
                .is_some_and(|turns| {
                    turns.iter().any(|turn| {
                        turn.get("turn_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| id == follow.turn_id.to_string())
                    })
                });
            if finished {
                terminal
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("idle")
            } else {
                "running"
            }
        } else {
            terminal
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("idle")
        };

        let data = json!({
            "chat_id": chat,
            "title": summary.title,
            "model": summary.model,
            "permission_mode": summary.permission_mode,
            "run_state": run_state,
            "messages": tail,
            "pending_approvals": approvals,
            "pending_plans": plans,
            "pending_questions": questions,
            "pending_host_consent": folder,
        });
        Ok(ToolOutput::text(format!("run_state: {run_state}")).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_run_turn
// ---------------------------------------------------------------------------

struct ChatRunTurnTool {
    state: Arc<AgentMcp>,
}

fn chat_run_turn_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_run_turn".into(),
        description: "Post a user prompt and follow the turn until it settles, parks on an interaction, or the timeout elapses."
            .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "prompt": {
                    "type": "string",
                    "description": "User text to post as this turn.",
                },
                "timeout_seconds": timeout_property(),
            }),
            &["chat_id", "prompt"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatRunTurnTool {
    fn spec(&self) -> ToolSpec {
        chat_run_turn_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(prompt) = args.get("prompt").and_then(Value::as_str) else {
            return fail_args("prompt is required");
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };
        let turn_id = TurnId::new();
        let mut client = self.state.client.lock().await;
        let mut stream = match EventStream::open(&client, chat).await {
            Ok(stream) => stream,
            Err(error) => return fail(error.to_string()),
        };
        if let Err(error) = client.post_message(chat, turn_id, prompt, &[], &[]).await {
            return fail(error.to_string());
        }
        let result = match follow_open_stream(
            &mut client,
            &mut stream,
            chat,
            turn_id,
            false,
            String::new(),
            timeout,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.state, chat, turn_id, &result).await;
        Ok(turn_output(&result))
    }
}

// ---------------------------------------------------------------------------
// chat_wait
// ---------------------------------------------------------------------------

struct ChatWaitTool {
    state: Arc<AgentMcp>,
}

fn chat_wait_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_wait".into(),
        description: "Re-follow an in-flight turn until it settles, parks, or the timeout elapses."
            .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "timeout_seconds": timeout_property(),
            }),
            &["chat_id"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatWaitTool {
    fn spec(&self) -> ToolSpec {
        chat_wait_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };
        let follow = self.state.follows.lock().await.get(&chat).cloned();
        let Some(follow) = follow else {
            return fail(format!("no in-flight turn for chat {chat}"));
        };
        let mut client = self.state.client.lock().await;
        let result = match follow_turn(
            &mut client,
            chat,
            follow.turn_id,
            follow.last_seq,
            follow.assistant_text,
            timeout,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.state, chat, follow.turn_id, &result).await;
        Ok(turn_output(&result))
    }
}

// ---------------------------------------------------------------------------
// chat_decide
// ---------------------------------------------------------------------------

struct ChatDecideTool {
    state: Arc<AgentMcp>,
}

fn chat_decide_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_decide".into(),
        description: "Apply an approval, plan, or questions decision, then follow to the next settle point. Decision objects mirror the print stdin protocol."
            .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "decision": {
                    "type": "object",
                    "description": "Print-protocol decision: {type:\"approval\", call_id, approve, feedback?} or {type:\"plan\", call_id, ...} or {type:\"questions\", call_id, answers}.",
                },
                "timeout_seconds": timeout_property(),
            }),
            &["chat_id", "decision"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatDecideTool {
    fn spec(&self) -> ToolSpec {
        chat_decide_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(decision_value) = args.get("decision") else {
            return fail_args("decision is required");
        };
        let decision = match Decision::parse(decision_value) {
            Ok(decision) => decision,
            Err(message) => return fail_args(message),
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };

        let stored = self.state.follows.lock().await.get(&chat).cloned();
        let mut client = self.state.client.lock().await;
        let turn_id = match stored.as_ref() {
            Some(follow) => follow.turn_id,
            None => match turn_id_for_decision(&client, chat, &decision).await {
                Ok(turn_id) => turn_id,
                Err(error) => return fail(error.to_string()),
            },
        };
        let after_seq = stored.as_ref().map(|follow| follow.last_seq).unwrap_or(0);
        let assistant_text = stored
            .as_ref()
            .map(|follow| follow.assistant_text.clone())
            .unwrap_or_default();

        let mut stream = match EventStream::open_after(&client, chat, after_seq).await {
            Ok(stream) => stream,
            Err(error) => return fail(error.to_string()),
        };
        if let Err(error) = apply_decision(&client, chat, &decision).await {
            return fail(error.to_string());
        }
        let result = match follow_open_stream(
            &mut client,
            &mut stream,
            chat,
            turn_id,
            after_seq > 0,
            assistant_text,
            timeout,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.state, chat, turn_id, &result).await;
        Ok(turn_output(&result))
    }
}

// ---------------------------------------------------------------------------
// chat_events
// ---------------------------------------------------------------------------

struct ChatEventsTool {
    state: Arc<AgentMcp>,
}

fn chat_events_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_events".into(),
        description: "Raw journal frames after a cursor, for auditing. Capped at 200 events."
            .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "after_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Return frames with seq greater than this cursor.",
                },
                "max_events": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_EVENTS,
                    "default": MAX_EVENTS,
                    "description": "Maximum frames to return (capped at 200).",
                },
            }),
            &["chat_id"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatEventsTool {
    fn spec(&self) -> ToolSpec {
        chat_events_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let after_seq = match args.get("after_seq") {
            None | Some(Value::Null) => 0,
            Some(value) => match value.as_i64() {
                Some(seq) if seq >= 0 => seq,
                _ => return fail_args("after_seq must be a non-negative integer"),
            },
        };
        let max_events = match args.get("max_events") {
            None | Some(Value::Null) => MAX_EVENTS,
            Some(value) => match value.as_u64() {
                Some(n) if n >= 1 => (n as usize).min(MAX_EVENTS),
                _ => return fail_args("max_events must be an integer from 1 to 200"),
            },
        };
        let mut client = self.state.client.lock().await;
        let (events, cursor) = match collect_events(&mut client, chat, after_seq, max_events).await
        {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "events": events,
            "events_cursor": cursor,
        });
        Ok(ToolOutput::text(format!("{} event(s)", events.len())).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_steer
// ---------------------------------------------------------------------------

struct ChatSteerTool {
    state: Arc<AgentMcp>,
}

fn chat_steer_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_steer".into(),
        description: "Steer the in-flight turn with more user text.".into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "text": {
                    "type": "string",
                    "description": "User text to inject into the active turn.",
                },
            }),
            &["chat_id", "text"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatSteerTool {
    fn spec(&self) -> ToolSpec {
        chat_steer_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return fail_args("text is required");
        };
        let follow = self.state.follows.lock().await.get(&chat).cloned();
        let Some(follow) = follow else {
            return fail(format!("no in-flight turn for chat {chat}"));
        };
        let client = self.state.client.lock().await;
        if let Err(error) = client
            .steer(chat, follow.turn_id, TurnId::new(), text)
            .await
        {
            return fail(error.to_string());
        }
        Ok(ToolOutput::text("steered").with_data(json!({ "ok": true })))
    }
}

// ---------------------------------------------------------------------------
// chat_cancel
// ---------------------------------------------------------------------------

struct ChatCancelTool {
    state: Arc<AgentMcp>,
}

fn chat_cancel_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_cancel".into(),
        description: "Cancel the in-flight turn.".into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
            }),
            &["chat_id"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatCancelTool {
    fn spec(&self) -> ToolSpec {
        chat_cancel_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let follow = self.state.follows.lock().await.get(&chat).cloned();
        let Some(follow) = follow else {
            return fail(format!("no in-flight turn for chat {chat}"));
        };
        let client = self.state.client.lock().await;
        if let Err(error) = client.cancel_turn(chat, follow.turn_id).await {
            return fail(error.to_string());
        }
        Ok(ToolOutput::text("cancelled").with_data(json!({ "ok": true })))
    }
}
