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
//! - approval is **auto** for `ReadOnly`/`Workspace`; `Sensitive` is refused
//!   (the park-and-resume approval flow lands next);
//! - cross-turn context is the stored text messages (structured tool-call
//!   persistence + context summarization come later).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{CallId, MessageId, TurnId};
use crate::model::{Chat, Message, Role};
use crate::provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, StopReason, Usage,
};
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
        }
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

            while let Some(event) = stream.next().await {
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
                let _ = events.unbounded_send(AgentEvent::TurnCompleted {
                    usage: total_usage,
                    stop_reason,
                });
                return Ok(());
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
                let output = self.run_tool(chat, call, events).await;
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
            }
            // Tool results ride in a user-role message (the Messages convention).
            transcript.push(ChatMessage {
                role: Role::User,
                content: results,
            });
        }

        Err(AgentError::msg("max steps per turn exceeded"))
    }

    /// Resolve approval and execute one tool call, returning its output. Tool and
    /// approval failures surface as error output, never `Err`.
    async fn run_tool(
        &self,
        chat: &Chat,
        call: &PendingCall,
        events: &UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        // v1 policy: ReadOnly/Workspace auto; Sensitive refused until the
        // park-and-resume approval flow lands.
        if matches!(tool.approval_class(), ApprovalClass::Sensitive) {
            let _ = events.unbounded_send(AgentEvent::ApprovalRequired {
                call_id: call.call_id,
                class: ApprovalClass::Sensitive,
                summary: format!("{} requires approval", call.name),
            });
            return ToolOutput::error("this tool requires approval, which is not yet supported");
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
}
