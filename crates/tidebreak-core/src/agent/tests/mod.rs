use super::transcript::{
    batch_tool_calls, checkpoint_is_projectable, parse_args, parse_tool_args, rebuild_transcript,
    rebuild_transcript_with_boundary, tool_result_blocks, truncate_to_bytes,
    CHECKPOINT_CONTEXT_PREFIX,
};
use super::types::{CONTEXT_CHECKPOINT_INSTRUCTION, CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS};
use super::*;
use crate::approval::{ApprovalDecision, ApprovalRequest, RefuseGate, ToolApprovalKind};
use crate::compaction::CompactionPolicy;
use crate::context;
use crate::db::DbStore;
use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{AgentRunId, ProjectId};
use crate::image::ImageRef;
use crate::model::{
    Chat, MessageAttachment, Project, ToolCallExecution, ToolCallStatus, TurnRunStatus,
};
use crate::provider::{ChatRequest, ProviderEvent, ProviderId, ToolChoice, Usage, VendorWebSearch};
use crate::semantic_checkpoint::{ContextCheckpoint, ContextCheckpointPayloadV2};
use crate::storage::AcceptClaimedToolCallOutcome;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolErrorCategory, ToolScratch, ToolSpec};
use crate::tools::{ListDir, ReadFile, WriteFile};
use crate::PermissionMode;
use async_trait::async_trait;
use chrono::DateTime;
use futures::channel::mpsc::unbounded;
use futures::channel::oneshot;
use futures::future;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

mod approvals;
mod cancellation;
mod client_tools;
mod compaction;
mod output_writeback;
mod provider_search;
mod recovery;
mod sandbox_delegation;
mod tool_dispatch;
mod transcript_rebuild;
mod turn_flow;

fn tool_scratch(path: &std::path::Path) -> ToolScratch {
    ToolScratch::from_dir(
        cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap(),
    )
}

fn emitted_events(emissions: Vec<ClaimedAgentEvent>) -> Vec<AgentEvent> {
    emissions
        .into_iter()
        .map(|emission| match emission {
            ClaimedAgentEvent::Pending { event, .. } => event,
            ClaimedAgentEvent::Committed { event, .. } => event.event,
            ClaimedAgentEvent::Recovered { event, .. } => event.event,
            ClaimedAgentEvent::Flush(_) => panic!("unhandled claimed-event flush"),
        })
        .collect()
}

/// A scripted provider: step 0 calls `read_file`, step 1 gives a final answer.
struct FakeProvider {
    calls: AtomicUsize,
}

struct ClientToolProvider {
    assistant_text: bool,
    /// Emit a second, server-executed call beside the client one. The
    /// checkpoint still carries a single call, so the loop has to run this
    /// sibling first rather than refuse the batch.
    sibling_call: bool,
    name: &'static str,
    arguments: &'static str,
}

struct ContextRecordingTool {
    observed_project: Arc<Mutex<Option<Option<ProjectId>>>>,
    observed_call: Arc<Mutex<Option<CallId>>>,
}

#[async_trait]
impl Tool for ContextRecordingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "record invocation context".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        *self.observed_project.lock().unwrap() = Some(ctx.project_id);
        *self.observed_call.lock().unwrap() = ctx.call_id;
        Ok(ToolOutput::text("recorded"))
    }
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

#[async_trait]
impl ModelProvider for ClientToolProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("client-tool")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        // A wrap-up call forbids tool calls, so the model answers in prose. It
        // says so with `tool_choice: none` where there are tools to forbid, and
        // by advertising none at all where there are not — the host withholds
        // the control on a tool-less request because providers reject that
        // pairing.
        if req.tool_choice == Some(ToolChoice::None) || req.tools.is_empty() {
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "that is as far as I got".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }
        let mut events = vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "native_1".into(),
                name: self.name.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: self.arguments.into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ];
        if self.sibling_call {
            events.splice(
                1..1,
                [
                    ProviderEvent::ToolCallStarted {
                        index: 1,
                        id: "native_2".into(),
                        name: "read_file".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 1,
                        fragment: r#"{"path":"a.txt"}"#.into(),
                    },
                ],
            );
        }
        if self.assistant_text {
            events.insert(
                0,
                ProviderEvent::TextDelta {
                    text: "I will connect it".into(),
                },
            );
        }
        Ok(stream::iter(events).boxed())
    }
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

async fn search_grant_chat(store: &Arc<dyn Store>) -> Chat {
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    chat
}

async fn search_grant_store() -> Arc<dyn Store> {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    // Keep the temp dir alive for the process; SQLite owns its connection.
    std::mem::forget(db);
    store
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (store, chat, workspace)
}

struct CheckpointNoopTool;

#[async_trait]
impl Tool for CheckpointNoopTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "checkpoint_noop".into(),
            description: "Return one inert test result.".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        Ok(ToolOutput::text("checkpoint tool result"))
    }
}
