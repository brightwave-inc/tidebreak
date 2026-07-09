//! The agent loop: the turn engine that drives a conversation.
//!
//! One [`Agent`] ties together a [`ModelProvider`], a [`ToolRegistry`], and a
//! [`Store`], and runs a *turn* — one user input through to a final answer —
//! emitting [`AgentEvent`]s as it goes.
//!
//! Per turn the loop: assembles the request → streams the model call →, if the
//! model called tools, runs them and feeds the results back → repeats until the
//! model stops, bounded by a max-steps guard.
//!
//! v1 scope (deliberately small; each is a tracked follow-up):
//! - tool calls run **sequentially** (concurrency for independent calls later);
//! - approval is **auto** for `ReadOnly`/`Workspace`; `Sensitive` parks via an
//!   [`ApprovalGate`] until approve/reject (standing grants / auto-judge later);
//! - cross-turn context is the stored text messages (structured tool-call
//!   persistence + context summarization come later).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use futures::channel::mpsc::UnboundedSender;
use futures::future::{self, Either};
use futures::StreamExt;
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalGate, ApprovalRequest, RefuseGate};
use crate::cancel::CancelToken;
use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{CallId, MessageId, TurnId};
use crate::model::{Chat, Message, Role};
use crate::provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, StopReason, Usage,
};
use crate::steer::SteerInbox;
use crate::storage::Store;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

/// A name-keyed registry of the tools available to the agent.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its advertised name (replacing any existing one).
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    /// Builder-style [`register`](Self::register).
    #[must_use]
    pub fn with(mut self, tool: Box<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(Box::as_ref)
    }

    /// The specs of every registered tool, to advertise to the model.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    /// Whether no tools are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Default cap on a single tool result fed back to the model: 64 KiB (~16k
/// tokens), enough for typical files while bounding a runaway read. A rough
/// byte-proxy for a token budget; token-accurate capping + paging come later.
pub const DEFAULT_MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;

/// Per-turn tuning for an [`Agent`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Provider model identifier (e.g. `claude-opus-4-8`).
    pub model: String,
    /// System prompt, if any.
    pub system_prompt: Option<String>,
    /// Upper bound on tokens to generate per model call.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Max model calls in one turn before the turn fails (loop guard).
    pub max_steps: usize,
    /// Max bytes of a single tool result fed back to the model; larger results
    /// are truncated with a notice, so one big read can't blow the context.
    pub max_tool_result_bytes: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            system_prompt: None,
            max_tokens: None,
            temperature: None,
            max_steps: 16,
            max_tool_result_bytes: DEFAULT_MAX_TOOL_RESULT_BYTES,
        }
    }
}

/// Drives turns for a chat over a provider, tool set, and store.
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn Store>,
    config: AgentConfig,
    approvals: Arc<dyn ApprovalGate>,
    cancel: CancelToken,
    steer: SteerInbox,
}

/// A tool call accumulated from the provider stream.
struct PendingCall {
    call_id: CallId,
    provider_id: String,
    name: String,
    args: String,
}

