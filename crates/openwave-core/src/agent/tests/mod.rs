use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::DateTime;
use futures::channel::mpsc::unbounded;
use futures::channel::oneshot;
use futures::future;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use serde_json::Value;

use super::transcript::{
    batch_tool_calls, checkpoint_is_projectable, parse_args, parse_tool_args, rebuild_transcript,
    rebuild_transcript_with_boundary, tool_result_blocks, truncate_to_bytes,
    CHECKPOINT_CONTEXT_PREFIX,
};
use super::types::CONTEXT_CHECKPOINT_SYSTEM_PROMPT;
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
    Chat, MessageAttachment, PermissionMode, Project, ToolCallExecution, ToolCallStatus,
    TurnRunStatus,
};
use crate::provider::{ChatRequest, ProviderEvent, ProviderId, Usage, VendorWebSearch};
use crate::semantic_checkpoint::{ContextCheckpoint, ContextCheckpointPayloadV2};
use crate::storage::AcceptClaimedToolCallOutcome;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolErrorCategory, ToolScratch, ToolSpec};
use crate::tools::{ListDir, ReadFile, WriteFile};

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

/// Advertisement order has to depend only on which tools are registered.
/// A regression here is invisible in behavior — it shows up as prompt-cache
/// misses and irreproducible runs, so nothing else would catch it.
#[test]
fn advertised_tools_are_ordered_by_name_whatever_the_registration_order() {
    let forwards = ToolRegistry::default()
        .with(Box::new(ListDir))
        .with(Box::new(ReadFile))
        .with(Box::new(WriteFile));
    let backwards = ToolRegistry::default()
        .with(Box::new(WriteFile))
        .with(Box::new(ReadFile))
        .with(Box::new(ListDir));

    let names = |registry: &ToolRegistry| -> Vec<String> {
        registry.specs().into_iter().map(|spec| spec.name).collect()
    };
    assert_eq!(names(&forwards), ["list_dir", "read_file", "write_file"]);
    assert_eq!(names(&forwards), names(&backwards));
}

#[test]
fn tool_arguments_are_parsed_without_forgiving_malformed_json() {
    assert_eq!(parse_tool_args(""), Some(Value::Object(Default::default())));
    assert_eq!(
        parse_tool_args(r#"{"hint":"Documents"}"#),
        Some(serde_json::json!({"hint": "Documents"}))
    );
    assert_eq!(parse_tool_args(r#"{"hint":"Documents""#), None);
}

#[test]
fn malformed_arguments_keep_the_streamed_fragment_beside_the_coerced_object() {
    assert_eq!(parse_args(""), (Value::Object(Default::default()), None));
    assert_eq!(
        parse_args(r#"{"hint":"Documents"}"#),
        (serde_json::json!({"hint": "Documents"}), None)
    );
    let (value, fragment) = parse_args(r#"{"hint":"Documents""#);
    assert_eq!(value, Value::Object(Default::default()));
    assert_eq!(fragment.as_deref(), Some(r#"{"hint":"Documents""#));
    // The fragment is bounded, and the bound lands on a char boundary.
    let mut huge = String::from(r#"{"hint":""#);
    huge.push_str(&"é".repeat(ToolCallRecord::MAX_ARGUMENT_BYTES));
    let (_, fragment) = parse_args(&huge);
    let fragment = fragment.expect("a garbled stream keeps its fragment");
    assert!(fragment.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES);
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

struct SandboxCorrectionProvider {
    calls: AtomicUsize,
}

struct SiblingSandboxSpawnProvider;

/// Asks for a tool once, then answers, recording the tool surface each
/// request advertised.
struct ToolSurfaceRecordingProvider {
    calls: AtomicUsize,
    advertised: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ModelProvider for ToolSurfaceRecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.advertised
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|tool| tool.name.clone()).collect());
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
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Streams a provider replay block beside a tool-only step, then answers,
/// recording the native state each request carried.
struct ReasoningRecordingProvider {
    calls: AtomicUsize,
    seen: Arc<Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl ModelProvider for ReasoningRecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push(
            req.messages
                .iter()
                .flat_map(|message| message.reasoning.blocks().to_vec())
                .collect(),
        );
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ReasoningBlock {
                    data: serde_json::json!({
                        "type": "thinking",
                        "thinking": "plan: read the note first",
                        "signature": "sig-1",
                    }),
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"note.txt"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
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
        // A wrap-up call advertises no tools, and a model with no schemas in
        // front of it answers in prose.
        if req.tools.is_empty() {
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

#[async_trait]
impl ModelProvider for SandboxCorrectionProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-correction")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let arguments = if first {
            r#"{"task":"Research the error handling options.","resource":null}"#
        } else {
            r#"{"task":"Research the error handling options."}"#
        };
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: if first {
                    "sandbox_null".into()
                } else {
                    "sandbox_omitted".into()
                },
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: arguments.into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[async_trait]
impl ModelProvider for SiblingSandboxSpawnProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sibling-sandbox-spawn")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "spawn_a".into(),
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"task":"research A"}"#.into(),
            },
            ProviderEvent::ToolCallStarted {
                index: 1,
                id: "spawn_b".into(),
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 1,
                fragment: r#"{"task":"research B"}"#.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn claimed_agent_returns_a_client_tool_checkpoint_without_executing_it() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let client_spec = ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    };
    let mut registry = ToolRegistry::new();
    registry.register_client(client_spec.clone(), ApprovalClass::ReadOnly);
    assert_eq!(
        registry.execution("connect_folder"),
        Some(ToolCallExecution::Client)
    );
    assert!(registry.get("connect_folder").is_none());
    assert_eq!(registry.specs(), vec![client_spec]);
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);
    let AgentTurnOutcome::ClientToolCall {
        request,
        usage,
        steer_revision,
        model_steps,
    } = outcome
    else {
        panic!("claimed agent should return a client checkpoint");
    };
    assert_eq!(request.chat_id, chat.id);
    assert_eq!(request.turn_id, turn_id);
    assert_eq!(request.provider_id, "native_1");
    assert_eq!(request.name, "connect_folder");
    assert_eq!(request.arguments, serde_json::json!({"hint": "Documents"}));
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. } if name == "connect_folder"
    )));

    let mut validated_registry = ToolRegistry::new();
    validated_registry.register_validated_client(
        crate::request_folder_access_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_request_folder_access_arguments,
    );
    let invalid_agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::REQUEST_FOLDER_ACCESS_TOOL,
                arguments: r#"{"reason":"Read reports","requested_capabilities":["write_files"],"path":"/Users/example/Documents"}"#,
            }),
            Arc::new(validated_registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
    let (invalid_tx, _invalid_rx) = unbounded();
    let invalid_outcome = invalid_agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &invalid_tx)
        .await
        .unwrap();
    // Arguments the validator rejects never become a request: the call is
    // answered in place and the turn runs on rather than suspending on it.
    assert!(
        !matches!(invalid_outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "invalid arguments must not reach a checkpoint: {invalid_outcome:?}"
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

async fn output_writeback_fixture() -> (tempfile::TempDir, Arc<dyn Store>, Chat, TurnId, uuid::Uuid)
{
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("writeback.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "publish the report")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    (db, store, chat, turn_id, lease_token)
}

async fn create_named_output(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    filename: &str,
    created_at: chrono::DateTime<Utc>,
) -> crate::OutputId {
    let id = crate::OutputId::new();
    store
        .create_output(&crate::CreateOutput {
            id,
            chat_id,
            filename: filename.to_owned(),
            kind: crate::DeliverableKind::Text,
            revision: crate::NewOutputRevision {
                id: crate::OutputRevisionId::new(),
                byte_len: 5,
                sha256: [7; 32],
                turn_id: None,
                producing_run_id: None,
                created_at,
            },
        })
        .await
        .unwrap();
    id
}

fn output_writeback_agent(
    store: Arc<dyn Store>,
    arguments: String,
    lease_token: uuid::Uuid,
) -> Agent {
    let mut registry = ToolRegistry::new();
    registry.register_validated_client(
        crate::write_output_to_connected_folder_tool_spec(),
        ApprovalClass::Workspace,
        crate::validate_write_output_to_connected_folder_arguments,
    );
    Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
            arguments: Box::leak(arguments.into_boxed_str()),
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
}

/// The model names an output by filename; the checkpoint carries the
/// resolved opaque id of the newest live output with that name — the same
/// record the output scan would version — so everything downstream keeps
/// working from a stable identity the model never saw.
#[tokio::test]
async fn output_writeback_filename_resolves_to_the_newest_live_output() {
    let (_db, store, chat, turn_id, lease_token) = output_writeback_fixture().await;
    // A retracted `report.md` frees the name for a later one. Only one live
    // output can hold it, and that is the one the checkpoint must resolve.
    let older = Utc::now() - chrono::Duration::minutes(10);
    let retracted = create_named_output(&store, chat.id, "report.md", older).await;
    store.delete_output(retracted, older).await.unwrap();
    let newest = create_named_output(&store, chat.id, "report.md", Utc::now()).await;

    let root_id = uuid::Uuid::new_v4();
    let agent = output_writeback_agent(
        store.clone(),
        format!(
            r#"{{"filename":"report.md","root_id":"{root_id}","path":"reports/report.md","mode":"create"}}"#
        ),
        lease_token,
    );
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    let AgentTurnOutcome::ClientToolCall { request, .. } = outcome else {
        panic!("a resolvable filename must reach a client checkpoint: {outcome:?}");
    };
    assert_eq!(request.name, crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL);
    assert_eq!(
        request.arguments,
        serde_json::json!({
            "output_id": newest.as_uuid(),
            "root_id": root_id,
            "path": "reports/report.md",
            "mode": "create"
        })
    );
}

/// A filename with no live output — never published, or deleted — is
/// answered in place with an error naming the file, instead of parking a
/// checkpoint no executor could satisfy.
#[tokio::test]
async fn output_writeback_without_a_live_match_is_refused_naming_the_filename() {
    let (_db, store, chat, turn_id, lease_token) = output_writeback_fixture().await;
    let deleted = create_named_output(&store, chat.id, "report.md", Utc::now()).await;
    store.delete_output(deleted, Utc::now()).await.unwrap();

    let agent = output_writeback_agent(
        store.clone(),
        format!(
            r#"{{"filename":"report.md","root_id":"{}","path":"report.md","mode":"create"}}"#,
            uuid::Uuid::new_v4()
        ),
        lease_token,
    );
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    assert!(
        !matches!(outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "an unresolvable filename must not reach a checkpoint: {outcome:?}"
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    let events = emitted_events(rx.collect().await);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("report.md")
        )),
        "the refusal must name the filename"
    );
}

#[tokio::test]
async fn user_questions_are_advertised_and_executable_only_in_the_foreground() {
    let mut registry = ToolRegistry::new();
    registry.register_validated_foreground_client(
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );

    assert!(registry.specs().is_empty());
    assert_eq!(
        registry
            .specs_for_foreground(true)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>(),
        vec![crate::ASK_USER_QUESTIONS_TOOL]
    );
    assert_eq!(
        registry.execution(crate::ASK_USER_QUESTIONS_TOOL),
        Some(ToolCallExecution::Client)
    );
    assert!(registry.is_foreground_client(crate::ASK_USER_QUESTIONS_TOOL));
    assert!(registry.client_arguments_are_valid(
        crate::ASK_USER_QUESTIONS_TOOL,
        &serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [{
                    "id": "staging",
                    "label": "Staging",
                    "description": "Deploy for verification."
                }]
            }]
        })
    ));
    assert!(!registry.client_arguments_are_valid(
        crate::ASK_USER_QUESTIONS_TOOL,
        &serde_json::json!({"questions": []})
    ));

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("foreground-question.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::ASK_USER_QUESTIONS_TOOL,
            arguments: r#"{"questions":[{"id":"target","header":"Target","question":"Where should I deploy?","options":[{"id":"staging","label":"Staging","description":"Deploy for verification."}]}]}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "deploy", &tx).await.unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::UserQuestionsAsked { .. })));
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn claimed_foreground_agent_returns_one_bounded_sandbox_checkpoint() {
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
        // Consent is not what this test is about: the chat has already
        // said it will not be asked, so the checkpoint shape is what
        // shows through.
        permission_mode: Some(PermissionMode::Allow),
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "research this")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();

    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    assert!(registry.specs().is_empty());
    let advertised = registry
        .specs_for_foreground(true)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        advertised,
        [crate::SPAWN_SANDBOX_AGENT_TOOL, crate::WAIT_FOR_AGENTS_TOOL]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::SPAWN_SANDBOX_AGENT_TOOL,
            arguments: r#"{"task":"Research the error handling options."}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);
    let AgentTurnOutcome::SandboxAgentSpawn {
        request,
        usage,
        steer_revision,
        model_steps,
        ..
    } = outcome
    else {
        panic!("foreground agent should return a sandbox checkpoint");
    };
    assert_eq!(request.task, "Research the error handling options.");
    assert_eq!(
        request.child_run_id,
        AgentRunId::sandbox_for_spawn_call(request.call_id)
    );
    assert!(request.is_well_formed());
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. }
            if name == crate::SPAWN_SANDBOX_AGENT_TOOL
    )));

    let mut correction_registry = ToolRegistry::new();
    correction_registry.register_foreground_agent_orchestration();
    let correction_provider = Arc::new(SandboxCorrectionProvider {
        calls: AtomicUsize::new(0),
    });
    let correction_agent = Agent::new(
        correction_provider.clone(),
        Arc::new(correction_registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (correction_tx, correction_rx) = unbounded();
    let corrected = correction_agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &correction_tx)
        .await
        .unwrap();
    drop(correction_tx);
    let correction_events = emitted_events(correction_rx.collect().await);
    let AgentTurnOutcome::SandboxAgentSpawn {
        request,
        model_steps,
        ..
    } = corrected
    else {
        panic!("foreground agent should correct a noncanonical sandbox resource");
    };
    assert_eq!(correction_provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(model_steps, 2);
    assert_eq!(
        request.arguments,
        serde_json::json!({"task": "Research the error handling options."})
    );
    assert!(request.is_well_formed());
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    // The correction arrives as the call's own result rather than as a
    // discarded step, so the assistant's output for that step survives it.
    assert!(!correction_events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    assert!(
        correction_events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("omit `resource`")
        )),
        "{correction_events:?}"
    );
}

#[tokio::test]
async fn sibling_sandbox_spawns_are_retained_for_sequential_checkpoints() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("siblings.db").display()
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
        permission_mode: Some(PermissionMode::Allow),
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "delegate")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn_run(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let agent = Agent::new(
        Arc::new(SiblingSandboxSpawnProvider),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    let AgentTurnOutcome::SandboxAgentSpawn {
        request,
        remaining_requests,
        ..
    } = outcome
    else {
        panic!("sibling spawns should produce a checkpoint");
    };
    assert_eq!(request.task, "research A");
    assert_eq!(remaining_requests.len(), 1);
    assert_eq!(remaining_requests[0].task, "research B");
}

/// A background run's own calls never come back to this chat's gate, so
/// the spawn is the only moment consent can be asked for. A chat that
/// would park a foreground call parks the delegation too, and a refusal
/// leaves no checkpoint for the worker to admit a child from.
#[tokio::test]
async fn a_refused_delegation_never_reaches_a_spawn_checkpoint() {
    let (outcome, events) = drive_gated_delegation(Arc::new(RefuseGate)).await;
    assert!(
        !matches!(outcome, AgentTurnOutcome::SandboxAgentSpawn { .. }),
        "a refused delegation must not yield a spawn checkpoint: {outcome:?}"
    );
    // The card names the policy the child would inherit, because egress is
    // what the reader is deciding.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ApprovalRequired { kind, preview, .. }
                if *kind == ToolApprovalKind::DelegateMayRunBackgroundAgent
                    && matches!(
                        preview,
                        Some(ToolActionPreview::DelegateAgent { task, network })
                            if task == "research A"
                                && *network == crate::NetworkPolicy::Open
                    )
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. } if output.is_error
        )),
        "the model is told the delegation was refused: {events:?}"
    );
}

/// An approved delegation is admitted through the ordinary spawn
/// checkpoint, carrying the flag that tells it to finalize the row the
/// approval parked on rather than insert a second one.
#[tokio::test]
async fn an_approved_delegation_checkpoints_against_its_own_parked_call() {
    let (outcome, _events) = drive_gated_delegation(Arc::new(crate::AutoApproveGate)).await;
    let AgentTurnOutcome::SandboxAgentSpawn { request, .. } = outcome else {
        panic!("an approved delegation should produce a checkpoint: {outcome:?}");
    };
    assert_eq!(request.task, "research A");
    assert!(request.approval_gated);
}

/// Drive one delegation in a chat that asks before it acts, with the
/// claimed-turn sink drained concurrently so the gate's journal flush is
/// acknowledged the way the worker acknowledges it.
async fn drive_gated_delegation(
    gate: Arc<dyn ApprovalGate>,
) -> (AgentTurnOutcome, Vec<AgentEvent>) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("gate.db").display()
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
        network_policy: crate::NetworkPolicy::Open,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "delegate")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn_run(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::SPAWN_SANDBOX_AGENT_TOOL,
            arguments: r#"{"task":"research A"}"#,
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_approvals(gate)
    .with_foreground_agent_orchestration();
    let (tx, mut rx) = unbounded();
    let chat_for_turn = chat.clone();
    let handle = tokio::spawn(async move {
        agent
            .run_claimed_turn(&chat_for_turn, turn_id, MessageId::new(), 1, &tx)
            .await
    });
    let mut events = Vec::new();
    while let Some(emission) = rx.next().await {
        match emission {
            ClaimedAgentEvent::Pending { event, .. } => events.push(event),
            ClaimedAgentEvent::Committed { event, .. }
            | ClaimedAgentEvent::Recovered { event, .. } => events.push(event.event),
            ClaimedAgentEvent::Flush(acknowledge) => {
                let _ = acknowledge.send(());
            }
        }
    }
    (handle.await.unwrap().unwrap(), events)
}

#[tokio::test]
async fn claimed_foreground_agent_returns_exact_ordered_wait_checkpoint() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("wait.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "wait for both")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let arguments = r#"{"agent_ids":["00000000-0000-0000-0000-000000000002","00000000-0000-0000-0000-000000000001"]}"#;
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::WAIT_FOR_AGENTS_TOOL,
            arguments,
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    let AgentTurnOutcome::WaitForAgents {
        request,
        steer_revision,
        model_steps,
        ..
    } = outcome
    else {
        panic!("foreground agent should return an ordered wait checkpoint");
    };
    assert_eq!(request.provider_id, "native_1");
    assert_eq!(
        request.arguments,
        serde_json::from_str::<Value>(arguments).unwrap()
    );
    assert_eq!(
        request.child_run_ids,
        [
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000001",
        ]
        .map(|id| AgentRunId(uuid::Uuid::parse_str(id).unwrap()))
    );
    assert!(request.is_well_formed());
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
}

#[tokio::test]
async fn a_mixed_batch_runs_the_server_call_then_checkpoints_the_client_one() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.txt"), "sibling result").unwrap();
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    registry.register(Box::new(ReadFile));
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: true,
            sibling_call: true,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);

    // This batch used to be refused twice and then fail the turn, throwing
    // away the preamble and the sibling's finished work each time. Now the
    // server call runs and commits, and the client call still leaves as the
    // step's checkpoint — a checkpoint that carries exactly one call.
    let AgentTurnOutcome::ClientToolCall {
        request,
        model_steps,
        ..
    } = outcome
    else {
        panic!("the client call should still reach its checkpoint: {outcome:?}");
    };
    assert_eq!(request.name, "connect_folder");
    assert_eq!(model_steps, 1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant
                && message.content.contains("I will connect it")),
        "the preamble should survive the checkpoint: {messages:?}"
    );
    // The sibling is terminal before the turn suspends, so the resuming
    // attempt finds nothing pending to guess about.
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert_eq!(calls[0].result.as_deref(), Some("sibling result"));
}

/// Arguments the loop cannot parse used to discard the step and count
/// towards failing the turn. They are a property of the one call, so they
/// are answered like any other bad call: the model is told what was wrong
/// and keeps the step it already spent.
#[tokio::test]
async fn a_client_call_with_unparseable_arguments_is_answered_not_discarded() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: "connect_folder",
            arguments: "{not json",
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);

    // Nothing to check point on: the call never became a request, so the
    // turn runs out its steps rather than suspending on a malformed one.
    assert!(
        !matches!(outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "a call that could not be parsed must not reach a checkpoint: {outcome:?}"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    let completions: Vec<&ToolOutput> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 1, "{completions:?}");
    assert!(completions[0].is_error);
    assert!(
        completions[0].content.contains("not valid JSON"),
        "the model should be told what to fix: {completions:?}"
    );
    // Declined before it ran, so there is no record for a resume to find.
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

/// A large result used to be cut to the feedback budget *before* it was
/// written down, so the remainder was destroyed rather than withheld and
/// the record's own 512 KiB cap was unreachable. Storage and context budget
/// are different questions and now have different bounds.
#[test]
fn a_large_result_is_kept_whole_in_the_record_and_cut_only_for_the_model() {
    let feedback = DEFAULT_MAX_TOOL_RESULT_BYTES;
    let durable = crate::model::ToolCallRecord::MAX_RESULT_BYTES;
    assert!(
        durable > feedback,
        "the record must hold more than one turn feeds"
    );

    // Bigger than the feedback budget, smaller than the record's cap: this
    // is the whole class of result that used to lose its tail.
    let content = "x".repeat(feedback * 2);
    assert!(content.len() < durable);
    assert_eq!(truncate_to_bytes(&content, durable, None), None);

    let call_id = CallId::new();
    let for_model =
        truncate_to_bytes(&content, feedback, Some(call_id)).expect("exceeds the budget");
    assert!(for_model.len() < content.len());
    assert!(for_model.contains("[truncated:"));
    assert!(for_model.contains(&content.len().to_string()));
    // The notice names the call, so the cut is a next step rather than a
    // dead end.
    assert!(for_model.contains("read_tool_result"));
    assert!(for_model.contains(&call_id.to_string()));
}

#[test]
fn a_resumed_transcript_is_bounded_like_a_live_one() {
    // The record may now hold more than a turn can afford to re-read, so
    // rebuilding has to apply the feedback bound too — otherwise resuming
    // would feed the model something the original step never did.
    let oversized = "y".repeat(DEFAULT_MAX_TOOL_RESULT_BYTES * 2);
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: ChatId::new(),
        turn_id: TurnId::new(),
        provider_id: "call-1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some(oversized.clone()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: Utc::now(),
        resolved_at: Some(Utc::now()),
    };
    let rebuilt = rebuild_transcript(&[], &[call], &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    let found = rebuilt.iter().find_map(|message| {
        message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
    });
    let content = found.expect("the resumed transcript replays the result");
    assert!(content.len() < oversized.len());
    assert!(content.contains("[truncated:"));
}

/// The host's own `web_search`. Registered so the turn has one to withhold,
/// and never expected to run.
struct HostWebSearch;

#[async_trait]
impl Tool for HostWebSearch {
    fn spec(&self) -> ToolSpec {
        crate::web_search_tool_spec()
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        panic!("a search the provider already ran must never be dispatched")
    }
}

/// What one request advertised, and the vendor budget it carried.
type SearchSurfaces = Arc<Mutex<Vec<(Vec<String>, Option<VendorWebSearch>)>>>;

/// Answers with a search it already ran, recording what each request
/// advertised and asked for.
struct VendorSearchProvider {
    seen: SearchSurfaces,
}

#[async_trait]
impl ModelProvider for VendorSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push((
            req.tools.iter().map(|tool| tool.name.clone()).collect(),
            req.vendor_web_search,
        ));
        Ok(stream::iter(vec![
            ProviderEvent::ProviderExecutedToolCall {
                name: crate::WEB_SEARCH_TOOL.into(),
                input: serde_json::json!({ "query": "openwave release notes" }),
                output: serde_json::json!({
                    "provider": "anthropic",
                    "results": [{
                        "url": "https://www.example.com/notes",
                        "title": "Release notes",
                        "snippet": "what shipped",
                    }],
                }),
                is_error: false,
                replay: None,
            },
            ProviderEvent::TextDelta {
                text: "here is what I found".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// A vendor turn end to end: the model is offered one web search rather
/// than two, the search the provider ran is kept like any other tool call,
/// and a later turn replays it as an ordinary pair.
#[tokio::test]
async fn a_provider_executed_search_replaces_the_host_tool_and_is_kept_like_one() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let budget = VendorWebSearch { max_uses: 3 };
    let agent = Agent::new(
        Arc::new(VendorSearchProvider { seen: seen.clone() }),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(HostWebSearch))
                .with(Box::new(ReadFile)),
        ),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            web_search: TurnWebSearch::Vendor(budget),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "what shipped?", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // One capability, one name: the request carries the vendor budget and
    // withholds the host tool, while the rest of the surface is untouched.
    let requests = seen.lock().unwrap().clone();
    let (advertised, vendor) = requests.first().expect("the turn made a model call");
    assert_eq!(*vendor, Some(budget));
    assert!(
        !advertised.contains(&crate::WEB_SEARCH_TOOL.to_owned())
            && advertised.contains(&"read_file".to_owned()),
        "advertised the wrong surface: {advertised:?}"
    );

    // The reader sees the search happen and finish, exactly as they would
    // a host search.
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. } if name == crate::WEB_SEARCH_TOOL
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })));

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    let [call] = &calls[..] else {
        panic!("the provider's search was not recorded: {calls:?}");
    };
    assert_eq!(call.name, crate::WEB_SEARCH_TOOL);
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(call.arguments["query"], "openwave release notes");
    assert!(call
        .result
        .as_deref()
        .is_some_and(|result| result.contains("https://www.example.com/notes")));

    // A later turn rebuilds it as the same provider-executed shape, so
    // adapters can origin-gate native replay or fall back to cleartext.
    let messages = store.list_messages(chat.id).await.unwrap();
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    let blocks: Vec<&ContentBlock> = rebuilt
        .iter()
        .flat_map(|message| message.content.iter())
        .collect();
    assert!(
        blocks.iter().any(|block| matches!(
            block,
            ContentBlock::ProviderExecutedToolCall { name, output, .. }
                if name == crate::WEB_SEARCH_TOOL
                    && output.to_string().contains("Release notes")
        )),
        "the replayed call kept no result: {rebuilt:?}"
    );
}

#[test]
fn exec_preview_blocks_follow_result_text_and_respect_model_capability() {
    let image = ImageRef {
        blob_id: uuid::Uuid::from_u128(7),
        media_type: crate::ImageMediaType::Png,
        width: 400,
        height: 300,
        byte_len: 10,
    };
    let visual = tool_result_blocks("call".into(), "done".into(), false, &[image], true);
    assert!(matches!(
        &visual[..],
        [
            ContentBlock::ToolResult { content, .. },
            ContentBlock::Image { image: attached }
        ] if content.contains("attached below") && *attached == image
    ));

    let text_only = tool_result_blocks("call".into(), "done".into(), false, &[image], false);
    assert!(matches!(
        &text_only[..],
        [ContentBlock::ToolResult { content, .. }]
            if content.contains("selected model does not accept image input")
    ));
}

/// The model narrates before it acts. Rejecting a client call for carrying
/// a preamble spent the whole step budget on a correction the model never
/// satisfied — the same failure #372 fixed for sensitive calls. The step
/// must check point instead, keeping the preamble durable across the
/// resume.
#[tokio::test]
async fn client_call_with_prose_checkpoints_and_keeps_the_preamble() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: true,
            sibling_call: false,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();

    // One step, not an exhausted budget: the call reached its checkpoint.
    let AgentTurnOutcome::ClientToolCall {
        request,
        model_steps,
        ..
    } = outcome
    else {
        panic!("expected a client tool checkpoint, got {outcome:?}");
    };
    assert_eq!(request.name, "connect_folder");
    assert_eq!(model_steps, 1);

    // The preamble is durable, so the resumed attempt rebuilds it.
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant
                && message.content.contains("I will connect it")),
        "the assistant preamble should survive the checkpoint: {messages:?}"
    );
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
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
            tool_scratch: Some(tool_scratch(workspace.path())),
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

    // User input and the final answer are text messages; the tool call is
    // a structured row (not Role::Tool).
    let stored = store.list_messages(chat.id).await.unwrap();
    let roles: Vec<Role> = stored.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].result.as_deref(), Some("hello from disk"));
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert!(calls[0].resolved_at.is_some());
}