impl Agent {
    /// Assemble an agent from its dependencies and config.
    ///
    /// Sensitive tools are refused by default ([`RefuseGate`]). Wire a real
    /// gate with [`with_approvals`](Self::with_approvals) for park-and-resume.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn Store>,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            store,
            config,
            approvals: Arc::new(RefuseGate),
            cancel: CancelToken::new(),
            steer: SteerInbox::new(),
        }
    }

    /// Use `gate` for Sensitive-tool decisions (park-and-resume on the server).
    #[must_use]
    pub fn with_approvals(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    /// Watch `cancel` so the turn can be stopped early. Without this the turn
    /// runs to completion (the default token is never tripped).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Drain mid-turn steer messages from `steer`. Without this the turn ignores
    /// any steer pushes (the default inbox stays empty).
    #[must_use]
    pub fn with_steer(mut self, steer: SteerInbox) -> Self {
        self.steer = steer;
        self
    }

    /// Run one turn: submit `user_input`, drive the loop to a final answer,
    /// streaming [`AgentEvent`]s to `events`.
    ///
    /// Returns `Err` (after emitting `TurnFailed`) on an infrastructure failure
    /// (provider, store) or when the step guard is exceeded. Tool failures are
    /// not errors — they come back to the model as failed tool output.
    pub async fn run_turn(
        &self,
        chat: &Chat,
        user_input: &str,
        events: &UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        let turn_id = TurnId::new();
        let _ = events.unbounded_send(AgentEvent::TurnStarted { turn_id });
        match self.drive(chat, turn_id, user_input, events).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = events.unbounded_send(AgentEvent::TurnFailed {
                    error: (&err).into(),
                });
                Err(err)
            }
        }
    }

    async fn drive(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        user_input: &str,
        events: &UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.persist(chat.id, turn_id, Role::User, user_input)
            .await?;
        // The provider transcript for this turn: prior stored text + the blocks
        // we build up as the loop runs.
        let mut transcript = self.load_transcript(chat.id).await?;
        let mut total_usage = Usage::default();

        for step in 0..self.config.max_steps {
            // Between steps: stop before starting a fresh model call if cancelled.
            if self.cancel.is_cancelled() {
                return self.finish_cancelled(events, total_usage);
            }
            // Boundary steer: inject any queued messages before the next model call.
            self.apply_steers(chat, turn_id, &mut transcript, events)
                .await?;

            let request = ChatRequest {
                model: self.config.model.clone(),
                system: self.config.system_prompt.clone(),
                messages: transcript.clone(),
                tools: self.tools.specs(),
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let mut stream = self.provider.stream(request).await?;
            let mut text = String::new();
            let mut calls: Vec<PendingCall> = Vec::new();
            let mut by_index: HashMap<u32, usize> = HashMap::new();
            let mut stop_reason = StopReason::EndTurn;

            // Race each stream item against cancel and interrupt-steer so a long
            // model call is preempted promptly. Cancel ends the turn; interrupt
            // discards this step's partial output and continues after injecting.
            enum StreamEnd {
                Done,
                Cancelled,
                Steered,
            }
            let stream_end = loop {
                let event = match future::select(
                    stream.next(),
                    future::select(self.cancel.cancelled(), self.steer.interrupted()),
                )
                .await
                {
                    Either::Left((Some(event), _)) => event,
                    Either::Left((None, _)) => break StreamEnd::Done,
                    Either::Right((Either::Left(((), _)), _)) => break StreamEnd::Cancelled,
                    Either::Right((Either::Right(((), _)), _)) => break StreamEnd::Steered,
                };
                match event {
                    ProviderEvent::TextDelta { text: delta } => {
                        let _ = events.unbounded_send(AgentEvent::TextDelta {
                            text: delta.clone(),
                        });
                        text.push_str(&delta);
                    }
                    ProviderEvent::ReasoningDelta { text: delta } => {
                        let _ = events.unbounded_send(AgentEvent::ReasoningDelta { text: delta });
                    }
                    ProviderEvent::ToolCallStarted { index, id, name } => {
                        let call_id = CallId::new();
                        let _ = events.unbounded_send(AgentEvent::ToolCallStarted {
                            call_id,
                            name: name.clone(),
                        });
                        by_index.insert(index, calls.len());
                        calls.push(PendingCall {
                            call_id,
                            provider_id: id,
                            name,
                            args: String::new(),
                        });
                    }
                    ProviderEvent::ToolCallArgsDelta { index, fragment } => {
                        if let Some(&i) = by_index.get(&index) {
                            let _ = events.unbounded_send(AgentEvent::ToolCallArgsDelta {
                                call_id: calls[i].call_id,
                                fragment: fragment.clone(),
                            });
                            calls[i].args.push_str(&fragment);
                        }
                    }
                    ProviderEvent::Usage(reported) => total_usage += reported,
                    ProviderEvent::Stop { reason } => stop_reason = reason,
                }
            };
            // Prefer cancel when both cancel and interrupt are ready (cancel is
            // the left arm of the nested select). Also catch a cancel that raced
            // the final stream event.
            if matches!(stream_end, StreamEnd::Cancelled) || self.cancel.is_cancelled() {
                return self.finish_cancelled(events, total_usage);
            }
            if matches!(stream_end, StreamEnd::Steered) {
                // Discard this step's partial output — nothing from it was
                // persisted. The marker lets replay/live clients clear deltas
                // that were already streamed for this abandoned provider step.
                let _ = events.unbounded_send(AgentEvent::StreamInterrupted);
                self.apply_steers(chat, turn_id, &mut transcript, events)
                    .await?;
                continue;
            }

            // Record the assistant message (text + any tool-use blocks).
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                blocks.push(ContentBlock::Text { text: text.clone() });
                self.persist(chat.id, turn_id, Role::Assistant, &text)
                    .await?;
            }
            for call in &calls {
                blocks.push(ContentBlock::ToolUse {
                    id: call.provider_id.clone(),
                    name: call.name.clone(),
                    input: parse_args(&call.args),
                });
            }
            if !blocks.is_empty() {
                transcript.push(ChatMessage {
                    role: Role::Assistant,
                    content: blocks,
                });
            }

            if calls.is_empty() {
                // Drain steers until the inbox is quiet, then complete. A steer
                // that arrives as the stream finished must continue the turn
                // rather than race a TurnCompleted. `try_complete` holds the
                // queue lock across the empty-check and terminal emit so a
                // concurrent push cannot 202 and then be orphaned.
                loop {
                    if self.cancel.is_cancelled() {
                        return self.finish_cancelled(events, total_usage);
                    }
                    if self
                        .apply_steers(chat, turn_id, &mut transcript, events)
                        .await?
                    {
                        break; // continue the outer step loop below
                    }
                    if self.steer.try_complete(|| {
                        let _ = events.unbounded_send(AgentEvent::TurnCompleted {
                            usage: total_usage,
                            stop_reason,
                        });
                    }) {
                        return Ok(());
                    }
                    // Steer arrived between drain and try_complete — loop.
                }
                continue;
            }

            // Tool calls need a following model call to consume their results. If
            // no step remains, stop now — running them would be wasted side
            // effects the model can never act on.
            if step + 1 >= self.config.max_steps {
                return Err(AgentError::msg("max steps per turn exceeded"));
            }

            // Run the tool calls and feed the results back for the next step.
            let mut results: Vec<ContentBlock> = Vec::new();
            for call in &calls {
                let output = self.run_tool(chat, turn_id, call, events).await;
                let _ = events.unbounded_send(AgentEvent::ToolCallCompleted {
                    call_id: call.call_id,
                    output: output.clone(),
                });
                self.persist(chat.id, turn_id, Role::Tool, &output.content)
                    .await?;
                results.push(ContentBlock::ToolResult {
                    tool_use_id: call.provider_id.clone(),
                    content: output.content,
                    is_error: output.is_error,
                });
                // A cancel that arrived during this tool (including while it was
                // parked on approval) stops the turn before the next model call.
                if self.cancel.is_cancelled() {
                    return self.finish_cancelled(events, total_usage);
                }
            }
            // Tool results ride in a user-role message (the Messages convention).
            transcript.push(ChatMessage {
                role: Role::User,
                content: results,
            });
            // Boundary steer after tools — injected before the next model step.
            self.apply_steers(chat, turn_id, &mut transcript, events)
                .await?;
        }

        Err(AgentError::msg("max steps per turn exceeded"))
    }

    /// Emit the cancellation terminal event and end the turn as a (non-error)
    /// success — the client asked for the stop, so it isn't a `TurnFailed`.
    fn finish_cancelled(&self, events: &UnboundedSender<AgentEvent>, usage: Usage) -> Result<()> {
        let _ = events.unbounded_send(AgentEvent::TurnCancelled { usage });
        Ok(())
    }

    /// Drain the steer inbox into the transcript. Returns whether any messages
    /// were injected. Emits [`AgentEvent::UserSteered`] per message.
    async fn apply_steers(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        transcript: &mut Vec<ChatMessage>,
        events: &UnboundedSender<AgentEvent>,
    ) -> Result<bool> {
        let msgs = self.steer.drain();
        if msgs.is_empty() {
            return Ok(false);
        }
        for msg in msgs {
            self.persist(chat.id, turn_id, Role::User, &msg.content)
                .await?;
            transcript.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: msg.content.clone(),
                }],
            });
            let _ = events.unbounded_send(AgentEvent::UserSteered {
                content: msg.content,
            });
        }
        Ok(true)
    }

    /// Resolve approval and execute one tool call, returning its output. Tool and
    /// approval failures surface as error output, never `Err`.
    async fn run_tool(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        call: &PendingCall,
        events: &UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        // v1 policy: ReadOnly/Workspace auto; Sensitive parks on the approval gate.
        // Arm *before* emitting ApprovalRequired so a client that sees the event
        // can never race a 404 against a not-yet-parked call.
        if matches!(tool.approval_class(), ApprovalClass::Sensitive) {
            let summary = format!("{} requires approval", call.name);
            let pending = self.approvals.arm(ApprovalRequest {
                call_id: call.call_id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                summary: summary.clone(),
            });
            let _ = events.unbounded_send(AgentEvent::ApprovalRequired {
                call_id: call.call_id,
                class: ApprovalClass::Sensitive,
                summary,
            });
            // Race the decision against cancellation so a turn parked on approval
            // can still be stopped. On cancel we close the approval card
            // (`ApprovalDecided { approved: false }`) and return an error result;
            // the loop's post-tool check then ends the turn as cancelled.
            //
            // `future::select` polls the left arm first, so when both are ready
            // (approve lands in the same tick as cancel) the decision would win
            // and a Sensitive tool would still run. Prefer cancel whenever the
            // token is already tripped (same idea as the post-stream\n            // `is_cancelled()` re-check after `select`).
            let decision = match future::select(pending, self.cancel.cancelled()).await {
                Either::Left((decision, _)) if !self.cancel.is_cancelled() => decision,
                Either::Left(_) | Either::Right(((), _)) => {
                    let _ = events.unbounded_send(AgentEvent::ApprovalDecided {
                        call_id: call.call_id,
                        approved: false,
                    });
                    return ToolOutput::error("turn cancelled while awaiting approval");
                }
            };
            let approved = matches!(decision, ApprovalDecision::Approve);
            let _ = events.unbounded_send(AgentEvent::ApprovalDecided {
                call_id: call.call_id,
                approved,
            });
            if let ApprovalDecision::Reject { reason } = decision {
                return ToolOutput::error(reason);
            }
            // A cancel that lands after Approve won `select` but before execute
            // (concurrent trip of the token) must not run the Sensitive tool.
            if self.cancel.is_cancelled() {
                return ToolOutput::error("turn cancelled while awaiting approval");
            }
        }
        let ctx = ToolCtx {
            chat_id: chat.id,
            workspace_dir: chat.workspace_dir.clone(),
        };
        let mut output = match tool.execute(&ctx, parse_args(&call.args)).await {
            Ok(output) => output,
            Err(err) => ToolOutput::error(err.to_string()),
        };
        if let Some(truncated) =
            truncate_to_bytes(&output.content, self.config.max_tool_result_bytes)
        {
            output.content = truncated;
        }
        output
    }

    async fn persist(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        role: Role,
        content: &str,
    ) -> Result<()> {
        self.store
            .append_message(&Message {
                id: MessageId::new(),
                chat_id,
                turn_id,
                role,
                content: content.to_string(),
                created_at: Utc::now(),
            })
            .await
    }

    async fn load_transcript(&self, chat_id: crate::id::ChatId) -> Result<Vec<ChatMessage>> {
        Ok(self
            .store
            .list_messages(chat_id)
            .await?
            .into_iter()
            .map(|message| ChatMessage::text(message.role, message.content))
            .collect())
    }
}