#[tokio::test]
async fn claimed_turn_defers_terminal_publication_to_durable_worker() {
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "read note.txt")
        .await
        .unwrap();
    let claimed_at = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    assert_eq!(claimed.id, turn_id);

    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let output_message_id = MessageId::new();
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);

    let AgentTurnOutcome::Completed {
        output,
        usage,
        stop_reason,
        ..
    } = outcome
    else {
        panic!("claimed turn should complete");
    };
    assert_eq!(output.id, output_message_id);
    assert_eq!(output.chat_id, chat.id);
    assert_eq!(output.turn_id, turn_id);
    assert_eq!(output.role, Role::Assistant);
    assert_eq!(output.content, "done");
    assert_eq!((usage.input_tokens, usage.output_tokens), (8, 6));
    assert_eq!(stop_reason, StopReason::EndTurn);
    assert!(
        events.iter().all(|event| !matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnCancelled { .. }
        )),
        "the worker owns lifecycle events around the durable execution boundary"
    );

    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.len(), 1, "accepted input must not be duplicated");
    assert_eq!(stored[0].role, Role::User);
    assert_eq!(stored[0].content, "read note.txt");
    assert!(
        stored.iter().all(|message| message.id != output_message_id),
        "final output must remain unpublished until atomic completion"
    );
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].turn_id, turn_id);

    for (index, event) in events.iter().enumerate() {
        let ordinal = i32::try_from(index + 1).unwrap();
        assert_eq!(
            store
                .append_turn_event(chat.id, turn_id, lease_token, ordinal, Utc::now(), event,)
                .await
                .unwrap(),
            Some(i64::from(ordinal))
        );
    }

    let completed = store
        .complete_turn_run_and_append_event(
            turn_id,
            lease_token,
            0,
            Utc::now(),
            &output,
            usage,
            stop_reason,
        )
        .await
        .unwrap()
        .expect("the live worker lease can publish its prepared output");
    assert!(matches!(
        completed.outcome,
        crate::CompleteTurnRunOutcome::Completed(_)
    ));
    let terminal = completed
        .terminal_event
        .expect("completion must return its committed terminal event");
    assert_eq!(terminal.seq, i64::try_from(events.len() + 1).unwrap());
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCompleted { usage, stop_reason }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .complete_turn_run_and_append_event(
            turn_id,
            lease_token,
            0,
            claimed_at + chrono::Duration::hours(1),
            &output,
            usage,
            stop_reason,
        )
        .await
        .unwrap()
        .expect("an exact completion retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::CompleteTurnRunOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));
    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].id, output.id);
    assert_eq!(stored[1].chat_id, output.chat_id);
    assert_eq!(stored[1].turn_id, output.turn_id);
    assert_eq!(stored[1].role, output.role);
    assert_eq!(stored[1].content, output.content);
    assert_eq!(
        stored[1].created_at.timestamp_micros(),
        output.created_at.timestamp_micros()
    );

    let failed_turn_id = TurnId::new();
    store
        .accept_turn(
            failed_turn_id,
            chat.id,
            "fake",
            "fail before calling the model",
        )
        .await
        .unwrap();
    let failure_claimed_at = Utc::now();
    let failure_token = uuid::Uuid::new_v4();
    let failed_claim = store
        .claim_turn_run(
            failure_token,
            failure_claimed_at,
            failure_claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("second accepted turn is claimable");
    assert_eq!(failed_claim.id, failed_turn_id);
    let failing_agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );
    let (failure_tx, failure_rx) = unbounded();
    // An invalid first event ordinal fails execution before any event.
    let error = failing_agent
        .run_claimed_turn(&chat, failed_turn_id, MessageId::new(), 0, &failure_tx)
        .await
        .expect_err("the identity guard fails execution");
    drop(failure_tx);
    let failure_events = emitted_events(failure_rx.collect().await);
    assert!(failure_events.iter().all(|event| !matches!(
        event,
        AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed { .. }
    )));
    let error_detail = error.to_string();
    let failure = store
        .record_turn_run_failure_and_append_event(
            failed_turn_id,
            failure_token,
            Utc::now(),
            crate::TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "agent_error",
            Some(&error_detail),
        )
        .await
        .unwrap()
        .expect("the worker can record failure before publishing its event");
    assert!(matches!(
        failure.outcome,
        crate::RecordTurnFailureOutcome::Recorded(_)
    ));
    let terminal = failure
        .terminal_event
        .expect("terminal failure must return its committed event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnFailed {
            error: crate::AgentErrorInfo {
                kind: "agent_error".into(),
                message: error_detail.clone(),
            }
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .record_turn_run_failure_and_append_event(
            failed_turn_id,
            failure_token,
            failure_claimed_at + chrono::Duration::hours(1),
            crate::TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "agent_error",
            Some(&error_detail),
        )
        .await
        .unwrap()
        .expect("an exact terminal failure retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::RecordTurnFailureOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));

    let cancelled_turn_id = TurnId::new();
    store
        .accept_turn(
            cancelled_turn_id,
            chat.id,
            "fake",
            "cancel before calling the model",
        )
        .await
        .unwrap();
    let cancellation_claimed_at = Utc::now();
    let cancellation_token = uuid::Uuid::new_v4();
    let cancelled_claim = store
        .claim_turn_run(
            cancellation_token,
            cancellation_claimed_at,
            cancellation_claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("third accepted turn is claimable");
    assert_eq!(cancelled_claim.id, cancelled_turn_id);
    assert!(matches!(
        store
            .request_turn_cancellation_and_append_event(cancelled_turn_id, Utc::now())
            .await
            .unwrap(),
        Some(crate::JournaledTurnOutcome {
            outcome: crate::RequestTurnCancellationOutcome::Requested(_),
            terminal_event: None,
        })
    ));

    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled_agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel);
    let (cancellation_tx, cancellation_rx) = unbounded();
    let outcome = cancelled_agent
        .run_claimed_turn(
            &chat,
            cancelled_turn_id,
            MessageId::new(),
            1,
            &cancellation_tx,
        )
        .await
        .unwrap();
    drop(cancellation_tx);
    assert_eq!(
        outcome,
        AgentTurnOutcome::Cancelled {
            output: None,
            citations: Vec::new(),
            usage: Usage::default(),
            model_steps: 0,
        }
    );
    let cancellation_events = emitted_events(cancellation_rx.collect().await);
    assert!(cancellation_events.iter().all(|event| !matches!(
        event,
        AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed { .. }
    )));
    let cancellation = store
        .finish_turn_cancellation_and_append_event(
            cancelled_turn_id,
            cancellation_token,
            Utc::now(),
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("the exact worker acknowledgement must commit");
    assert!(matches!(
        cancellation.outcome,
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
    let terminal = cancellation
        .terminal_event
        .expect("terminal cancellation must return its committed event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCancelled {
            usage: Usage::default()
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            cancelled_turn_id,
            cancellation_token,
            cancellation_claimed_at + chrono::Duration::hours(1),
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("an exact cancellation retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::FinishTurnCancellationOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));
}

#[tokio::test]
async fn tool_context_inherits_the_chats_project_scope() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let project = Project {
        id: ProjectId::new(),
        title: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_project(&project).await.unwrap();
    let chat = Chat {
        id: ChatId::new(),
        project_id: Some(project.id),
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let observed_project = Arc::new(Mutex::new(None));
    let observed_call = Arc::new(Mutex::new(None));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(ContextRecordingTool {
        observed_project: observed_project.clone(),
        observed_call: observed_call.clone(),
    })));
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, _rx) = unbounded();
    agent.run_turn(&chat, "inspect context", &tx).await.unwrap();
    assert_eq!(*observed_project.lock().unwrap(), Some(Some(project.id)));
    assert!(
        observed_call.lock().unwrap().is_some(),
        "provider adapters need the canonical call id for reconciliation"
    );
}

/// The step budget used to be a cliff: a turn whose last budgeted step
/// asked for a tool failed with `max_steps_exceeded`, throwing away both the
/// tool work and any prose the reader could already see on screen. The
/// budget now bounds tool rounds only — one further model call, made with no
/// tools advertised so it cannot ask for another round, closes the turn with
/// a real answer.
#[tokio::test]
async fn a_turn_at_the_step_ceiling_concludes_with_an_answer() {
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    // One step of budget, and step 0 asks for a tool: the turn is at its
    // ceiling the moment that call comes back.
    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolSurfaceRecordingProvider {
            calls: AtomicUsize::new(0),
            advertised: advertised.clone(),
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
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the ceiling must not end the turn as a failure: {events:?}"
    );
    // The last budgeted step's tool still ran, and the closing answer was
    // written with its result in hand.
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .last()
            .map(|message| (message.role, message.content.as_str())),
        Some((Role::Assistant, "done")),
        "the reader keeps a real answer: {messages:?}"
    );
    // The wrap-up call carries no tool schemas, so the model has no way to
    // ask for a round the budget cannot pay for.
    let advertised = advertised.lock().unwrap().clone();
    assert_eq!(advertised.len(), 2, "one tool step, then the wrap-up");
    assert!(!advertised[0].is_empty());
    assert!(advertised[1].is_empty());
}

#[tokio::test]
async fn a_chat_only_model_never_receives_tool_schemas() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolSurfaceRecordingProvider {
            // Start on the provider's answer branch: this test is about the
            // outbound capability surface, not tool execution.
            calls: AtomicUsize::new(1),
            advertised: advertised.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store,
        AgentConfig {
            model: "chat-only".into(),
            tools_supported: false,
            ..Default::default()
        },
    );

    let (tx, _rx) = unbounded();
    agent
        .run_turn(&chat, "answer without tools", &tx)
        .await
        .unwrap();
    assert_eq!(
        advertised.lock().unwrap().as_slice(),
        &[Vec::<String>::new()]
    );
}

/// One `read_file` call per step, arguments taken from a script, then a
/// final answer once the script runs out.
struct RepeatedCallProvider {
    calls: AtomicUsize,
    scripts: Vec<&'static str>,
}

#[async_trait]
impl ModelProvider for RepeatedCallProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("repeat")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = match self.scripts.get(step) {
            Some(args) => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("call_{step}"),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: (*args).into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            None => vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A read-only tool that counts its executions, so a test can tell a call
/// that ran from one that was answered without running.
struct CountingReadTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "a counting read tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("same result"))
    }
}

fn repeated_call_agent(
    store: Arc<dyn Store>,
    ran: Arc<AtomicUsize>,
    scripts: Vec<&'static str>,
) -> Agent {
    Agent::new(
        Arc::new(RepeatedCallProvider {
            calls: AtomicUsize::new(0),
            scripts,
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CountingReadTool { ran }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
}

/// After `REPEATED_CALL_LIMIT` identical executions, further identical
/// calls are answered without dispatching the tool — and the refusal still
/// terminalizes the admitted durable row, so recovery never finds a
/// refused call pending.
#[tokio::test]
async fn the_fourth_identical_call_is_refused_instead_of_run() {
    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let same = r#"{"path":"note.txt"}"#;
    let agent = repeated_call_agent(store.clone(), ran.clone(), vec![same; 5]);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the refusal steers the model, it does not fail the turn: {events:?}"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        REPEATED_CALL_LIMIT,
        "only the streak executes; every later identical call is refused"
    );

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 5, "refused calls still get durable rows");
    for call in &calls[..REPEATED_CALL_LIMIT] {
        assert_eq!(call.status, ToolCallStatus::Completed);
    }
    // The fourth and fifth asks are both refused: re-issuing the same
    // call keeps getting the refusal until something changes.
    for call in &calls[REPEATED_CALL_LIMIT..] {
        assert_eq!(call.status, ToolCallStatus::Failed);
        assert!(
            call.result
                .as_deref()
                .is_some_and(|result| result.starts_with("not run: this exact call")),
            "the refusal is the model-facing result: {:?}",
            call.result
        );
        assert!(call.resolved_at.is_some(), "the refused row terminalizes");
    }
}

/// A different argument is a change of course: it executes, and the
/// original call earns a fresh streak afterwards.
#[tokio::test]
async fn a_changed_argument_resets_the_repeat_streak() {
    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let same = r#"{"path":"note.txt"}"#;
    let other = r#"{"path":"other.txt"}"#;
    let agent = repeated_call_agent(
        store.clone(),
        ran.clone(),
        vec![same, same, same, other, same],
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 5, "every call ran");
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert!(
        calls
            .iter()
            .all(|call| call.status == ToolCallStatus::Completed),
        "nothing was refused: {calls:?}"
    );
}

/// A provider replay block streamed on a tool-only step must gain a durable
/// empty assistant carrier, ride verbatim into the next request, and survive
/// the turn. Whether it goes on the wire is then the adapter's call — see the
/// router's replay gate.
#[tokio::test]
async fn tool_only_provider_replay_survives_the_turn_that_produced_it() {
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ReasoningRecordingProvider {
            calls: AtomicUsize::new(0),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "{events:?}"
    );

    let block = serde_json::json!({
        "type": "thinking",
        "thinking": "plan: read the note first",
        "signature": "sig-1",
    });
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one tool step, then the answer step");
        assert!(seen[0].is_empty(), "nothing to replay on the first call");
        assert_eq!(
            seen[1],
            vec![block.clone()],
            "the block reaches the next step exactly as streamed"
        );
    }

    // A second turn on a fresh agent over the same store is the reload:
    // every message comes back off disk.
    let reloaded = Agent::new(
        Arc::new(ReasoningRecordingProvider {
            calls: AtomicUsize::new(1),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    reloaded.run_turn(&chat, "and again", &tx).await.unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;
    assert_eq!(
        seen.lock().unwrap()[2],
        vec![block],
        "the persisted block comes back on the rebuilt transcript"
    );
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
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
            tool_scratch: Some(tool_scratch(workspace.path())),
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

/// Streams `counter` calls whose arguments are well-formed JSON: first a
/// shape the advertised schema forbids, then a conforming one.
struct SchemaArgsProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SchemaArgsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("schema-args")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_wrong".into(),
                    name: "strict_counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": 42}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            1 => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_right".into(),
                    name: "strict_counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": "note"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            _ => vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A read-only tool with a required, typed argument.
struct StrictCountingTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for StrictCountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "strict_counter".into(),
            description: "a read-only tool".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("counted"))
    }
}

struct SchemaRecordingTool {
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for SchemaRecordingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schema_recorder".into(),
            description: "records schema-validated arguments".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false,
                "x-fixture-constraint": {"mode": "advisory"}
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        self.calls.lock().unwrap().push(args);
        Ok(ToolOutput::text("recorded"))
    }
}

#[tokio::test]
async fn registry_dispatch_rejects_schema_mismatches_and_preserves_valid_arguments() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let registry = ToolRegistry::new().with(Box::new(SchemaRecordingTool {
        calls: calls.clone(),
    }));
    let tool = registry.get("schema_recorder").unwrap();
    let context = ToolCtx::new_legacy_workspace(
        ChatId::new(),
        None,
        std::path::PathBuf::from("unused-by-schema-recorder"),
    );

    let refused = tool
        .execute(&context, serde_json::json!({"query": 42}))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert_eq!(
        refused.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    assert!(calls.lock().unwrap().is_empty());

    // The unrecognized extension is advisory: supported constraints still
    // apply, while a conforming call crosses the boundary byte-for-byte.
    let valid = serde_json::json!({"query": "waves"});
    let accepted = tool.execute(&context, valid.clone()).await.unwrap();
    assert!(!accepted.is_error);
    assert_eq!(*calls.lock().unwrap(), vec![valid]);
}

#[test]
fn registry_refuses_schema_mismatches_for_server_and_client_tools() {
    let spec = ToolSpec {
        name: "client_schema".into(),
        description: "a schema-validated client tool".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "labels": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minContains": 1,
                    "contains": {"const": "required"}
                }
            },
            "required": ["labels"]
        }),
    };
    let mut registry = ToolRegistry::new().with(Box::new(StrictCountingTool {
        ran: Arc::new(AtomicUsize::new(0)),
    }));
    registry.register_client(spec, ApprovalClass::ReadOnly);

    assert!(registry
        .schema_mismatch("strict_counter", &serde_json::json!({"path": 42}))
        .is_some_and(|mismatch| mismatch.contains("string")));
    assert_eq!(
        registry.schema_mismatch(
            "client_schema",
            &serde_json::json!({"labels": ["other", "required"]})
        ),
        None
    );
    assert!(registry
        .schema_mismatch("client_schema", &serde_json::json!({"labels": ["other"]}))
        .is_some());
}

#[test]
fn registry_fails_open_when_a_tool_schema_does_not_compile() {
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "invalid_schema".into(),
            description: "a tool with a misconfigured schema".into(),
            input_schema: serde_json::json!({"type": "nonsense"}),
        },
        ApprovalClass::ReadOnly,
    );

    assert_eq!(
        registry.schema_mismatch("invalid_schema", &serde_json::json!({})),
        None
    );
}

#[tokio::test]
async fn arguments_violating_the_advertised_schema_are_refused_before_the_tool_runs() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let ran = Arc::new(AtomicUsize::new(0));

    let agent = Agent::new(
        Arc::new(SchemaArgsProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(StrictCountingTool { ran: ran.clone() }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "count something", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // Only the conforming call reached the tool.
    assert_eq!(ran.load(Ordering::SeqCst), 1, "exactly one call ran");
    let outputs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 2);
    let refused = outputs[0];
    assert!(refused.is_error);
    assert_eq!(
        refused.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    // The mismatch and the schema ride along so the model can re-emit.
    assert!(refused.content.contains("\"path\""), "{}", refused.content);
    assert!(!outputs[1].is_error);
}

/// A read-only tool that records whether it ran.
struct CountingTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "counter".into(),
            description: "a read-only tool".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("counted"))
    }
}

/// Streams a truncated argument fragment for `counter`, then finishes.
struct TruncatedArgsProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for TruncatedArgsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("truncated")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_counter".into(),
                    name: "counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": "note"#.into(),
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
async fn malformed_arguments_go_back_to_the_model_instead_of_running_the_tool() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let ran = Arc::new(AtomicUsize::new(0));

    let agent = Agent::new(
        Arc::new(TruncatedArgsProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CountingTool { ran: ran.clone() }))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "count something", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(ran.load(Ordering::SeqCst), 0, "tool must not have run");
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output.clone()),
            _ => None,
        })
        .expect("the call was answered");
    assert!(output.is_error);
    assert_eq!(
        output.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    // The schema rides along so the model can re-emit the call.
    assert!(output.content.contains("\"path\""), "{}", output.content);

    // The garbled fragment survives to the journal: the durable record
    // shows what the provider actually streamed, not only the coerced
    // empty object a post-hoc debugging session cannot learn from.
    let recorded = store.list_tool_calls(chat.id).await.unwrap();
    let call = recorded
        .iter()
        .find(|call| call.name == "counter")
        .expect("the refused call was still recorded");
    assert_eq!(call.arguments, serde_json::json!({}));
    assert_eq!(call.raw_arguments.as_deref(), Some(r#"{"path": "note"#));
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

/// Provider that prefaces a sensitive `boom` call with prose, then
/// finishes on the next step.
struct ProseBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ProseBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("prose-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::TextDelta {
                    text: "I'll run the sensitive tool for you.".into(),
                },
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

/// The failure that motivated #372: prose plus one sensitive call must
/// keep the preamble, persist it like any other text+tool step, and reach
/// the approval gate on the first step instead of burning the budget on
/// corrective retries.
#[tokio::test]
async fn sensitive_call_with_prose_keeps_the_preamble_and_parks() {
    use crate::approval::AutoApproveGate;

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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
    let provider = Arc::new(ProseBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        tools,
        store.clone(),
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

    // The step is never rejected or scrubbed: the streamed preamble stands.
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    // The call parks on the first step and runs once approved.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    // No corrective retry: the tool step plus the closing step.
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    // The preamble is persisted exactly once, like any other text+tool step.
    let history = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.contains("sensitive tool for you"))
            .count(),
        1
    );
}

/// Provider that asks for two sensitive calls in one step. Both run, one
/// at a time — a parked call has to be the turn's only pending row, so the
/// second is admitted only once the first is terminal, never declined.
struct SiblingBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SiblingBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sibling-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_a".into(),
                    name: "boom".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_b".into(),
                    name: "boom".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
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
async fn a_second_sensitive_call_runs_once_the_first_is_terminal() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let ran = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
    let provider = Arc::new(SiblingBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(RecordingGate {
        store: store.clone(),
        chat_id: chat.id,
        observed: observed.clone(),
    }));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // The step stands and nothing is declined: each call parks in turn and
    // runs. A sibling used to be answered with "has to run on its own",
    // which forced the model to re-ask a step later for work it had
    // already requested correctly.
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ApprovalRequired { .. }))
            .count(),
        2
    );
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    let completions: Vec<&ToolOutput> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 2, "{completions:?}");
    assert!(completions
        .iter()
        .all(|output| output.content == "boomed" && !output.is_error));
    // Both ran, so both leave a durable record.
    assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    // The recovery invariant, held at both parks: every earlier sibling is
    // terminal and the parked call is the turn's only pending row.
    let snapshots = observed.lock().unwrap().clone();
    assert_eq!(
        snapshots,
        vec![
            vec![("boom".into(), ToolCallStatus::Pending)],
            vec![
                ("boom".into(), ToolCallStatus::Completed),
                ("boom".into(), ToolCallStatus::Pending),
            ],
        ]
    );
}

/// Provider that pairs a plain server call with a sensitive one in the same
/// step, then finishes.
struct MixedBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for MixedBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("mixed-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_read".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"a.txt"}"#.into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_boom".into(),
                    name: "boom".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
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

/// One durable-record snapshot per approval registration: each row's tool
/// name and status at the instant the gate saw the request.
type GateSnapshots = Arc<Mutex<Vec<Vec<(String, ToolCallStatus)>>>>;

/// Approval gate that photographs the durable record at the instant each
/// request is registered, then approves.
struct RecordingGate {
    store: Arc<dyn Store>,
    chat_id: ChatId,
    observed: GateSnapshots,
}

impl crate::approval::ApprovalGate for RecordingGate {
    fn register(
        &self,
        _request: crate::approval::ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            let calls = self.store.list_tool_calls(self.chat_id).await.unwrap();
            self.observed.lock().unwrap().push(
                calls
                    .into_iter()
                    .map(|call| (call.name, call.status))
                    .collect(),
            );
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async { crate::approval::ApprovalDecision::Approve })
                    as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

/// The resume invariant, stated as behaviour: a call that parks on the gate
/// is the turn's only pending row. The loop no longer refuses the batch to
/// get that — it admits the sensitive call after its plain siblings have
/// resolved, so `resume_pending_server_calls` has nothing to disambiguate.
#[tokio::test]
async fn a_sensitive_call_parks_only_after_its_plain_sibling_is_terminal() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.txt"), "read first").unwrap();
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let ran = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(MixedBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(ReadFile))
                .with(Box::new(BoomTool { ran: ran.clone() })),
        ),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(RecordingGate {
        store: store.clone(),
        chat_id: chat.id,
        observed: observed.clone(),
    }));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let at_approval = observed.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        at_approval,
        vec![
            ("read_file".into(), ToolCallStatus::Completed),
            ("boom".into(), ToolCallStatus::Pending),
        ],
        "the parked call must be the only pending row"
    );
}

/// A Sensitive, standing-grantable tool (`search`) that records whether it
/// ran.
struct SearchTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "a sensitive search tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("searched"))
    }
}

/// Provider that asks for the `search` tool once, then finishes.
struct SearchProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("search")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_search".into(),
                    name: "search".into(),
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

fn search_agent(
    store: Arc<dyn Store>,
    ran: Arc<AtomicUsize>,
    grants: Arc<crate::approval::StandingGrants>,
) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SearchTool { ran })));
    // Default gate is `RefuseGate`: it rejects any call that reaches it, so
    // the tool running proves the standing grant bypassed the gate entirely.
    Agent::new(
        Arc::new(SearchProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_standing_grants(grants)
}

#[tokio::test]
async fn standing_grant_runs_sensitive_tool_without_parking() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "a covered call must not re-prompt"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "searched" && !output.is_error
    )));
}

#[tokio::test]
async fn standing_grant_for_another_chat_does_not_bypass_the_gate() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    // A grant scoped to a different chat must not cover this chat's call.
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat {
            chat_id: ChatId::new(),
        },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "an uncovered call must still park on the gate"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

async fn permission_mode_chat(
    store: &Arc<dyn Store>,
    mode: Option<crate::model::PermissionMode>,
) -> Chat {
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: mode,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    chat
}

/// A Workspace-class tool that records whether it ran.
struct WorkspaceWriteTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for WorkspaceWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "a workspace write tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Workspace
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("written"))
    }
}

/// Provider that asks for `write_file` once, then finishes.
struct WorkspaceWriteProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for WorkspaceWriteProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("workspace-write")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_write".into(),
                    name: "write_file".into(),
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

fn workspace_write_agent(store: Arc<dyn Store>, ran: Arc<AtomicUsize>) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(WorkspaceWriteTool { ran })));
    // Default gate is `RefuseGate`, so whether the tool runs is exactly
    // whether the mode kept the call off the gate.
    Agent::new(
        Arc::new(WorkspaceWriteProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
}

/// The default mode is Ask, and Ask parks Workspace-class calls: reversing
/// either half of that silently stops asking before file edits.
#[tokio::test]
async fn ask_mode_parks_workspace_writes_by_default() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, None).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalRequired { class, kind, .. }
                if *class == ApprovalClass::Workspace
                    && *kind == ToolApprovalKind::WorkspaceMayModifyFiles
        )),
        "an uncovered workspace call must park in Ask"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

/// Auto keeps today's behavior: workspace writes proceed without a card.
#[tokio::test]
async fn auto_mode_runs_workspace_writes_without_asking() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::model::PermissionMode::Auto)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "Auto must not ask before a workspace write"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// Allow bypasses the gate for Sensitive calls entirely — no card, no
/// approval row, the tool just runs. The inverse regression (Allow still
/// parking) would make the mode a lie in the other direction.
#[tokio::test]
async fn allow_mode_runs_sensitive_without_the_gate() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::model::PermissionMode::Allow)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(
        store,
        ran.clone(),
        Arc::new(crate::approval::StandingGrants::new()),
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "Allow must not park a sensitive call"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// Plan mode refuses a mutating call outright: no approval card the
/// reader could accept, no tool run. Losing either half turns "plan mode
/// is read-only" into a prompt-level suggestion.
#[tokio::test]
async fn plan_mode_refuses_workspace_writes_without_parking() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::model::PermissionMode::Plan)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "plan mode must refuse, not park: there is nothing to approve"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("plan mode")
        )),
        "the model must be told the call was refused because of plan mode"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

/// A standing grant made in another mode must not let a plan turn run a
/// mutating call: the refusal comes before grant matching on purpose.
#[tokio::test]
async fn plan_mode_standing_grant_does_not_bypass_the_refusal() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::model::PermissionMode::Plan)).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a covered sensitive call must still be refused in plan mode"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
}

/// The plan surface advertises only read-only registrations, so the model
/// is never offered a tool the turn would refuse.
#[test]
fn plan_surface_advertises_only_read_only_tools() {
    let mut tools = ToolRegistry::new()
        .with(Box::new(ReadFile))
        .with(Box::new(WorkspaceWriteTool {
            ran: Arc::new(AtomicUsize::new(0)),
        }))
        .with(Box::new(SearchTool {
            ran: Arc::new(AtomicUsize::new(0)),
        }));
    tools.register_validated_client(
        crate::read_connected_file_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_read_connected_file_arguments,
    );
    tools.register_validated_client(
        crate::write_output_to_connected_folder_tool_spec(),
        ApprovalClass::Workspace,
        crate::validate_write_output_to_connected_folder_arguments,
    );
    tools.register_validated_foreground_client(
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );
    tools.register_foreground_agent_orchestration();

    tools = tools.with(Box::new(TaskPlanStub));

    let mut names = tools
        .specs_for_surface(true, true)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    names.sort();
    // `update_task_plan` is read-only by consent class but still commits a
    // durable row, and a plan-mode turn is drafting a proposal the reader has
    // not accepted. It is carved out by name rather than by class.
    assert_eq!(
        names,
        vec!["ask_user_questions", "read_connected_file", "read_file"]
    );
    assert!(tools
        .specs_for_surface(true, false)
        .iter()
        .any(|spec| spec.name == crate::UPDATE_TASK_PLAN_TOOL));
}

/// Stands in for the server-side task-plan tool: read-only class, real write.
struct TaskPlanStub;