/// Truncate `content` to at most `max_bytes` (on a UTF-8 char boundary) and
/// append a notice. Returns `None` when it already fits.
fn truncate_to_bytes(content: &str, max_bytes: usize) -> Option<String> {
    if content.len() <= max_bytes {
        return None;
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!(
        "{}\n\n[truncated: {} of {} bytes shown]",
        &content[..end],
        end,
        content.len()
    ))
}

/// Parse accumulated tool-call args; malformed JSON becomes an empty object so a
/// tool can report the problem itself rather than aborting the turn.
fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or(Value::Object(Default::default()))
}

// The end-to-end test needs the SQLite store and the built-in tools.
#[cfg(all(test, feature = "sqlite", feature = "tools"))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use futures::channel::mpsc::unbounded;
    use futures::stream::{self, BoxStream};

    use super::*;
    use crate::db::DbStore;
    use crate::id::ChatId;
    use crate::provider::ProviderId;
    use crate::tools::ReadFile;

    /// A scripted provider: step 0 calls `read_file`, step 1 gives a final answer.
    struct FakeProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fake")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_1".into(),
                        name: "read_file".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"path":"note.txt"}"#.into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                        ..Default::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        ..Default::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn turn_runs_a_tool_call_then_finishes() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let tools = Arc::new(ToolRegistry::new().with(Box::new(ReadFile)));
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        // The tool ran against the real workspace file and the turn completed.
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { .. })
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallStarted { name, .. } if name == "read_file"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "hello from disk" && !output.is_error
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "done")));
        // TurnCompleted usage sums both model calls (5+3 in, 2+4 out).
        let usage = events.iter().find_map(|e| match e {
            AgentEvent::TurnCompleted { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(
            usage.map(|u| (u.input_tokens, u.output_tokens)),
            Some((8, 6))
        );

        // User input, the tool result, and the final answer were persisted.
        let stored = store.list_messages(chat.id).await.unwrap();
        let roles: Vec<Role> = stored.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Tool, Role::Assistant]);
    }

    #[tokio::test]
    async fn max_steps_guard_fails_before_running_tools() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        // Only one step allowed, but step 0 returns a tool call — there's no
        // step left to consume the result, so the tool must NOT run.
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        let result = agent.run_turn(&chat, "read note.txt", &tx).await;
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(result.is_err());
        assert!(matches!(events.last(), Some(AgentEvent::TurnFailed { .. })));
        // The tool never ran: no completion event and nothing tool-related persisted.
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. })));
        let roles: Vec<Role> = store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(roles, vec![Role::User]);
    }

    #[tokio::test]
    async fn large_tool_results_are_truncated() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "x".repeat(10_000)).unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store,
            AgentConfig {
                model: "fake".into(),
                max_tool_result_bytes: 100,
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        let output = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { output, .. } => Some(output.clone()),
                _ => None,
            })
            .expect("a tool completed");
        assert!(!output.is_error);
        assert!(output.content.len() < 10_000, "result should be capped");
        assert!(output.content.contains("[truncated:"));
    }

    /// A Sensitive tool that records whether it ran.
    struct BoomTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "boom".into(),
                description: "a sensitive tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }
        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("boomed"))
        }
    }

    /// Provider that always asks for the `boom` tool once, then finishes.
    struct BoomProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for BoomProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("boom")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_boom".into(),
                        name: "boom".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn sensitive_tool_parks_until_approved() {
        use crate::approval::AutoApproveGate;

        let workspace = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(AutoApproveGate));

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalDecided { approved: true, .. })));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "boomed" && !output.is_error
        )));
    }

    /// Streams one text delta, then stalls forever — lets a test cancel mid-stream
    /// at a known point (after the delta lands).
    struct StallProvider;

    #[async_trait]
    impl ModelProvider for StallProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("stall")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let head = stream::iter(vec![ProviderEvent::TextDelta {
                text: "partial".into(),
            }]);
            Ok(head.chain(stream::pending()).boxed())
        }
    }

    /// Gate that signals once a call is parked, then never resolves — so a test
    /// can cancel a turn while it is genuinely waiting on approval.
    struct SignalPendingGate {
        armed: std::sync::Mutex<Option<futures::channel::oneshot::Sender<()>>>,
    }

    impl ApprovalGate for SignalPendingGate {
        fn arm(&self, _request: ApprovalRequest) -> crate::approval::ApprovalFuture<'_> {
            if let Some(tx) = self.armed.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Box::pin(future::pending())
        }
    }

    /// Trips cancel, then resolves Approve immediately — both arms of the
    /// approval `select` are ready in the same poll. Without a cancel-preferring
    /// check, `select` would take Approve and the Sensitive tool would run.
    struct CancelThenApproveGate {
        cancel: CancelToken,
    }

    impl ApprovalGate for CancelThenApproveGate {
        fn arm(&self, _request: ApprovalRequest) -> crate::approval::ApprovalFuture<'_> {
            self.cancel.cancel();
            Box::pin(async { ApprovalDecision::Approve })
        }
    }

    async fn cancel_test_chat() -> (Arc<dyn Store>, Chat, tempfile::TempDir) {
        let workspace = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        (store, chat, workspace)
    }

    #[tokio::test]
    async fn cancel_before_the_turn_stops_before_any_model_call() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        // A provider whose stream would panic the test if ever polled — proving
        // the loop-top check short-circuits before the first model call.
        let provider = FakeProvider {
            calls: AtomicUsize::new(0),
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        let agent = Agent::new(
            Arc::new(provider),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        // Only the lifecycle bookends: started → cancelled, no model work between.
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { .. })));
    }

    #[tokio::test]
    async fn cancel_mid_stream_preempts_the_model_call() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(StallProvider),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel.clone());

        let (tx, mut rx) = unbounded();
        let chat_id = chat.id;
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        // Cancel the instant the first delta lands; the stream then stalls, so
        // only the cancel can end the turn.
        let mut cancelled = false;
        while let Some(event) = rx.next().await {
            match event {
                AgentEvent::TextDelta { text } if text == "partial" => cancel.cancel(),
                AgentEvent::TurnCancelled { .. } => cancelled = true,
                _ => {}
            }
        }
        handle.await.unwrap();

        assert!(cancelled, "a mid-stream cancel ends the turn as cancelled");
        // The partial assistant text of the preempted step is discarded, not stored.
        let roles: Vec<Role> = store
            .list_messages(chat_id)
            .await
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(roles, vec![Role::User]);
    }

    #[tokio::test]
    async fn cancel_unblocks_a_turn_parked_on_approval() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let (armed_tx, armed_rx) = futures::channel::oneshot::channel();
        let gate = Arc::new(SignalPendingGate {
            armed: std::sync::Mutex::new(Some(armed_tx)),
        });
        let ran = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(gate)
        .with_cancel(cancel.clone());

        let (tx, rx) = unbounded();
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        // Wait until the Sensitive call is genuinely parked, then cancel.
        armed_rx.await.unwrap();
        cancel.cancel();
        handle.await.unwrap();
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(ran.load(Ordering::SeqCst), 0, "the parked tool never runs");
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalDecided {
                approved: false,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    #[tokio::test]
    async fn cancel_wins_when_approval_and_cancel_are_both_ready() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let ran = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(CancelThenApproveGate {
            cancel: cancel.clone(),
        }))
        .with_cancel(cancel);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "cancel must preempt an approve that is ready in the same poll"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalDecided {
                approved: false,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    #[tokio::test]
    async fn interrupt_steer_preempts_mid_stream_and_continues() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        // First call stalls after "partial"; after steer, second call finishes.
        struct StallThenFinish {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl ModelProvider for StallThenFinish {
            fn id(&self) -> ProviderId {
                ProviderId::new("stall-then-finish")
            }
            async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let head = stream::iter(vec![ProviderEvent::TextDelta {
                        text: "partial".into(),
                    }]);
                    return Ok(head.chain(stream::pending()).boxed());
                }
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta {
                        text: "after steer".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }

        let steer = SteerInbox::new();
        let agent = Agent::new(
            Arc::new(StallThenFinish {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_steer(steer.clone());

        let chat_id = chat.id;
        let (tx, mut rx) = unbounded();
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        let mut steered = false;
        let mut interrupted = false;
        let mut completed = false;
        while let Some(event) = rx.next().await {
            match event {
                AgentEvent::TextDelta { text } if text == "partial" => {
                    steer.push("please change course", true);
                }
                AgentEvent::StreamInterrupted => {
                    interrupted = true;
                }
                AgentEvent::UserSteered { content } => {
                    assert_eq!(content, "please change course");
                    steered = true;
                }
                AgentEvent::TurnCompleted { .. } => completed = true,
                AgentEvent::TurnCancelled { .. } => {
                    panic!("steer must continue the turn, not cancel it")
                }
                _ => {}
            }
        }
        handle.await.unwrap();

        assert!(
            interrupted,
            "interrupt steer marks the partial provider stream as abandoned"
        );
        assert!(steered, "steer event emitted");
        assert!(completed, "turn completes after steer");
        let roles: Vec<_> = store
            .list_messages(chat_id)
            .await
            .unwrap()
            .iter()
            .map(|m| (m.role, m.content.clone()))
            .collect();
        // Initial user + steered user + final assistant (partial discarded).
        assert!(roles.iter().any(|(r, c)| *r == Role::User && c == "go"));
        assert!(roles
            .iter()
            .any(|(r, c)| *r == Role::User && c == "please change course"));
        assert!(roles
            .iter()
            .any(|(r, c)| *r == Role::Assistant && c == "after steer"));
        assert!(!roles.iter().any(|(_, c)| c == "partial"));
    }

    #[tokio::test]
    async fn cancel_wins_over_steer_when_both_ready() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let cancel = CancelToken::new();
        let steer = SteerInbox::new();
        // Trip both before the turn starts racing the stream.
        cancel.cancel();
        steer.push("ignored", true);

        let agent = Agent::new(
            Arc::new(StallProvider),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel)
        .with_steer(steer);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::UserSteered { .. })),
            "cancel must win; steer is not applied"
        );
    }

    #[tokio::test]
    async fn sensitive_tool_is_refused_without_a_gate() {
        let workspace = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: workspace.path().to_path_buf(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalDecided {
                approved: false,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. } if output.is_error
        )));
    }
}