#[async_trait]
impl Tool for TaskPlanStub {
    fn spec(&self) -> ToolSpec {
        crate::update_task_plan_tool_spec()
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        Ok(ToolOutput::text("recorded"))
    }
}

/// A Sensitive tool that escapes the chat workspace (`exec`) and records
/// whether it ran.
struct ExecTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec".into(),
            description: "an escaping command execution tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("executed"))
    }
}

/// Provider that asks for the `exec` tool once, then finishes.
struct ExecProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ExecProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("exec")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_exec".into(),
                    name: "exec".into(),
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

fn exec_agent(store: Arc<dyn Store>, ran: Arc<AtomicUsize>, grants: Arc<StandingGrants>) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(ExecTool { ran })));
    // Default gate is `RefuseGate`: it rejects any call that reaches it, so
    // the tool running proves the standing grant bypassed the gate entirely.
    Agent::new(
        Arc::new(ExecProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_standing_grants(grants)
}

#[tokio::test]
async fn standing_grant_runs_escaping_exec_without_parking() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "exec",
        ToolApprovalKind::for_tool_name("exec"),
        Utc::now(),
    )
    .expect("exec is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = exec_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "a covered escaping call must not re-prompt"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "executed" && !output.is_error
    )));
}

#[tokio::test]
async fn ungranted_escaping_exec_still_parks_deny_by_default() {
    use crate::approval::StandingGrants;

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    // No grant covers this chat: an escaping action must still park.
    let grants = Arc::new(StandingGrants::new());

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = exec_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalRequired { kind, .. }
                if *kind == ToolApprovalKind::ExecMayRunNetworkedCommand
        )),
        "an uncovered escaping call must park on the gate with a presentable kind"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

/// Counts every execution so a test can prove a fenced tool never ran.
struct SpyTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SpyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "spy".into(),
            description: "records whether it executed".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("spied"))
    }
}

/// Asks for the `spy` tool once, but first lets the turn's lease be stolen
/// while this provider call is in flight: a fresh claim scan past the lease
/// expiry starts the retry attempt under a new token.
struct LeaseStealingProvider {
    store: Arc<dyn Store>,
    steal_at: DateTime<Utc>,
    stole: AtomicUsize,
}

#[async_trait]
impl ModelProvider for LeaseStealingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("lease-steal")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        if self.stole.fetch_add(1, Ordering::SeqCst) == 0 {
            let outcome = self
                .store
                .claim_turn_run(
                    uuid::Uuid::new_v4(),
                    self.steal_at,
                    self.steal_at + chrono::Duration::minutes(1),
                )
                .await?;
            assert!(
                outcome.turn.is_some(),
                "expired turn should be reclaimed for a retry by the steal"
            );
        }
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "call_spy".into(),
                name: "spy".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "{}".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

struct AnswerOnlyProvider;

#[async_trait]
impl ModelProvider for AnswerOnlyProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("answer-only")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "recovered".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

struct RefusalProvider(Vec<ProviderEvent>);

#[async_trait]
impl ModelProvider for RefusalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("refusal")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(self.0.clone()).boxed())
    }
}

async fn run_claimed_refusal(events: Vec<ProviderEvent>) -> (AgentTurnOutcome, Vec<AgentEvent>) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("refusal.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "question")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease_token,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    let agent = Agent::new(
        Arc::new(RefusalProvider(events)),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let journaled = rx
        .filter_map(|item| async move {
            match item {
                ClaimedAgentEvent::Pending { event, .. } => Some(event),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .await;
    (outcome, journaled)
}

#[tokio::test]
async fn foreground_refusal_distinguishes_empty_partial_and_bare_events() {
    let (empty, empty_events) = run_claimed_refusal(vec![ProviderEvent::Refusal {
        details: RefusalDetails::from_category(Some("cyber")),
    }])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = empty
    else {
        panic!("structured empty refusal should complete as refused");
    };
    assert_eq!(output.content, "");
    assert_eq!(refusal.category(), Some("cyber"));
    assert!(!refusal.partial_output());
    assert!(
        !empty_events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)),
        "a refusal with no started tool calls has nothing to discard"
    );

    let (partial, _) = run_claimed_refusal(vec![
        ProviderEvent::TextDelta {
            text: "A partial answer".into(),
        },
        ProviderEvent::Refusal {
            details: RefusalDetails::from_category(Some("general_harms")),
        },
    ])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = partial
    else {
        panic!("structured mid-stream refusal should complete as refused");
    };
    assert_eq!(output.content, "A partial answer");
    assert_eq!(refusal.category(), Some("general_harms"));
    assert!(refusal.partial_output());

    let (bare, _) = run_claimed_refusal(vec![ProviderEvent::Stop {
        reason: StopReason::Refusal,
    }])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = bare
    else {
        panic!("bare refusal stop should use default metadata");
    };
    assert_eq!(output.content, "");
    assert_eq!(refusal.category(), None);
    assert!(!refusal.partial_output());

    // Calls that started before the refusal were already journaled, so the
    // refusal has to mark them discarded or replay is left holding a call
    // that never resolves.
    let (with_calls, call_events) = run_claimed_refusal(vec![
        ProviderEvent::ToolCallStarted {
            index: 0,
            id: "call-0".into(),
            name: "echo".into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            index: 0,
            fragment: "{\"text\"".into(),
        },
        ProviderEvent::Refusal {
            details: RefusalDetails::from_category(Some("cyber")),
        },
    ])
    .await;
    assert!(
        matches!(
            with_calls,
            AgentTurnOutcome::Completed {
                stop_reason: StopReason::Refusal,
                ..
            }
        ),
        "a refusal mid tool call still completes as refused"
    );
    let started = call_events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStarted { .. }));
    let interrupted = call_events
        .iter()
        .position(|e| matches!(e, AgentEvent::StreamInterrupted));
    assert!(
        matches!((started, interrupted), (Some(a), Some(b)) if a < b),
        "the started call is marked discarded by the refusal"
    );
}

/// The in-process driver must not report success for a turn whose final
/// model response has neither text nor a tool call: the caller gets a
/// blank turn with nothing to act on and no error to explain it. The
/// worker refuses the same response (its disposition is to retry while
/// budgets allow); the in-process driver has no attempt accounting, so
/// the turn fails instead of completing.
#[tokio::test]
async fn an_empty_model_response_does_not_complete_an_in_process_turn() {
    struct EmptyProvider;

    #[async_trait]
    impl ModelProvider for EmptyProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("empty")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }])
            .boxed())
        }
    }

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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(EmptyProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    let result = agent.run_turn(&chat, "say something", &tx).await;
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(result.is_err());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCompleted { .. })),
        "an empty response must not complete the turn"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed { .. })),
        "the failure surfaces as TurnFailed"
    );
}

/// A mid-stream provider failure must keep the classification the
/// equivalent HTTP-status failure would have had: an in-band overload
/// surfaces to the client as `overloaded`, not the generic `provider`.
#[tokio::test]
async fn a_mid_stream_failure_reaches_the_client_with_its_classification() {
    struct OverloadedProvider;

    #[async_trait]
    impl ModelProvider for OverloadedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("overloaded")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "partial".into(),
                },
                ProviderEvent::Failed {
                    error: ProviderErrorInfo::from_error(&AgentError::Overloaded(
                        "anthropic returned 500 (overloaded_error)".into(),
                    )),
                },
            ])
            .boxed())
        }
    }

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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(OverloadedProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    let result = agent.run_turn(&chat, "say something", &tx).await;
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(
        result.unwrap_err().kind(),
        "overloaded",
        "the turn fails under the classified kind"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TurnFailed { error } if error.kind == "overloaded"
        )),
        "the classification reaches the client on TurnFailed"
    );
}

#[tokio::test]
async fn a_mid_stream_context_overflow_restarts_after_discarding_the_candidate() {
    struct OverflowThenAnswer {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for OverflowThenAnswer {
        fn id(&self) -> ProviderId {
            ProviderId::new("overflow-then-answer")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            let first = requests.len() == 1;
            drop(requests);

            let events = if first {
                vec![
                    ProviderEvent::TextDelta {
                        text: "discard me".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "partial-call".into(),
                        name: "missing_tool".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{\"unfinished\":".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 11,
                        output_tokens: 3,
                        ..Usage::default()
                    }),
                    ProviderEvent::Failed {
                        error: ProviderErrorInfo::from_error(&AgentError::PromptTooLong(
                            "context overflow".into(),
                        )),
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "recovered".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 7,
                        output_tokens: 2,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(OverflowThenAnswer {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            context_window: 64,
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, &"word ".repeat(200), &tx)
        .await
        .unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    let request_tokens = {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "the same model step is retried once");
        [
            context::estimate_transcript_tokens(&requests[0].messages),
            context::estimate_transcript_tokens(&requests[1].messages),
        ]
    };
    assert!(
        request_tokens[1] < request_tokens[0],
        "the retry uses the next reduction level"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StreamInterrupted))
            .count(),
        1,
        "clients clear the abandoned prose and tool call before the retry"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { text } if text == "recovered")));
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TurnCompleted {
                usage: Usage {
                    input_tokens: 18,
                    output_tokens: 5,
                    ..
                },
                ..
            }
        )),
        "usage includes provider work from the discarded attempt"
    );
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .last()
            .unwrap()
            .content,
        "recovered",
        "only the successful candidate is persisted"
    );
}

#[tokio::test]
async fn a_stolen_lease_fences_intermediate_tool_effects() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "go")
        .await
        .unwrap();
    let now = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
    let agent = Agent::new(
        Arc::new(LeaseStealingProvider {
            store: store.clone(),
            // The steal reads a claim time past the lease expiry, so the
            // scan reclaims and terminalizes the turn deterministically.
            steal_at: now + chrono::Duration::minutes(2),
            stole: AtomicUsize::new(0),
        }),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(lease_token);

    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let _ = rx.collect::<Vec<_>>().await;

    // The stale segment refuses to persist tool-call rows or run the tool.
    assert!(
        matches!(outcome, AgentTurnOutcome::Failed { .. }),
        "a stolen lease must not complete the turn: {outcome:?}"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a stolen lease must not execute tool side effects"
    );
    // The retry claim stands; the stale worker committed nothing.
    let turn = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Running);
    assert_ne!(turn.lease_token, Some(lease_token));
}

#[tokio::test]
async fn retry_abandons_an_inherited_pending_tool_without_replaying_it() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "go")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance: {outcome:?}"),
    };
    let first_claim_at = accepted.available_at;
    let first_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            first_lease,
            first_claim_at,
            first_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call_id = CallId::new();
    let call = ToolCallRecord {
        id: call_id,
        chat_id: chat.id,
        turn_id,
        provider_id: "call_spy".into(),
        name: "spy".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: first_claim_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, first_lease, first_claim_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));

    // Simulate a crash after acceptance and possible execution but before
    // result commit. Reclaiming creates the next failure attempt.
    let retry_at = first_claim_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    let retried = store
        .claim_turn_run(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(retried.attempt_count, 2);

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
    let agent = Agent::new(
        Arc::new(AnswerOnlyProvider),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(retry_lease);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let _ = rx.collect::<Vec<_>>().await;

    assert!(matches!(outcome, AgentTurnOutcome::Completed { .. }));
    assert_eq!(ran.load(Ordering::SeqCst), 0, "pending work was replayed");
    let stored = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.id == call_id)
        .unwrap();
    assert_eq!(stored.status, ToolCallStatus::Failed);
    assert_eq!(
        stored.error_code.as_deref(),
        Some("tool_execution_interrupted")
    );
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
    fn register(
        &self,
        _request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            if let Some(tx) = self.armed.lock().unwrap().take() {
                let _ = tx.send(());
            }
            crate::approval::ApprovalRegistration {
                decision: Box::pin(future::pending()) as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

/// Trips cancel, then resolves Approve immediately — both arms of the
/// approval `select` are ready in the same poll. Without a cancel-preferring
/// check, `select` would take Approve and the Sensitive tool would run.
struct CancelThenApproveGate {
    cancel: CancelToken,
}

impl ApprovalGate for CancelThenApproveGate {
    fn register(
        &self,
        _request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            self.cancel.cancel();
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async { ApprovalDecision::Approve })
                    as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (store, chat, workspace)
}

struct ToolFutureDropMarker(Arc<AtomicBool>);

impl Drop for ToolFutureDropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct BlockingTool {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for BlockingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "blocking".into(),
            description: "wait until the turn is cancelled".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let _drop = ToolFutureDropMarker(self.dropped.clone());
        self.entered.notify_one();
        future::pending().await
    }
}

struct BlockingToolProvider;

#[async_trait]
impl ModelProvider for BlockingToolProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("blocking-tool")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "blocking_1".into(),
                name: "blocking".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "{}".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn parallel_read_results_stay_ordered_even_when_a_failure_finishes_first() {
    struct SlowRead {
        started: Arc<tokio::sync::Notify>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Tool for SlowRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "slow_read".into(),
                description: "a deliberately delayed read".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.started.notify_one();
            let release = self
                .release
                .lock()
                .unwrap()
                .take()
                .expect("slow read runs once");
            release.await.expect("test releases the slow read");
            Ok(ToolOutput::text("slow result"))
        }
    }

    struct FastFailingRead;

    #[async_trait]
    impl Tool for FastFailingRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fast_read".into(),
                description: "a read that fails immediately".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            Ok(ToolOutput::error("fast read failed"))
        }
    }

    struct ParallelReadProvider {
        calls: AtomicUsize,
        received_results: Arc<Mutex<Vec<(String, String, bool)>>>,
    }

    #[async_trait]
    impl ModelProvider for ParallelReadProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("parallel-read")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "slow_call".into(),
                        name: "slow_read".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 1,
                        id: "fast_call".into(),
                        name: "fast_read".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 1,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                let results = request
                    .messages
                    .last()
                    .expect("the second request includes the tool results")
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => Some((tool_use_id.clone(), content.clone(), *is_error)),
                        _ => None,
                    })
                    .collect();
                *self.received_results.lock().unwrap() = results;
                vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let slow_started = Arc::new(tokio::sync::Notify::new());
    let (release_slow, slow_release) = oneshot::channel();
    let received_results = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ParallelReadProvider {
            calls: AtomicUsize::new(0),
            received_results: received_results.clone(),
        }),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(SlowRead {
                    started: slow_started.clone(),
                    release: Mutex::new(Some(slow_release)),
                }))
                .with(Box::new(FastFailingRead)),
        ),
        store.clone(),
        AgentConfig {
            model: "parallel-read".into(),
            ..Default::default()
        },
    );

    let chat_id = chat.id;
    let (tx, mut rx) = unbounded();
    let turn = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    slow_started.notified().await;
    let first_completion = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(AgentEvent::ToolCallCompleted { output, .. }) = rx.next().await {
                break output;
            }
        }
    })
    .await
    .expect("the fast call must finish before the slow call is released");
    assert!(first_completion.is_error);
    assert_eq!(first_completion.content, "fast read failed");
    release_slow.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(1), turn)
        .await
        .expect("the released read finishes the turn")
        .unwrap()
        .unwrap();
    assert_eq!(
        *received_results.lock().unwrap(),
        vec![
            ("slow_call".into(), "slow result".into(), false),
            ("fast_call".into(), "fast read failed".into(), true),
        ],
        "the next model request keeps the provider's requested order"
    );
    assert!(
        store
            .list_tool_calls(chat_id)
            .await
            .unwrap()
            .iter()
            .all(|call| call.status.is_terminal()),
        "a failed sibling cannot leave the slow call pending"
    );
}

#[tokio::test]
async fn cancellation_drops_every_parallel_read_future() {
    struct ParallelBlockingRead {
        name: &'static str,
        entered: Arc<AtomicUsize>,
        both_entered: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicUsize>,
    }

    struct CountDrop(Arc<AtomicUsize>);

    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Tool for ParallelBlockingRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "waits for cancellation".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            let _drop = CountDrop(self.dropped.clone());
            if self.entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                self.both_entered.notify_one();
            }
            future::pending().await
        }
    }

    struct ParallelBlockingProvider;

    #[async_trait]
    impl ModelProvider for ParallelBlockingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("parallel-blocking")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "blocking_a".into(),
                    name: "blocking_a".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "blocking_b".into(),
                    name: "blocking_b".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let both_entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        Arc::new(ParallelBlockingProvider),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(ParallelBlockingRead {
                    name: "blocking_a",
                    entered: entered.clone(),
                    both_entered: both_entered.clone(),
                    dropped: dropped.clone(),
                }))
                .with(Box::new(ParallelBlockingRead {
                    name: "blocking_b",
                    entered: entered.clone(),
                    both_entered: both_entered.clone(),
                    dropped: dropped.clone(),
                })),
        ),
        store,
        AgentConfig {
            model: "parallel-blocking".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, rx) = unbounded();
    let turn = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    let both_started =
        tokio::time::timeout(std::time::Duration::from_secs(1), both_entered.notified()).await;
    cancel.cancel();
    both_started.expect("both read-only calls should begin together");
    tokio::time::timeout(std::time::Duration::from_secs(1), turn)
        .await
        .expect("cancellation stops every parallel read")
        .unwrap()
        .unwrap();

    let events = rx.collect::<Vec<_>>().await;
    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AgentEvent::ToolCallCompleted { output, .. }
                    if output.is_error && output.content == "turn cancelled during tool execution"
            ))
            .count(),
        2,
        "every admitted read receives a terminal cancellation result"
    );
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

struct SemanticCheckpointProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    summary_calls: Arc<AtomicUsize>,
    foreground_calls: Arc<AtomicUsize>,
    malformed_summary: bool,
    tool_first: bool,
}

#[async_trait]
impl ModelProvider for SemanticCheckpointProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("semantic-checkpoint")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let maintenance = request.system.as_deref() == Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT);
        self.requests.lock().unwrap().push(request);
        if maintenance {
            self.summary_calls.fetch_add(1, Ordering::SeqCst);
            let text = if self.malformed_summary {
                "not a structured checkpoint"
            } else {
                r#"{"version":2,"original_requests":[],"confirmed_decisions":["Use the durable SQLite path."],"unresolved_questions":["Confirm the rollout date."],"task_state":["Migration implementation is in progress."],"source_identities":["source:decision-doc"],"output_identities":["output:migration-plan"],"conclusions":["The local path preserves exact retries."]}"#
            };
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: text.into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_read_input_tokens: 10,
                    cache_creation_input_tokens: 5,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }

        let call = self.foreground_calls.fetch_add(1, Ordering::SeqCst);
        if self.tool_first && call == 0 {
            return Ok(stream::iter(vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                }),
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "checkpoint_tool_1".into(),
                    name: "checkpoint_noop".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed());
        }
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
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

/// What the host resolves for maintenance work. Deliberately not the
/// conversation model, so a maintenance request is identifiable by its
/// model alone.
fn test_utility_model() -> UtilityModel {
    UtilityModel {
        provider: None,
        model: "utility-model".into(),
        reasoning_model: false,
        reasoning_effort: None,
        context_window: 3_000,
    }
}

/// Production defaults floor the trigger at 50k tokens and protect the five
/// newest rows, which no small-window test can reach. These keep the same
/// percentage hysteresis while letting a few-thousand-token transcript cross
/// the threshold and still leave a compactable prefix.
fn test_compaction_policy() -> CompactionPolicy {
    CompactionPolicy {
        threshold_fraction: 0.75,
        target_fraction: 0.25,
        min_threshold_tokens: 0,
        protect_recent_messages: 2,
    }
}

async fn append_semantic_checkpoint_history(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
) -> Vec<Message> {
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::User,
            reasoning: Default::default(),
            content: format!(
                "OLD PREFIX: choose the durable SQLite path. {}",
                "historical detail ".repeat(1_200)
            ),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "OLD ASSISTANT: SQLite is confirmed; source:decision-doc.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::User,
            reasoning: Default::default(),
            content: "RECENT USER: keep this exchange raw.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "RECENT ASSISTANT: this is the newest completed exchange.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
    ];
    for message in &messages {
        store.append_message(message).await.unwrap();
    }
    messages
}

#[tokio::test]
async fn creates_projects_and_deduplicates_a_structured_semantic_checkpoint() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let history = append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let foreground_calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(SemanticCheckpointProvider {
        requests: requests.clone(),
        summary_calls: summary_calls.clone(),
        foreground_calls: foreground_calls.clone(),
        malformed_summary: false,
        tool_first: true,
    });
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            utility_model: Some(test_utility_model()),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "CURRENT USER: continue the migration.", &tx)
        .await
        .unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        foreground_calls.load(Ordering::SeqCst),
        2,
        "a second foreground tool step must not recursively summarize"
    );
    let checkpoint = store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .expect("the reduced prefix is checkpointed");
    assert_eq!(
        checkpoint.source_message_id, history[0].id,
        "compaction cuts back to the oldest row the raw-history target cannot keep"
    );
    assert_eq!(
        checkpoint.format_version,
        crate::CONTEXT_CHECKPOINT_FORMAT_V2
    );
    assert_eq!(
        checkpoint.usage,
        Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 5,
        },
        "maintenance usage is durable on the checkpoint"
    );
    let payload: ContextCheckpointPayloadV2 = serde_json::from_str(&checkpoint.content).unwrap();
    assert_eq!(
        payload.confirmed_decisions,
        ["Use the durable SQLite path."]
    );
    // The host, not the summarizer, owns `original_requests`: the founding ask
    // in the compacted prefix has to survive even though the model returned an
    // empty list.
    assert!(
        payload
            .original_requests
            .iter()
            .any(|request| request.contains("OLD PREFIX")),
        "the compacted user ask carries forward: {:?}",
        payload.original_requests
    );

    let requests = requests.lock().unwrap();
    let maintenance = requests
        .iter()
        .find(|request| request.system.as_deref() == Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT))
        .expect("one maintenance request");
    assert!(maintenance.tools.is_empty());
    assert!(maintenance.images.is_empty());
    // The call constrains its own output, and the schema it sends has to
    // survive the conversion every adapter runs it through — a payload field
    // that cannot be expressed strictly would fail every checkpoint call
    // rather than degrade to prose.
    let Some(crate::provider::ResponseFormat::JsonSchema { name, schema }) =
        &maintenance.response_format
    else {
        panic!("the checkpoint call asks for a constrained payload");
    };
    assert_eq!(name, "context_checkpoint");
    assert!(
        crate::tool::strict_json_schema(schema, crate::tool::OptionalProperties::AcceptNull)
            .is_some(),
        "the checkpoint payload schema has a strict form: {schema}"
    );
    let maintenance_debug = format!("{:?}", maintenance.messages);
    assert!(maintenance_debug.contains("OLD PREFIX"));
    assert!(!maintenance_debug.contains("RECENT USER"));
    assert!(!maintenance_debug.contains(CHECKPOINT_CONTEXT_PREFIX));

    let foreground = requests
        .iter()
        .filter(|request| request.system.as_deref() != Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT))
        .collect::<Vec<_>>();
    assert!(foreground.iter().all(|request| request.messages.iter().any(
            |message| message.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text } if text.contains(CHECKPOINT_CONTEXT_PREFIX)),
            ),
        )));
    assert!(!context::has_orphaned_tool_blocks(
        &foreground.last().unwrap().messages
    ));
    assert!(foreground.last().unwrap().messages.iter().any(|message| {
            message.content.iter().any(
                |block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "checkpoint_tool_1"),
            )
        }));

    let turn_usage = events.iter().find_map(|event| match event {
        AgentEvent::TurnCompleted { usage, .. } => Some(*usage),
        _ => None,
    });
    assert_eq!(
        turn_usage,
        Some(Usage {
            input_tokens: 12,
            output_tokens: 5,
            ..Usage::default()
        }),
        "checkpoint usage is not charged to the user-visible turn"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })),
        "standing a checkpoint in for its own prefix is compaction, which the \
         compaction events report — not deterministic truncation"
    );
    let compaction: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::CompactionStarted | AgentEvent::CompactionFinished { .. }
            )
        })
        .collect();
    assert!(
        matches!(
            compaction.as_slice(),
            [
                AgentEvent::CompactionStarted,
                AgentEvent::CompactionFinished { compacted: true }
            ]
        ),
        "one compaction runs, and it reports success: {compaction:?}"
    );
}

#[tokio::test]
async fn malformed_checkpoint_summary_fails_open_to_deterministic_reduction() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let foreground_calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests,
            summary_calls: summary_calls.clone(),
            foreground_calls: foreground_calls.clone(),
            malformed_summary: true,
            tool_first: true,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            utility_model: Some(test_utility_model()),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "continue", &tx).await.unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;
    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(foreground_calls.load(Ordering::SeqCst), 2);
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnCompleted { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::CompactionFinished { compacted: false })),
        "a compaction that produced nothing still closes its status"
    );
}

/// The trigger is a fraction of the model's window, so the same history that
/// compacts on a small model has to pass untouched on a large one. At 0.75 the
/// 50k window only compacts past 37 500 tokens, which this transcript is far
/// below; the 3 000 window compacts past 2 250, which it clears.
#[tokio::test]
async fn model_window_change_recalculates_the_checkpoint_threshold() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;

    let large_summary_calls = Arc::new(AtomicUsize::new(0));
    let large_agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
            summary_calls: large_summary_calls.clone(),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "large-context-model".into(),
            context_window: 50_000,
            utility_model: Some(test_utility_model()),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    large_agent
        .run_turn(&chat, "large-window turn", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;
    assert_eq!(large_summary_calls.load(Ordering::SeqCst), 0);
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());

    let small_requests = Arc::new(Mutex::new(Vec::new()));
    let small_summary_calls = Arc::new(AtomicUsize::new(0));
    let small_agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: small_requests.clone(),
            summary_calls: small_summary_calls.clone(),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            utility_model: Some(test_utility_model()),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    small_agent
        .run_turn(&chat, "small-window turn", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;
    assert_eq!(small_summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        small_requests.lock().unwrap()[0].model,
        "utility-model",
        "maintenance runs on the utility model, not the conversation's"
    );
    assert_eq!(
        store
            .get_context_checkpoint(chat.id)
            .await
            .unwrap()
            .unwrap()
            .chat_id,
        chat.id
    );
}

#[tokio::test]
async fn oversized_transcript_emits_context_truncated() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    // Records what the provider actually received, and answers immediately.
    struct AnswerProvider {
        seen_tokens: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl ModelProvider for AnswerProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("answer")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.seen_tokens.store(
                context::estimate_transcript_tokens(&req.messages),
                Ordering::SeqCst,
            );
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let seen_tokens = Arc::new(AtomicUsize::new(0));
    // A small context window forces reduction of a large input.
    let context_window = 3000;
    let agent = Agent::new(
        Arc::new(AnswerProvider {
            seen_tokens: seen_tokens.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "answer".into(),
            context_window,
            ..Default::default()
        },
    );

    let huge = "word ".repeat(2000); // ~3300 tokens, over the ~2250 budget
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, &huge, &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    let truncated = events.iter().find_map(|e| match e {
        AgentEvent::ContextTruncated {
            original_tokens,
            fitted_tokens,
        } => Some((*original_tokens, *fitted_tokens)),
        _ => None,
    });
    let (original, fitted) = truncated.expect("ContextTruncated emitted for oversized input");
    assert!(
        fitted < original,
        "fitted {fitted} should be < original {original}"
    );
    // What actually went to the provider matches the reported fitted size and
    // is within the reduced budget.
    assert_eq!(seen_tokens.load(Ordering::SeqCst), fitted as usize);
    assert!(fitted as usize <= context::compute_message_budget(context_window, 0, None, &[]));
}

/// Compaction is a soft load boundary, not a last-resort fallback: once a
/// checkpoint covers a prefix, the model reads the checkpoint instead of that
/// prefix on every subsequent turn, however much window is available. Only a
/// boundary this transcript cannot locate falls back to the full raw history.
#[tokio::test]
async fn projects_a_checkpoint_whenever_its_boundary_is_valid() {
    struct CaptureProvider {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for CaptureProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("checkpoint-capture")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.requests.lock().unwrap().push(request);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let historical = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "old decision ".repeat(1_000),
        llm_content: None,
        created_at: Utc::now(),
    };
    store.append_message(&historical).await.unwrap();
    let checkpoint = ContextCheckpoint {
        chat_id: chat.id,
        source_message_id: historical.id,
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "The user chose the durable option.".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    store.save_context_checkpoint(&checkpoint).await.unwrap();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(CaptureProvider {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "checkpoint-capture".into(),
            context_window: 2_000,
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "What did we decide?", &tx)
        .await
        .unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    let request = requests
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("one provider request");
    let projected: Vec<_> = request
            .messages
            .iter()
            .filter(|message| {
                message.role == Role::System
                    && message.content.iter().any(|block| {
                        matches!(block, ContentBlock::Text { text } if text.contains(CHECKPOINT_CONTEXT_PREFIX))
                    })
            })
            .collect();
    assert_eq!(
        projected.len(),
        1,
        "the checkpoint is projected exactly once"
    );
    assert!(projected[0].content.iter().any(
        |block| matches!(block, ContentBlock::Text { text } if text.contains(&checkpoint.content)),
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })),
        "a projected checkpoint with a tail that fits has not truncated anything"
    );
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .all(|message| !message.content.contains(CHECKPOINT_CONTEXT_PREFIX)));
    assert!(!format!("{events:?}").contains(CHECKPOINT_CONTEXT_PREFIX));

    // A window large enough for the raw covered history changes nothing: the
    // boundary is still valid, so the prefix stays replaced by its checkpoint.
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(CaptureProvider {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "checkpoint-capture".into(),
            context_window: 50_000,
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "Please answer again.", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;
    let wide = requests
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("one provider request");
    let mentions = |needle: &str| {
        wide.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains(needle)))
        })
    };
    assert!(
        mentions(CHECKPOINT_CONTEXT_PREFIX),
        "a valid boundary projects regardless of how much window is spare"
    );
    assert!(
        !mentions("old decision"),
        "the covered prefix is not resent beside its own checkpoint"
    );

    // Fail open: a checkpoint whose source row this transcript cannot locate
    // has no boundary to stand in for, so the raw history goes out whole.
    let raw = vec![ChatMessage::text(Role::User, "What did we decide?")];
    assert_eq!(
        agent.fit_transcript(&raw, 0, Some(&checkpoint), None),
        (raw.clone(), false)
    );
}

#[tokio::test]
async fn checkpoint_fitting_preserves_tool_pairs_and_fails_closed_when_over_budget() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let config = AgentConfig {
        model: "checkpoint-fit".into(),
        context_window: 1_400,
        ..Default::default()
    };
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        config.clone(),
    );
    let transcript = vec![
        ChatMessage::text(Role::User, "old detail ".repeat(1_000)),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "decision.md"}),
            }],
            reasoning: MessageReasoning::default(),
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "the durable decision".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        },
        ChatMessage::text(Role::User, "Continue from the decision."),
    ];
    let checkpoint = ContextCheckpoint {
        chat_id: chat.id,
        source_message_id: MessageId::new(),
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "Earlier discussion selected the durable option.".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    let (fitted, reduced) = agent.fit_transcript(&transcript, 0, Some(&checkpoint), Some(1));
    assert!(
        !reduced,
        "the post-boundary tail fits beside the checkpoint, so nothing was trimmed"
    );
    assert!(matches!(
        fitted.first(),
        Some(ChatMessage {
            role: Role::System,
            ..
        })
    ));
    assert!(!context::has_orphaned_tool_blocks(&fitted));
    assert!(fitted.iter().any(|message| message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "call_1"),)));
    assert!(fitted.iter().any(|message| message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"),
        )));

    let over_budget = ContextCheckpoint {
        content: "x".repeat(crate::MAX_CONTEXT_CHECKPOINT_BYTES),
        ..checkpoint
    };
    let expected = context::fit_to_budget(
        &transcript[1..],
        context::compute_message_budget(config.context_window, 0, None, &[]),
        context::content_floor_for_level(0),
    );
    assert_eq!(
        agent.fit_transcript(&transcript, 0, Some(&over_budget), Some(1)),
        expected,
        "a checkpoint that cannot share the request budget is dropped rather than \
         crowding out the post-boundary history it was meant to summarize"
    );
}

#[test]
fn unsupported_or_foreign_checkpoints_are_not_projectable() {
    let chat_id = ChatId::new();
    let checkpoint = ContextCheckpoint {
        chat_id,
        source_message_id: MessageId::new(),
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "valid historical context".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    assert!(checkpoint_is_projectable(&checkpoint, chat_id));
    assert!(!checkpoint_is_projectable(&checkpoint, ChatId::new()));
    assert!(checkpoint_is_projectable(
        &ContextCheckpoint {
            format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V2,
            ..checkpoint.clone()
        },
        chat_id,
    ));
    assert!(!checkpoint_is_projectable(
        &ContextCheckpoint {
            format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V2 + 1,
            ..checkpoint
        },
        chat_id,
    ));
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
    // The prose the reader was already watching commits durably with the
    // cancellation, so the next model turn sees what was said (#1182).
    let messages = store.list_messages(chat_id).await.unwrap();
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    assert_eq!(messages[1].content, "partial");
}

/// The durable path's mid-stream cancel: the claimed outcome carries the
/// partial prose out for the worker to commit, and once committed the next
/// context load reads it annotated as user-stopped (#1182) while the
/// durable row keeps exactly what the user watched stream.
#[tokio::test]
async fn claimed_cancel_carries_partial_output_and_context_notes_the_stop() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "stall", "go")
        .await
        .unwrap();
    let claimed_at = Utc::now();
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");

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

    let output_message_id = MessageId::new();
    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn({
        let chat = chat.clone();
        async move {
            agent
                .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
                .await
        }
    });
    while let Some(emission) = rx.next().await {
        match emission {
            ClaimedAgentEvent::Pending {
                event: AgentEvent::TextDelta { .. },
                ..
            } => cancel.cancel(),
            ClaimedAgentEvent::Flush(ack) => {
                let _ = ack.send(());
            }
            _ => {}
        }
    }
    let outcome = handle.await.unwrap().unwrap();
    let AgentTurnOutcome::Cancelled {
        output,
        citations,
        usage,
        ..
    } = outcome
    else {
        panic!("a mid-stream cancel ends the claimed turn as cancelled: {outcome:?}")
    };
    let output = output.expect("a prose-only cancel carries its partial output");
    assert_eq!(
        (output.id, output.content.as_str()),
        (output_message_id, "partial")
    );

    // Play the worker: durably request, then acknowledge with the output.
    store
        .request_turn_cancellation(turn_id, Utc::now())
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            Utc::now(),
            usage,
            Some(&output),
            &citations,
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation with output");

    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.last().map(|m| m.content.as_str()), Some("partial"));
    let transcript = agent_for_store(&store).load_transcript(chat.id, None).await;
    let assistant_text = transcript
        .unwrap()
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("cancelled partial output reaches model context");
    assert_eq!(assistant_text, format!("partial{USER_INTERRUPTION_NOTE}"));
}

/// A throwaway agent over `store`, for exercising context loading.
fn agent_for_store(store: &Arc<dyn Store>) -> Agent {
    Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
}

struct ToolCallStallProvider;

#[async_trait]
impl ModelProvider for ToolCallStallProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("tool-stall")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let head = stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "partial".into(),
            },
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "call-0".into(),
                name: "echo".into(),
            },
        ]);
        Ok(head.chain(stream::pending()).boxed())
    }
}

/// A cancel that lands after `ToolCallStarted` was already journaled must
/// mark the call discarded, or replay and live clients hold a call that
/// never resolves. The marker is conditional — a cancel with only partial
/// prose must not send it, because replay clears visible assistant text on
/// the marker and cancellation deliberately retains that prose.
#[tokio::test]
async fn cancel_after_a_tool_call_starts_does_not_leave_it_dangling() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(ToolCallStallProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "tool-stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    // Cancel the instant the started call is visible; the stream then
    // stalls, so only the cancel can end the turn.
    let mut events = Vec::new();
    while let Some(event) = rx.next().await {
        if matches!(event, AgentEvent::ToolCallStarted { .. }) {
            cancel.cancel();
        }
        events.push(event);
    }
    handle.await.unwrap();

    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStarted { .. }));
    let interrupted = events
        .iter()
        .position(|e| matches!(e, AgentEvent::StreamInterrupted));
    let cancelled = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnCancelled { .. }));
    assert!(
        matches!((started, interrupted, cancelled), (Some(a), Some(b), Some(c)) if a < b && b < c),
        "the started call is marked discarded before the turn terminalizes: {events:?}"
    );

    // The other half of the contract: with no started tool call the marker
    // stays unsent, so the partial prose the client already showed survives.
    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    let mut events = Vec::new();
    while let Some(event) = rx.next().await {
        if matches!(event, AgentEvent::TextDelta { .. }) {
            cancel.cancel();
        }
        events.push(event);
    }
    handle.await.unwrap();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnCancelled { .. })),
        "a prose-only cancel still ends the turn as cancelled"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)),
        "a cancel with no started tool call keeps the partial prose"
    );
}

#[tokio::test]
async fn cancel_drops_an_in_flight_server_tool_future() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let agent = Agent::new(
        Arc::new(BlockingToolProvider),
        Arc::new(ToolRegistry::new().with(Box::new(BlockingTool {
            entered: entered.clone(),
            dropped: dropped.clone(),
        }))),
        store,
        AgentConfig {
            model: "blocking-tool".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, rx) = unbounded();
    let handle = tokio::spawn(async move {
        agent.run_turn(&chat, "go", &tx).await.unwrap();
    });

    entered.notified().await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), handle)
        .await
        .expect("cancellation should stop an in-flight tool promptly")
        .unwrap();
    let events = rx.collect::<Vec<_>>().await;

    assert!(
        dropped.load(Ordering::SeqCst),
        "cancellation must drop the tool future so its HTTP request can abort"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.is_error && output.content == "turn cancelled during tool execution"
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
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
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalDecided { approved: true, .. })));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

struct RestartTool {
    ran: Arc<AtomicUsize>,
    class: ApprovalClass,
}

#[async_trait]
impl Tool for RestartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "recover search".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        self.class
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("recovered result"))
    }
}

struct RestartGate(Arc<dyn Store>);

impl ApprovalGate for RestartGate {
    fn register(
        &self,
        request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        let store = self.0.clone();
        Box::pin(async move {
            let approval = store
                .get_tool_call_approval(request.call_id)
                .await
                .unwrap()
                .expect("approval receipt must survive restart");
            let decision = match approval.decision() {
                Some(decision) => decision,
                None => {
                    store
                        .decide_tool_call_approval(
                            request.chat_id,
                            request.call_id,
                            &ApprovalDecision::Approve,
                            Utc::now(),
                        )
                        .await
                        .unwrap();
                    ApprovalDecision::Approve
                }
            };
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async move { decision }),
                publication: crate::approval::ApprovalRequiredPublication::None,
            }
        })
    }
}

struct RestartProvider {
    provider_id: String,
    expect_error: bool,
}

#[async_trait]
impl ModelProvider for RestartProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("restart")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        assert!(request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                ContentBlock::ToolResult { tool_use_id, is_error, .. }
                    if tool_use_id == &self.provider_id && *is_error == self.expect_error
                )
            })
        }));
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

async fn assert_sensitive_restart_recovery(
    preapproved: bool,
    current_class: ApprovalClass,
    tool_present: bool,
) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("restart.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "search")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now().max(accepted.available_at);
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "persisted-search".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "restart"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, lease_token, claimed_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    store
        .request_tool_call_approval(
            &ApprovalRequest {
                auto_judge: false,
                call_id: call.id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::for_tool_name(&call.name),
                preview: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    if preapproved {
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now())
            .await
            .unwrap();
    }
    let ran = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    if tool_present {
        registry.register(Box::new(RestartTool {
            ran: ran.clone(),
            class: current_class,
        }));
    }
    let agent = Agent::new(
        Arc::new(RestartProvider {
            provider_id: call.provider_id.clone(),
            expect_error: true,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_approvals(Arc::new(RestartGate(store.clone())))
    .with_durable_steer(lease_token);
    let (tx, mut rx) = unbounded();
    let events = tokio::spawn(async move {
        let mut collected = Vec::new();
        while let Some(event) = rx.next().await {
            match event {
                ClaimedAgentEvent::Flush(acknowledge) => {
                    let _ = acknowledge.send(());
                }
                ClaimedAgentEvent::Pending { event, .. } => collected.push(event),
                ClaimedAgentEvent::Committed { event, .. }
                | ClaimedAgentEvent::Recovered { event, .. } => {
                    collected.push(event.event);
                }
            }
        }
        collected
    });
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Completed { .. }
    ));
    drop(tx);
    let events = events.await.unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
    let approval_decided = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ApprovalDecided { call_id, .. } if *call_id == call.id
            )
        })
        .expect("recovery must close its durable approval card");
    let tool_completed = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id
            )
        })
        .expect("recovery must publish its failed completion");
    assert!(approval_decided < tool_completed);
}

#[tokio::test]
async fn reclaimed_turn_suppresses_pending_and_preapproved_sensitive_calls() {
    assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, true).await;
    assert_sensitive_restart_recovery(true, ApprovalClass::Sensitive, true).await;
    assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, false).await;
}

async fn pending_workspace_restart(
    name: &str,
    arguments: Value,
) -> (
    tempfile::TempDir,
    Arc<dyn Store>,
    Chat,
    TurnId,
    uuid::Uuid,
    ToolCallRecord,
) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("cancelled-restart.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "recover workspace call")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now().max(accepted.available_at);
    assert!(store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap()
        .turn
        .is_some());
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "persisted-workspace-call".into(),
        name: name.into(),
        arguments,
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, lease_token, claimed_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    (db, store, chat, turn_id, lease_token, call)
}

#[tokio::test]
async fn cancelled_reclaim_resolves_pending_write_without_touching_scratch() {
    let scratch = tempfile::tempdir().unwrap();
    let (_db, store, chat, turn_id, lease_token, call) = pending_workspace_restart(
        "write_file",
        serde_json::json!({"path": "cancelled.txt", "content": "must not exist"}),
    )
    .await;
    store
        .request_tool_call_approval(
            &ApprovalRequest {
                auto_judge: false,
                call_id: call.id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::for_tool_name(&call.name),
                preview: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let provider = Arc::new(BoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(ToolRegistry::new().with(Box::new(WriteFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(scratch.path())),
            ..AgentConfig::default()
        },
    )
    .with_cancel(cancel)
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Cancelled { .. }
    ));
    drop(tx);
    let events = emitted_events(rx.collect().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(!scratch.path().join("cancelled.txt").exists());
    let approval_decided = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ApprovalDecided {
                    call_id,
                    approved: false,
                } if *call_id == call.id
            )
        })
        .expect("cancelled recovery must close its durable approval card");
    let tool_completed = events
            .iter()
            .position(|event| {
                matches!(event, AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id)
            })
            .expect("cancelled recovery must publish failed tool completion");
    assert!(approval_decided < tool_completed);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
}

struct CancelDuringRecoveryTool {
    cancel: CancelToken,
    classifications: AtomicUsize,
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CancelDuringRecoveryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "recovery_write".into(),
            description: "test recovery cancellation fence".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        if self.classifications.fetch_add(1, Ordering::SeqCst) == 1 {
            self.cancel.cancel();
        }
        ApprovalClass::Workspace
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("unexpected execution"))
    }
}

#[tokio::test]
async fn recovery_never_reexecutes_a_pending_workspace_call() {
    let (_db, store, chat, turn_id, lease_token, call) =
        pending_workspace_restart("recovery_write", serde_json::json!({})).await;
    let cancel = CancelToken::new();
    let ran = Arc::new(AtomicUsize::new(0));
    let tool = CancelDuringRecoveryTool {
        cancel: cancel.clone(),
        classifications: AtomicUsize::new(0),
        ran: ran.clone(),
    };
    let agent = Agent::new(
        Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(tool))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_cancel(cancel)
    .with_durable_steer(lease_token);
    let (tx, _rx) = unbounded();
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Completed { .. }
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
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
            AgentEvent::UserSteered { content, .. } => {
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
async fn boundary_steer_persists_distinct_legacy_assistant_candidates() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    struct BoundaryThenFinish {
        calls: AtomicUsize,
        release: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
    }
    #[async_trait]
    impl ModelProvider for BoundaryThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("boundary-then-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let release = self.release.lock().unwrap().take().unwrap();
                return Ok(stream::iter(vec![ProviderEvent::TextDelta {
                    text: "first candidate".into(),
                }])
                .chain(stream::once(async move {
                    let _ = release.await;
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    }
                }))
                .boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "final candidate".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (release_tx, release_rx) = futures::channel::oneshot::channel();
    let steer = SteerInbox::new();
    let agent = Agent::new(
        Arc::new(BoundaryThenFinish {
            calls: AtomicUsize::new(0),
            release: Mutex::new(Some(release_rx)),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_steer(steer.clone());

    let chat_id = chat.id;
    let (tx, mut rx) = unbounded();
    let run = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    while let Some(event) = rx.next().await {
        if matches!(
            event,
            AgentEvent::TextDelta { ref text } if text == "first candidate"
        ) {
            assert!(steer.push("revise that", false));
            let _ = release_tx.send(());
            break;
        }
    }
    run.await.unwrap().unwrap();

    let messages = store.list_messages(chat_id).await.unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "go");
    assert_eq!(messages[1].content, "first candidate");
    assert_eq!(messages[2].content, "revise that");
    assert_eq!(messages[3].content, "final candidate");
    assert_ne!(messages[1].id, messages[3].id);
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

#[test]
fn rebuild_replays_message_images_in_their_recorded_order() {
    use crate::image::ImageMediaType;

    let turn = TurnId::new();
    let chat = ChatId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let with_images = MessageId::new();
    let text_only = MessageId::new();
    let mut messages = vec![
        Message {
            id: with_images,
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "compare these".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: text_only,
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "and this one?".into(),
            llm_content: None,
            created_at: t1,
        },
    ];
    let image = |seed: u128, media_type| ImageRef {
        blob_id: uuid::Uuid::from_u128(seed),
        media_type,
        width: 800,
        height: 600,
        byte_len: 4_096,
    };
    let first = image(1, ImageMediaType::Png);
    let second = image(2, ImageMediaType::Jpeg);
    messages[0].llm_content =
        crate::model::user_message_llm_content("compare these", &[first, second], &[], &[], false);
    // Deliberately out of row order: the ordinal decides, not arrival.
    let attachments = vec![
        MessageAttachment {
            message_id: with_images,
            chat_id: chat,
            ordinal: 1,
            image: second,
            created_at: t0,
        },
        MessageAttachment {
            message_id: with_images,
            chat_id: chat,
            ordinal: 0,
            image: first,
            created_at: t0,
        },
    ];

    let rebuilt = rebuild_transcript(&messages, &[], &attachments, DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].role, Role::User);
    assert_eq!(
            rebuilt[0].content,
            vec![
                ContentBlock::Image { image: first },
                ContentBlock::Image { image: second },
                ContentBlock::Text {
                    text: format!(
                        "# Important context\n\n<attachments>\n\
                         image_1: id={}; media_type=image/png; byte_size=4096; this is image content block 1\n\
                         image_2: id={}; media_type=image/jpeg; byte_size=4096; this is image content block 2\n\
                         </attachments>\n\n# User message\n\ncompare these",
                        first.blob_id, second.blob_id
                    )
                },
            ]
        );
    // A message with no attachments rebuilds exactly as it did before.
    assert_eq!(
        rebuilt[1].content,
        vec![ContentBlock::Text {
            text: "and this one?".into()
        }]
    );
    // Reloading the same rows reproduces the identical block sequence.
    assert_eq!(
        rebuild_transcript(&messages, &[], &attachments, DEFAULT_MAX_TOOL_RESULT_BYTES),
        rebuilt
    );
}

#[test]
fn rebuild_announces_file_routes_and_bounds_attachment_context() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let message_id = MessageId::new();
    let created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut message = Message {
        id: message_id,
        chat_id: chat,
        turn_id: turn,
        role: Role::User,
        reasoning: Default::default(),
        content: "summarize this file".into(),
        llm_content: None,
        created_at,
    };
    let text_id = crate::id::DocumentId::new();
    let text_blob = crate::model::DocumentSourceBlob::from_bytes(b"decoded notes");
    let text = crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 0,
        document_id: text_id,
        title: Some("notes.txt".into()),
        media_type: "text/plain".into(),
        source_blob: Some(text_blob),
        readable: true,
        created_at,
    };
    let pdf_id = crate::id::DocumentId::new();
    let pdf_blob = crate::model::DocumentSourceBlob::from_bytes(b"%PDF opaque");
    let pdf = crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 1,
        document_id: pdf_id,
        title: Some("brief.pdf".into()),
        media_type: "application/pdf".into(),
        source_blob: Some(pdf_blob),
        readable: false,
        created_at,
    };
    let mut documents = vec![text, pdf];
    let oversized_id = crate::id::DocumentId::new();
    documents.push(crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 2,
        document_id: oversized_id,
        title: Some("large.xlsx".into()),
        media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        source_blob: Some(crate::model::DocumentSourceBlob::from_digest(
            [9; 32],
            crate::model::MAX_EXEC_WORKSPACE_FILE_BYTES as u64 + 1,
        )),
        readable: false,
        created_at,
    });
    for ordinal in 3..=8 {
        documents.push(crate::model::MessageDocumentAttachment {
            message_id,
            chat_id: chat,
            ordinal,
            document_id: crate::id::DocumentId::new(),
            title: Some(format!("extra-{ordinal}.bin")),
            media_type: "application/octet-stream".into(),
            source_blob: Some(crate::model::DocumentSourceBlob::from_bytes(
                format!("extra-{ordinal}").as_bytes(),
            )),
            readable: false,
            created_at,
        });
    }

    message.llm_content =
        crate::model::user_message_llm_content(&message.content, &[], &documents, &[], false);
    let rebuilt = rebuild_transcript_with_boundary(
        &[message],
        &[],
        &[],
        DEFAULT_MAX_TOOL_RESULT_BYTES,
        false,
        None,
    )
    .0;
    let ContentBlock::Text { text } = &rebuilt[0].content[0] else {
        panic!("file attachment should annotate the user text");
    };
    assert!(text.starts_with("# Important context\n\n<attachments>"));
    assert!(text.contains(&text_id.to_string()));
    assert!(text.contains("\"title\":\"notes.txt\""));
    assert!(text.contains(&format!(
        "route: readable via read_source(document_id=\"{text_id}\")"
    )));
    let pdf_path = format!(
        "documents/{}",
        crate::model::exec_attachment_file_name(Some("brief.pdf"), pdf_id)
    );
    assert!(text.contains(&pdf_id.to_string()));
    assert!(text.contains("\"title\":\"brief.pdf\""));
    assert!(text.contains("\"media_type\":\"application/pdf\""));
    assert!(text.contains(&format!(
        "route: raw bytes at {pdf_path} in the exec workspace; helper: python3 \
             .openwave/exec-scripts/render_pdf.py {pdf_path}"
    )));
    assert!(text.contains(&oversized_id.to_string()));
    assert!(text.contains(&format!(
        "route: raw bytes not materialized because the file exceeds the \
             {}-byte exec workspace limit",
        crate::model::MAX_EXEC_WORKSPACE_FILE_BYTES
    )));
    assert!(text.contains("1 more attachment(s) omitted."));
    assert!(text.ends_with("</attachments>\n\n# User message\n\nsummarize this file"));
}

#[test]
fn rebuild_attaches_tools_to_assistant_text() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "read it".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "looking…".into(),
            llm_content: None,
            created_at: t1,
        },
    ];
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "a"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some("ok".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: t2,
        resolved_at: Some(DateTime::<Utc>::from_timestamp(1_003, 0).unwrap()),
    }];
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 3);
    assert_eq!(rebuilt[0].role, Role::User);
    assert!(matches!(
        &rebuilt[1].content[..],
        [
            ContentBlock::Text { text },
            ContentBlock::ToolUse { id, name, .. }
        ] if text == "looking…" && id == "tu_1" && name == "read_file"
    ));
    assert!(matches!(
        &rebuilt[2].content[..],
        [ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error: false
        }] if tool_use_id == "tu_1" && content == "ok"
    ));
}

#[test]
fn orchestration_forces_a_model_step_boundary_despite_overlapping_timestamps() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let t3 = DateTime::<Utc>::from_timestamp(1_003, 0).unwrap();
    let call = |provider_id: &str,
                execution: ToolCallExecution,
                created_at: DateTime<Utc>,
                resolved_at: DateTime<Utc>| ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: provider_id.into(),
        name: if execution == ToolCallExecution::Orchestration {
            crate::SPAWN_SANDBOX_AGENT_TOOL.into()
        } else {
            "read_file".into()
        },
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution,
        status: ToolCallStatus::Completed,
        result: Some("ok".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: Some(resolved_at),
    };
    let calls = vec![
        call("ordinary-before", ToolCallExecution::Server, t1, t3),
        call("spawn", ToolCallExecution::Orchestration, t2, t2),
        call("ordinary-after", ToolCallExecution::Server, t2, t3),
    ];
    let batches = batch_tool_calls(&calls);
    assert_eq!(batches.len(), 3);
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch
                .iter()
                .map(|call| call.provider_id.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![
            vec!["ordinary-before"],
            vec!["spawn"],
            vec!["ordinary-after"],
        ]
    );
}

#[test]
fn answered_user_questions_rebuild_as_a_model_facing_tool_result() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let created_at = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let answer = crate::AnswerUserQuestions {
        answers: vec![crate::UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into()],
            custom_answer: None,
        }],
        additional_user_context: None,
    };
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "question_1".into(),
        name: crate::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [{
                    "id": "staging",
                    "label": "Staging",
                    "description": "Deploy for verification."
                }]
            }]
        }),
        raw_arguments: None,
        execution: ToolCallExecution::Orchestration,
        status: ToolCallStatus::Completed,
        result: Some(serde_json::to_string(&answer).unwrap()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: Some(created_at),
    }];

    let rebuilt = rebuild_transcript(&[], &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert!(matches!(
        &rebuilt[0],
        ChatMessage {
            role: Role::Assistant,
            content: assistant,
            ..
        } if matches!(
            &assistant[..],
            [ContentBlock::ToolUse { id, name, .. }]
                if id == "question_1" && name == crate::ASK_USER_QUESTIONS_TOOL
        )
    ));
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = &rebuilt[1].content[0]
    else {
        panic!("answer must rebuild as a tool result");
    };
    assert_eq!(rebuilt[1].role, Role::User);
    assert_eq!(tool_use_id, "question_1");
    assert!(!is_error);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(content).unwrap(),
        answer
    );
}

#[test]
fn rebuild_emits_tool_only_step_before_final_text() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "go".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "done".into(),
            llm_content: None,
            created_at: t2,
        },
    ];
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some("data".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: t1,
        resolved_at: Some(t1),
    }];
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(rebuilt[0].role, Role::User);
    assert!(matches!(
        &rebuilt[1].content[..],
        [ContentBlock::ToolUse { .. }]
    ));
    assert!(matches!(
        &rebuilt[2].content[..],
        [ContentBlock::ToolResult { .. }]
    ));
    assert_eq!(rebuilt[3].role, Role::Assistant);
}

#[test]
fn rebuild_skips_legacy_tool_role_rows() {
    let turn = TurnId::new();
    let chat = ChatId::new();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "hi".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Tool,
            reasoning: Default::default(),
            content: "legacy".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "bye".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
        },
    ];
    let rebuilt = rebuild_transcript(&messages, &[], &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].role, Role::User);
    assert_eq!(rebuilt[1].role, Role::Assistant);
}

#[tokio::test]
async fn second_turn_rebuilds_prior_tool_calls_into_transcript() {
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
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    // Turn 1: tool call then finish (FakeProvider).
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;

    // Turn 2: provider that records the request so we can assert ToolUse/Result
    // blocks were rebuilt from the store.
    let seen: Arc<Mutex<Vec<ChatMessage>>> = Arc::new(Mutex::new(Vec::new()));
    struct CaptureProvider {
        seen: Arc<Mutex<Vec<ChatMessage>>>,
    }
    #[async_trait]
    impl ModelProvider for CaptureProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("capture")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            *self.seen.lock().unwrap() = req.messages;
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }
    let agent = Agent::new(
        Arc::new(CaptureProvider { seen: seen.clone() }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "what did you find?", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;

    let messages = seen.lock().unwrap().clone();
    assert!(
        messages.iter().any(|m| {
            m.role == Role::Assistant
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name == "read_file"))
        }),
        "expected rebuilt ToolUse in cross-turn transcript: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| {
            m.role == Role::User
                && m.content.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::ToolResult { content, .. } if content == "hello from disk"
                    )
                })
        }),
        "expected rebuilt ToolResult in cross-turn transcript: {messages:?}"
    );
}
