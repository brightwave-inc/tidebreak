#![allow(dead_code)]

use super::*;

use std::net::Ipv4Addr;
use std::ops::Range;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::web_search::{
    WebSearchError, WebSearchProvider, WebSearchProviderKind, WebSearchRequest, WebSearchResolver,
    WebSearchResolverError, WebSearchResponse, WebSearchResult, WebSearchTool,
};
use async_trait::async_trait;
use axum::body::{to_bytes, Body, Bytes};
use axum::http::{header, Request, StatusCode};
use futures::stream::{self, BoxStream, StreamExt};
use openwave_core::{
    AgentConfig, AgentErrorInfo, AgentEvent, AgentRunInboxStatus, AgentRunStatus, ApprovalClass,
    BeginRootAttachmentChange, BlobMetadata, BlobStore, BlobStream, CallId, Chat, ChatId,
    ChatRequest, ChatRootAttachment, ClientToolCallRequest, ContentBlock, DeleteProjectOutcome,
    HostRootId, Message, MessageId, ModelProvider, ParkSandboxToolCallOutcome,
    ParkTurnForClientCallOutcome, Project, ProjectId, ProviderEvent, ProviderId, Role,
    RootAttachmentChangeAction, RootAttachmentChangeId, RootAttachmentOrigin,
    SandboxToolCallRequest, SecretProvider, SequencedEvent, StopReason, Tool, ToolCallExecution,
    ToolCallRecord, ToolCallResolution, ToolCallStatus, ToolCtx, ToolOutput, ToolRegistry,
    ToolSpec, TurnCheckpointProgress, TurnId, TurnRunStatus, TurnSteerId, Usage,
};
use resolver::ProviderResolver;
use sea_orm::ConnectionTrait;
use serde::de::DeserializeOwned;
use tokio::sync::Notify;
use tower::ServiceExt;

mod app_grant;
mod app_invoke;
mod app_library;
mod chat_titling;
mod configuration;
mod conformance;
mod connected_apps;
mod conversations;
mod documents;
mod image_attachment;
mod lifecycle;
mod root_attachment;
mod sandbox;
mod websocket;
mod workers;

use conversations::patch_chat;
use lifecycle::{post_json, post_native_json, steer_turn, steer_turn_with_id};

/// Self-host fixture credentials, named rather than inlined: the token floor
/// is 32 characters, and inline high-entropy literals trip the secret scan.
/// Alice administers the fixture deployments; Bob is a member.
const ALICE_TOKEN: &str = "alice-token-padded-out-to-thirty-two";
const BOB_TOKEN: &str = "bob-token-padded-out-to-thirty-two-x";

struct MigratedSqliteTemplate {
    _directory: tempfile::TempDir,
    database: std::path::PathBuf,
}

static MIGRATED_SQLITE_TEMPLATE: tokio::sync::OnceCell<MigratedSqliteTemplate> =
    tokio::sync::OnceCell::const_new();

/// Build the current empty schema once, then copy it into isolated server tests.
///
/// Tests that exercise restart, locking, or unusual database setup keep their
/// explicit connection path rather than using this helper.
async fn migrated_sqlite_template() -> &'static MigratedSqliteTemplate {
    MIGRATED_SQLITE_TEMPLATE
        .get_or_init(|| async {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("template.db");
            let url = format!("sqlite://{}?mode=rwc", database.display());
            let store = DbStore::connect(&url).await.unwrap();
            drop(store);

            let checkpoint = sea_orm::Database::connect(&url).await.unwrap();
            checkpoint
                .execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE);")
                .await
                .unwrap();
            checkpoint.close().await.unwrap();

            MigratedSqliteTemplate {
                _directory: directory,
                database,
            }
        })
        .await
}

async fn temp_db_store(database_name: &str) -> (tempfile::TempDir, DbStore) {
    let template = migrated_sqlite_template().await;
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(database_name);
    std::fs::copy(&template.database, &database).unwrap();
    let store = DbStore::connect(&format!("sqlite://{}?mode=rw", database.display()))
        .await
        .unwrap();
    (directory, store)
}

#[test]
fn transcript_citation_json_is_closed_and_renderer_bounded() {
    let message_id = MessageId::new();
    let snapshot = crate::routes::ChatMessageSnapshot {
        id: message_id,
        role: crate::routes::TranscriptRole::Assistant,
        content: "answer".into(),
        created_at: chrono::Utc::now(),
        citations: vec![openwave_core::AssistantCitationSnapshot {
            id: openwave_core::AssistantCitationId::derive(message_id, 1),
            ordinal: 1,
            document_id: openwave_core::DocumentId::new(),
            locator: openwave_core::CitationLocator::Pages { start: 2, end: 4 },
        }],
        image_attachments: None,
        file_attachments: None,
        invoked_skills: None,
    };
    let json = serde_json::to_value(snapshot).unwrap();
    let citation = &json["citations"][0];
    assert_eq!(
        citation
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["document_id", "id", "locator", "ordinal"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    // The locator is a tagged union that the renderer can navigate directly.
    assert_eq!(
        citation["locator"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["end", "kind", "start"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert_eq!(citation["locator"]["kind"], "pages");
}

/// A provider that answers with a one-line completion and no tool calls.
struct FakeProvider;

#[async_trait]
impl ModelProvider for FakeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta { text: "hi".into() },
            ProviderEvent::Usage(Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Default::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// A provider that emits visible text before the provider's terminal refusal.
struct MidStreamRefusalProvider;

#[async_trait]
impl ModelProvider for MidStreamRefusalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("mid-stream-refusal")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "Visible partial answer".into(),
            },
            ProviderEvent::Refusal {
                details: openwave_core::RefusalDetails::from_category(Some("general_harms")),
            },
        ])
        .boxed())
    }
}

/// Drives non-blocking spawn, premature completion correction, ordered wait,
/// and final foreground completion. The sandbox surface remains independent.
#[derive(Default)]
struct SandboxRoundTripProvider {
    foreground_calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<ChatRequest>>,
    second_child_started: tokio::sync::Notify,
}

#[async_trait]
impl ModelProvider for SandboxRoundTripProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-round-trip")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let sandbox = !request.tools.iter().any(|tool| {
            matches!(
                tool.name.as_str(),
                openwave_core::SPAWN_SANDBOX_AGENT_TOOL | openwave_core::WAIT_FOR_AGENTS_TOOL
            )
        });
        let delegated_task = sandbox.then(|| {
            request
                .messages
                .first()
                .and_then(|message| message.content.first())
                .and_then(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        });
        self.requests.lock().unwrap().push(request);
        let events = if sandbox {
            let first = delegated_task
                .as_deref()
                .is_some_and(|task| task.contains("first child"));
            if first {
                self.second_child_started.notified().await;
                tokio::time::sleep(Duration::from_millis(25)).await;
            } else {
                self.second_child_started.notify_one();
            }
            vec![
                ProviderEvent::TextDelta {
                    text: if first {
                        "first child result".into()
                    } else {
                        "second child result".into()
                    },
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        } else {
            let foreground_call = self.foreground_calls.fetch_add(1, Ordering::SeqCst);
            match foreground_call {
                0 => vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "delegate-1".into(),
                        name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"task":"Return the first child result."}"#.into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ],
                1 => vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "delegate-2".into(),
                        name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"task":"Return the second child result."}"#.into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ],
                // The worker must reject this terminal answer while children
                // remains unsettled, then inject a fixed wait correction.
                2 => vec![
                    ProviderEvent::TextDelta {
                        text: "premature parent answer".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
                3 => {
                    let request = self.requests.lock().unwrap().last().unwrap().clone();
                    let agent_ids = request
                        .messages
                        .iter()
                        .flat_map(|message| &message.content)
                        .filter_map(|block| match block {
                            ContentBlock::ToolResult { content, .. } => {
                                serde_json::from_str::<serde_json::Value>(content)
                                    .ok()
                                    .and_then(|value| value["agent_id"].as_str().map(str::to_owned))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(agent_ids.len(), 2);
                    vec![
                        ProviderEvent::ToolCallStarted {
                            index: 0,
                            id: "wait-1".into(),
                            name: openwave_core::WAIT_FOR_AGENTS_TOOL.into(),
                        },
                        ProviderEvent::ToolCallArgsDelta {
                            index: 0,
                            fragment: serde_json::json!({"agent_ids":agent_ids}).to_string(),
                        },
                        ProviderEvent::Stop {
                            reason: StopReason::ToolUse,
                        },
                    ]
                }
                _ => vec![
                    ProviderEvent::TextDelta {
                        text: "parent completed after ordered wait".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            }
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Names two delegations in one model step, so the batch has to survive the
/// approval park between them. Counts foreground invocations: a resumed claim
/// that re-invokes the model to recover the tail is the bug this exists to
/// catch.
#[derive(Default)]
struct GatedSpawnBatchProvider {
    foreground_calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for GatedSpawnBatchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("gated-spawn-batch")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let foreground = request
            .tools
            .iter()
            .any(|tool| tool.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL);
        if !foreground {
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "child result".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }
        let events = if self.foreground_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "delegate-1".into(),
                    name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"task":"Research the first question."}"#.into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "delegate-2".into(),
                    name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: r#"{"task":"Research the second question."}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "both delegations are running".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A provider that records the model each request asked for, then answers
/// like `FakeProvider`. Lets a test assert which model a turn ran against.
#[derive(Clone, Default)]
struct RecordingProvider {
    models: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("recording")
    }
    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.models.lock().unwrap().push(req.model);
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "recorded".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// A provider whose completion blocks on `gate` until the test releases it —
/// so a turn stays active while the test checks concurrency behavior.
struct GatedProvider {
    gate: Arc<Notify>,
}

#[async_trait]
impl ModelProvider for GatedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("gated")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let gate = self.gate.clone();
        Ok(stream::once(async move {
            gate.notified().await;
            stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "gated answer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
        })
        .flatten()
        .boxed())
    }
}

/// Completes one turn, then leaves the next turn live after one text delta.
/// This models reopening a conversation while a response is still streaming.
struct ReplayBoundaryProvider {
    calls: AtomicUsize,
    active_delta_entered: Arc<Notify>,
}

#[async_trait]
impl ModelProvider for ReplayBoundaryProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("replay-boundary")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "durable answer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }

        let entered = self.active_delta_entered.clone();
        Ok(stream::once(async move {
            entered.notify_one();
            ProviderEvent::TextDelta {
                text: "still streaming".into(),
            }
        })
        .chain(stream::pending())
        .boxed())
    }
}

/// A blob store that pauses its first publication so a second same-document
/// request can prove the route holds its document write guard across blob I/O.
struct FirstPutGatedBlobStore {
    inner: openwave_core::FsBlobStore,
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
}

/// An instrumented store used to prove a response range never falls back to
/// [`BlobStore::get`], which would materialize the complete source.
struct RangeOnlyBlobStore {
    bytes: Arc<Vec<u8>>,
    full_reads: AtomicUsize,
    requested_ranges: Mutex<Vec<Range<u64>>>,
}

#[async_trait]
impl BlobStore for RangeOnlyBlobStore {
    async fn put(&self, _id: uuid::Uuid, _bytes: Vec<u8>) -> Result<()> {
        Err(AgentError::Store(
            "range-only test store cannot publish".into(),
        ))
    }

    async fn get(&self, _id: uuid::Uuid) -> Result<Option<Vec<u8>>> {
        self.full_reads.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Store(
            "document route must not materialize the blob".into(),
        ))
    }

    async fn metadata(&self, _id: uuid::Uuid) -> Result<Option<BlobMetadata>> {
        Ok(Some(BlobMetadata {
            byte_len: u64::try_from(self.bytes.len()).expect("usize always fits in u64"),
        }))
    }

    async fn read_range(&self, _id: uuid::Uuid, range: Range<u64>) -> Result<Option<BlobStream>> {
        self.requested_ranges.lock().unwrap().push(range.clone());
        let start = usize::try_from(range.start)
            .map_err(|_| AgentError::Store("test range start exceeds usize".into()))?;
        let end = usize::try_from(range.end)
            .map_err(|_| AgentError::Store("test range end exceeds usize".into()))?;
        let bytes = self
            .bytes
            .get(start..end)
            .ok_or_else(|| AgentError::Store("test range is outside source bytes".into()))?
            .to_vec();
        Ok(Some(stream::once(async move { Ok(bytes) }).boxed()))
    }

    fn delete(&self, _id: uuid::Uuid) -> Result<()> {
        Ok(())
    }
}

struct PendingBlobStream {
    dropped: Arc<AtomicBool>,
}

impl futures::Stream for PendingBlobStream {
    type Item = Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for PendingBlobStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// A stream that stays pending until the HTTP body is cancelled.
struct CancellationAwareBlobStore {
    byte_len: u64,
    stream_dropped: Arc<AtomicBool>,
}

#[async_trait]
impl BlobStore for CancellationAwareBlobStore {
    async fn put(&self, _id: uuid::Uuid, _bytes: Vec<u8>) -> Result<()> {
        Err(AgentError::Store(
            "cancellation test store cannot publish".into(),
        ))
    }

    async fn get(&self, _id: uuid::Uuid) -> Result<Option<Vec<u8>>> {
        Err(AgentError::Store(
            "document route must not materialize the blob".into(),
        ))
    }

    async fn metadata(&self, _id: uuid::Uuid) -> Result<Option<BlobMetadata>> {
        Ok(Some(BlobMetadata {
            byte_len: self.byte_len,
        }))
    }

    async fn read_range(&self, _id: uuid::Uuid, _range: Range<u64>) -> Result<Option<BlobStream>> {
        Ok(Some(Box::pin(PendingBlobStream {
            dropped: Arc::clone(&self.stream_dropped),
        })))
    }

    fn delete(&self, _id: uuid::Uuid) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl BlobStore for FirstPutGatedBlobStore {
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.put(id, bytes).await
    }

    async fn get(&self, id: uuid::Uuid) -> Result<Option<Vec<u8>>> {
        self.inner.get(id).await
    }

    fn delete(&self, id: uuid::Uuid) -> Result<()> {
        self.inner.delete(id)
    }
}

/// A resolver that always hands back a fixed provider — lets a test inject a
/// fake in place of the real credential-driven resolution.
struct FixedResolver(Arc<dyn ModelProvider>);

#[async_trait]
impl ProviderResolver for FixedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

/// An in-memory `SecretProvider` for tests (no OS keychain).
#[derive(Default)]
struct MemSecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

#[async_trait]
impl SecretProvider for MemSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

/// Store wrapper that pauses the first terminal event append. This exposes
/// races between `run_turn` returning and the journal finishing.
struct PauseTerminalStore {
    inner: Arc<dyn Store>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    blocked: std::sync::atomic::AtomicBool,
    pause_terminal: std::sync::atomic::AtomicBool,
    fail_after_claim_commit: std::sync::atomic::AtomicBool,
    pause_after_claim_commit: std::sync::atomic::AtomicBool,
    fail_after_heartbeat_commit: std::sync::atomic::AtomicBool,
    fail_after_completion_commit: std::sync::atomic::AtomicBool,
    fail_after_assistant_commit: std::sync::atomic::AtomicBool,
    assistant_append_calls: AtomicUsize,
    fail_after_park_commit: std::sync::atomic::AtomicBool,
    pause_before_spawn_checkpoint: std::sync::atomic::AtomicBool,
    fail_after_apply_steer_commit: std::sync::atomic::AtomicBool,
    cancel_after_apply_steer_commit: std::sync::atomic::AtomicBool,
    apply_steer_cancellation_committed: Arc<Notify>,
    pause_before_steer_read: std::sync::atomic::AtomicBool,
    advance_before_steer_read: std::sync::atomic::AtomicBool,
    fail_terminal_recovery: std::sync::atomic::AtomicBool,
    terminal_recovery_calls: AtomicUsize,
    scan_before_failure_resolution: std::sync::atomic::AtomicBool,
    scan_before_cancellation_ack: std::sync::atomic::AtomicBool,
    pause_nonterminal_event: std::sync::atomic::AtomicBool,
    pause_accept: std::sync::atomic::AtomicBool,
    fail_document_delete: std::sync::atomic::AtomicBool,
    delete_project_after_get: std::sync::atomic::AtomicBool,
}

impl PauseTerminalStore {
    fn new(inner: Arc<dyn Store>, entered: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self {
            inner,
            entered,
            release,
            blocked: std::sync::atomic::AtomicBool::new(false),
            pause_terminal: std::sync::atomic::AtomicBool::new(true),
            fail_after_claim_commit: std::sync::atomic::AtomicBool::new(false),
            pause_after_claim_commit: std::sync::atomic::AtomicBool::new(false),
            fail_after_heartbeat_commit: std::sync::atomic::AtomicBool::new(false),
            fail_after_completion_commit: std::sync::atomic::AtomicBool::new(false),
            fail_after_assistant_commit: std::sync::atomic::AtomicBool::new(false),
            assistant_append_calls: AtomicUsize::new(0),
            fail_after_park_commit: std::sync::atomic::AtomicBool::new(false),
            pause_before_spawn_checkpoint: std::sync::atomic::AtomicBool::new(false),
            fail_after_apply_steer_commit: std::sync::atomic::AtomicBool::new(false),
            cancel_after_apply_steer_commit: std::sync::atomic::AtomicBool::new(false),
            apply_steer_cancellation_committed: Arc::new(Notify::new()),
            pause_before_steer_read: std::sync::atomic::AtomicBool::new(false),
            advance_before_steer_read: std::sync::atomic::AtomicBool::new(false),
            fail_terminal_recovery: std::sync::atomic::AtomicBool::new(false),
            terminal_recovery_calls: AtomicUsize::new(0),
            scan_before_failure_resolution: std::sync::atomic::AtomicBool::new(false),
            scan_before_cancellation_ack: std::sync::atomic::AtomicBool::new(false),
            pause_nonterminal_event: std::sync::atomic::AtomicBool::new(false),
            pause_accept: std::sync::atomic::AtomicBool::new(false),
            fail_document_delete: std::sync::atomic::AtomicBool::new(false),
            delete_project_after_get: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn do_not_pause_terminal(&self) {
        self.pause_terminal.store(false, Ordering::SeqCst);
    }

    fn fail_after_next_claim_commit(&self) {
        self.fail_after_claim_commit.store(true, Ordering::SeqCst);
    }

    fn pause_after_next_claim_commit(&self) {
        self.pause_after_claim_commit.store(true, Ordering::SeqCst);
    }

    fn fail_after_next_heartbeat_commit(&self) {
        self.fail_after_heartbeat_commit
            .store(true, Ordering::SeqCst);
    }

    fn fail_after_next_completion_commit(&self) {
        self.fail_after_completion_commit
            .store(true, Ordering::SeqCst);
    }

    fn fail_after_next_assistant_commit(&self) {
        self.fail_after_assistant_commit
            .store(true, Ordering::SeqCst);
    }

    fn assistant_append_calls(&self) -> usize {
        self.assistant_append_calls.load(Ordering::SeqCst)
    }

    fn fail_after_next_park_commit(&self) {
        self.fail_after_park_commit.store(true, Ordering::SeqCst);
    }

    fn pause_before_next_spawn_checkpoint(&self) {
        self.pause_before_spawn_checkpoint
            .store(true, Ordering::SeqCst);
    }

    fn fail_after_next_apply_steer_commit(&self) {
        self.fail_after_apply_steer_commit
            .store(true, Ordering::SeqCst);
    }

    fn cancel_after_next_apply_steer_commit(&self) -> Arc<Notify> {
        self.cancel_after_apply_steer_commit
            .store(true, Ordering::SeqCst);
        self.apply_steer_cancellation_committed.clone()
    }

    fn pause_before_next_steer_read(&self) {
        self.pause_before_steer_read.store(true, Ordering::SeqCst);
    }

    fn advance_before_next_steer_read(&self) {
        self.advance_before_steer_read.store(true, Ordering::SeqCst);
    }

    fn fail_next_terminal_recovery(&self) {
        self.fail_terminal_recovery.store(true, Ordering::SeqCst);
    }

    fn terminal_recovery_calls(&self) -> usize {
        self.terminal_recovery_calls.load(Ordering::SeqCst)
    }

    fn let_scan_win_next_failure_resolution(&self) {
        self.scan_before_failure_resolution
            .store(true, Ordering::SeqCst);
    }

    fn let_scan_win_next_cancellation_ack(&self) {
        self.scan_before_cancellation_ack
            .store(true, Ordering::SeqCst);
    }

    fn pause_next_nonterminal_event(&self) {
        self.pause_nonterminal_event.store(true, Ordering::SeqCst);
    }

    fn pause_next_acceptance(&self) {
        self.pause_accept.store(true, Ordering::SeqCst);
    }

    fn fail_next_document_delete(&self) {
        self.fail_document_delete.store(true, Ordering::SeqCst);
    }

    fn delete_project_after_next_get(&self) {
        self.delete_project_after_get.store(true, Ordering::SeqCst);
    }

    async fn terminalize_expired_turn(&self, id: TurnId) -> Result<()> {
        let turn = self
            .inner
            .get_turn_run(id)
            .await?
            .ok_or_else(|| AgentError::Store("injected scan could not find turn".into()))?;
        let now = turn
            .lease_expires_at
            .ok_or_else(|| AgentError::Store("injected scan found no lease".into()))?
            + chrono::Duration::microseconds(1);
        let mut scan_at = now;
        loop {
            let outcome = self
                .inner
                .claim_turn_run(
                    uuid::Uuid::new_v4(),
                    scan_at,
                    scan_at + chrono::Duration::seconds(1),
                )
                .await?;
            if outcome.terminal_event.is_some() {
                return Ok(());
            }
            let Some(retried) = outcome.turn else {
                return Err(AgentError::Store(
                    "injected scan neither retried nor terminalized the turn".into(),
                ));
            };
            scan_at = retried
                .lease_expires_at
                .ok_or_else(|| AgentError::Store("injected retry has no lease".into()))?
                + chrono::Duration::microseconds(1);
        }
    }
}

#[async_trait]
impl Store for PauseTerminalStore {
    async fn create_project(&self, project: &Project) -> Result<()> {
        self.inner.create_project(project).await
    }
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        let project = self.inner.get_project(id).await?;
        if project.is_some() && self.delete_project_after_get.swap(false, Ordering::SeqCst) {
            assert_eq!(
                self.inner.delete_project(id).await?,
                DeleteProjectOutcome::Deleted
            );
        }
        Ok(project)
    }
    async fn list_projects(&self) -> Result<Vec<Project>> {
        self.inner.list_projects().await
    }
    async fn create_document(&self, document: &openwave_core::DocumentRecord) -> Result<()> {
        self.inner.create_document(document).await
    }
    async fn get_document(
        &self,
        id: openwave_core::DocumentId,
    ) -> Result<Option<openwave_core::DocumentRecord>> {
        self.inner.get_document(id).await
    }
    async fn list_documents(
        &self,
        scope: openwave_core::DocumentScope,
    ) -> Result<Vec<openwave_core::DocumentRecord>> {
        self.inner.list_documents(scope).await
    }
    async fn list_document_summaries(
        &self,
        scope: openwave_core::DocumentScope,
        after: Option<openwave_core::DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<openwave_core::DocumentSummaryRecord>> {
        self.inner
            .list_document_summaries(scope, after, limit)
            .await
    }
    async fn delete_document(&self, id: openwave_core::DocumentId) -> Result<()> {
        if self.fail_document_delete.swap(false, Ordering::SeqCst) {
            return Err(AgentError::Store(
                "injected document catalog delete failure".into(),
            ));
        }
        self.inner.delete_document(id).await
    }
    async fn upsert_document(
        &self,
        document: &openwave_core::DocumentUpsert,
    ) -> Result<openwave_core::DocumentRecord> {
        self.inner.upsert_document(document).await
    }
    async fn accept_document_source(
        &self,
        source: &openwave_core::DocumentSourceUpsert,
    ) -> Result<openwave_core::DocumentRecord> {
        self.inner.accept_document_source(source).await
    }
    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        self.inner.create_chat(chat).await
    }
    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat> {
        self.inner.create_chat_with_project_defaults(chat).await
    }
    async fn create_chat_with_project_defaults_and_settings_scoped(
        &self,
        owner: &openwave_core::OwnerId,
        chat: &Chat,
        settings: &[(String, serde_json::Value)],
    ) -> Result<Chat> {
        self.inner
            .create_chat_with_project_defaults_and_settings_scoped(owner, chat, settings)
            .await
    }
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        self.inner.get_chat(id).await
    }
    async fn list_chats(&self) -> Result<Vec<Chat>> {
        self.inner.list_chats().await
    }
    async fn get_chat_transcript(
        &self,
        id: ChatId,
    ) -> Result<Option<openwave_core::ChatTranscriptSnapshot>> {
        self.inner.get_chat_transcript(id).await
    }
    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        self.inner.set_chat_model(id, model).await
    }
    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()> {
        self.inner.set_chat_title(id, title).await
    }
    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool> {
        self.inner.set_chat_title_if_unset(id, title).await
    }
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<openwave_core::ReasoningEffort>>,
        permission_mode: Option<Option<openwave_core::PermissionMode>>,
        network_policy: Option<openwave_core::NetworkPolicy>,
    ) -> Result<bool> {
        self.inner
            .update_chat_metadata(
                id,
                title,
                model,
                reasoning_effort,
                permission_mode,
                network_policy,
            )
            .await
    }
    async fn get_turn_run(&self, id: TurnId) -> Result<Option<openwave_core::TurnRun>> {
        self.inner.get_turn_run(id).await
    }
    async fn list_turn_runs(&self, chat_id: ChatId) -> Result<Vec<openwave_core::TurnRun>> {
        self.inner.list_turn_runs(chat_id).await
    }
    // Hooked here rather than on `accept_turn`, because the trait's plain
    // `accept_turn` delegates to this one — overriding only the former would
    // leave a turn carrying attachments bypassing the pause entirely.
    async fn accept_turn_with_attachments(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
        images: &[openwave_core::ImageRef],
        documents: &[openwave_core::DocumentId],
        invoked_skills: &[String],
    ) -> Result<openwave_core::AcceptTurnOutcome> {
        if self.pause_accept.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner
            .accept_turn_with_attachments(
                id,
                chat_id,
                model,
                content,
                images,
                documents,
                invoked_skills,
            )
            .await
    }
    async fn accept_turn_steer(
        &self,
        id: TurnSteerId,
        turn_id: TurnId,
        chat_id: ChatId,
        content: &str,
        interrupt: bool,
    ) -> Result<openwave_core::AcceptTurnSteerOutcome> {
        self.inner
            .accept_turn_steer(id, turn_id, chat_id, content, interrupt)
            .await
    }
    async fn list_pending_turn_steers(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Vec<openwave_core::TurnSteer>>> {
        if self.pause_before_steer_read.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        if self.advance_before_steer_read.swap(false, Ordering::SeqCst) {
            let turn = self
                .inner
                .get_turn_run(turn_id)
                .await?
                .ok_or_else(|| AgentError::Store("injected steer read lost turn".into()))?;
            let lease_expires_at = turn
                .lease_expires_at
                .ok_or_else(|| AgentError::Store("injected steer read lost lease".into()))?
                + chrono::Duration::seconds(1);
            let advanced = now + chrono::Duration::milliseconds(1);
            if !self
                .inner
                .heartbeat_turn_run(turn_id, lease_token, advanced, lease_expires_at)
                .await?
            {
                return Err(AgentError::Store(
                    "injected steer read could not advance heartbeat".into(),
                ));
            }
        }
        self.inner
            .list_pending_turn_steers(turn_id, lease_token, now)
            .await
    }
    async fn apply_turn_steer(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        steer_id: TurnSteerId,
        attempt_event_ordinal: i32,
        preceding_assistant: Option<&Message>,
        preceding_citations: &[openwave_core::AssistantCitationInput],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<openwave_core::JournaledTurnSteerOutcome>> {
        let applied = self
            .inner
            .apply_turn_steer(
                turn_id,
                lease_token,
                steer_id,
                attempt_event_ordinal,
                preceding_assistant,
                preceding_citations,
                now,
            )
            .await?;
        if applied.is_some()
            && self
                .cancel_after_apply_steer_commit
                .swap(false, Ordering::SeqCst)
        {
            // Commit the cancellation that beats the steer's ambiguous response.
            // `request_turn_cancellation` returns `Ok(None)` when it loses the
            // optimistic-concurrency race against a concurrent heartbeat that has
            // just bumped `updated_at`; production callers retry with a fresh
            // timestamp. Retry here too so the modeled cancellation always lands
            // instead of being silently abandoned (which left the turn Running
            // with the steer applied and the notify never fired — the flake).
            loop {
                if self
                    .inner
                    .request_turn_cancellation_and_append_event(turn_id, chrono::Utc::now())
                    .await?
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            self.apply_steer_cancellation_committed.notify_one();
            return Err(AgentError::Store(
                "injected cancelled ambiguous steer application response".into(),
            ));
        }
        if applied.is_some()
            && self
                .fail_after_apply_steer_commit
                .swap(false, Ordering::SeqCst)
        {
            return Err(AgentError::Store(
                "injected ambiguous steer application response".into(),
            ));
        }
        Ok(applied)
    }
    async fn claim_turn_run(
        &self,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::ClaimTurnRunOutcome> {
        let outcome = self
            .inner
            .claim_turn_run(lease_token, now, lease_expires_at)
            .await?;
        if outcome.turn.is_some() && self.pause_after_claim_commit.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
            return Err(AgentError::Store(
                "injected delayed ambiguous claim response".into(),
            ));
        }
        if outcome.turn.is_some() && self.fail_after_claim_commit.swap(false, Ordering::SeqCst) {
            return Err(AgentError::Store(
                "injected ambiguous claim response".into(),
            ));
        }
        Ok(outcome)
    }
    async fn heartbeat_turn_run(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let heartbeat = self
            .inner
            .heartbeat_turn_run(id, lease_token, now, lease_expires_at)
            .await?;
        if heartbeat
            && self
                .fail_after_heartbeat_commit
                .swap(false, Ordering::SeqCst)
        {
            return Err(AgentError::Store(
                "injected ambiguous heartbeat response".into(),
            ));
        }
        Ok(heartbeat)
    }
    async fn fence_turn_lease(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::TurnLeaseFence> {
        self.inner.fence_turn_lease(id, lease_token, now).await
    }
    async fn complete_turn_run_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<chrono::Utc>,
        output: &Message,
        usage: Usage,
        stop_reason: StopReason,
    ) -> Result<Option<openwave_core::JournaledTurnOutcome<openwave_core::CompleteTurnRunOutcome>>>
    {
        if self.pause_terminal.load(Ordering::SeqCst)
            && self
                .blocked
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
        let outcome = self
            .inner
            .complete_turn_run_and_append_event(
                id,
                lease_token,
                expected_steer_revision,
                now,
                output,
                usage,
                stop_reason,
            )
            .await?;
        if outcome.is_some()
            && self
                .fail_after_completion_commit
                .swap(false, Ordering::SeqCst)
        {
            return Err(AgentError::Store(
                "injected ambiguous completion response".into(),
            ));
        }
        Ok(outcome)
    }
    async fn park_turn_for_client_tool_call(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        progress: TurnCheckpointProgress,
        now: chrono::DateTime<chrono::Utc>,
        call: &ClientToolCallRequest,
    ) -> Result<Option<ParkTurnForClientCallOutcome>> {
        let outcome = self
            .inner
            .park_turn_for_client_tool_call(
                turn_id,
                lease_token,
                expected_steer_revision,
                progress,
                now,
                call,
            )
            .await?;
        if outcome.is_some() && self.fail_after_park_commit.swap(false, Ordering::SeqCst) {
            return Err(AgentError::Store(
                "injected ambiguous client checkpoint response".into(),
            ));
        }
        Ok(outcome)
    }
    async fn checkpoint_sandbox_spawn(
        &self,
        request: &openwave_core::SandboxSpawnCheckpointRequest,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<openwave_core::CheckpointSandboxSpawnOutcome>> {
        if self
            .pause_before_spawn_checkpoint
            .swap(false, Ordering::SeqCst)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.checkpoint_sandbox_spawn(request, now).await
    }
    async fn resumed_sandbox_spawn_batch(
        &self,
        turn_id: TurnId,
        attempt_count: i32,
        claim_count: i32,
    ) -> Result<Vec<openwave_core::SandboxAgentSpawnRequest>> {
        self.inner
            .resumed_sandbox_spawn_batch(turn_id, attempt_count, claim_count)
            .await
    }
    async fn record_turn_run_failure_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        retry: openwave_core::TurnFailureRetry,
        model_steps: i32,
        usage: Usage,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<openwave_core::JournaledTurnOutcome<openwave_core::RecordTurnFailureOutcome>>>
    {
        if self
            .scan_before_failure_resolution
            .swap(false, Ordering::SeqCst)
        {
            self.terminalize_expired_turn(id).await?;
        }
        self.inner
            .record_turn_run_failure_and_append_event(
                id,
                lease_token,
                now,
                retry,
                model_steps,
                usage,
                error_code,
                error_detail,
            )
            .await
    }
    async fn request_turn_cancellation_and_append_event(
        &self,
        id: TurnId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Option<openwave_core::JournaledTurnOutcome<openwave_core::RequestTurnCancellationOutcome>>,
    > {
        self.inner
            .request_turn_cancellation_and_append_event(id, now)
            .await
    }
    async fn finish_turn_cancellation_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        usage: Usage,
        output: Option<&openwave_core::Message>,
        citations: &[openwave_core::AssistantCitationInput],
    ) -> Result<
        Option<openwave_core::JournaledTurnOutcome<openwave_core::FinishTurnCancellationOutcome>>,
    > {
        if self
            .scan_before_cancellation_ack
            .swap(false, Ordering::SeqCst)
        {
            self.terminalize_expired_turn(id).await?;
        }
        self.inner
            .finish_turn_cancellation_and_append_event(
                id,
                lease_token,
                now,
                usage,
                output,
                citations,
            )
            .await
    }
    async fn append_turn_event(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        attempt_event_ordinal: i32,
        now: chrono::DateTime<chrono::Utc>,
        event: &AgentEvent,
    ) -> Result<Option<i64>> {
        if attempt_event_ordinal > 1 && self.pause_nonterminal_event.swap(false, Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
        self.inner
            .append_turn_event(
                chat_id,
                turn_id,
                lease_token,
                attempt_event_ordinal,
                now,
                event,
            )
            .await
    }
    async fn recover_exact_turn_terminal_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        self.terminal_recovery_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_terminal_recovery.swap(false, Ordering::SeqCst) {
            return Err(AgentError::Store(
                "injected transient terminal recovery failure".into(),
            ));
        }
        self.inner
            .recover_exact_turn_terminal_event(turn_id, lease_token, event)
            .await
    }
    async fn recover_exact_completed_turn_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        output: &Message,
        citations: &[openwave_core::AssistantCitationInput],
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        self.terminal_recovery_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_terminal_recovery.swap(false, Ordering::SeqCst) {
            return Err(AgentError::Store(
                "injected transient terminal recovery failure".into(),
            ));
        }
        self.inner
            .recover_exact_completed_turn_event(turn_id, lease_token, output, citations, event)
            .await
    }
    async fn append_message(&self, message: &Message) -> Result<()> {
        self.inner.append_message(message).await
    }
    async fn append_assistant_message_with_citations(
        &self,
        message: &Message,
        citations: &[openwave_core::AssistantCitationInput],
    ) -> Result<()> {
        self.assistant_append_calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .append_assistant_message_with_citations(message, citations)
            .await?;
        if self
            .fail_after_assistant_commit
            .swap(false, Ordering::SeqCst)
        {
            return Err(AgentError::Store(
                "injected ambiguous assistant append response".into(),
            ));
        }
        Ok(())
    }
    async fn append_claimed_assistant_message_with_citations(
        &self,
        message: &Message,
        citations: &[openwave_core::AssistantCitationInput],
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::AppendClaimedMessageOutcome> {
        self.assistant_append_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self
            .inner
            .append_claimed_assistant_message_with_citations(message, citations, lease_token, now)
            .await?;
        if self
            .fail_after_assistant_commit
            .swap(false, Ordering::SeqCst)
        {
            return Err(AgentError::Store(
                "injected ambiguous claimed assistant append response".into(),
            ));
        }
        Ok(outcome)
    }
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        self.inner.list_messages(chat_id).await
    }
    async fn accept_tool_call(
        &self,
        call: &openwave_core::ToolCallRecord,
    ) -> Result<openwave_core::AcceptToolCallOutcome> {
        self.inner.accept_tool_call(call).await
    }
    async fn accept_claimed_tool_call(
        &self,
        call: &openwave_core::ToolCallRecord,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::AcceptClaimedToolCallOutcome> {
        self.inner
            .accept_claimed_tool_call(call, lease_token, now)
            .await
    }
    async fn request_tool_call_approval(
        &self,
        request: &openwave_core::ApprovalRequest,
        requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::RequestToolApprovalOutcome> {
        self.inner
            .request_tool_call_approval(request, requested_at)
            .await
    }
    async fn request_tool_call_approval_and_append_event(
        &self,
        request: &openwave_core::ApprovalRequest,
        lease_token: uuid::Uuid,
        event_ordinal: i32,
        requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::JournaledToolApprovalOutcome> {
        self.inner
            .request_tool_call_approval_and_append_event(
                request,
                lease_token,
                event_ordinal,
                requested_at,
            )
            .await
    }
    async fn decide_tool_call_approval(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: &openwave_core::ApprovalDecision,
        decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::DecideToolApprovalOutcome> {
        self.inner
            .decide_tool_call_approval(chat_id, call_id, decision, decided_at)
            .await
    }
    async fn decide_tool_call_approval_with_grant(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: &openwave_core::ApprovalDecision,
        grant: &openwave_core::StandingGrant,
        decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::DecideToolApprovalOutcome> {
        self.inner
            .decide_tool_call_approval_with_grant(chat_id, call_id, decision, grant, decided_at)
            .await
    }
    async fn get_tool_call_approval(
        &self,
        call_id: CallId,
    ) -> Result<Option<openwave_core::ToolApproval>> {
        self.inner.get_tool_call_approval(call_id).await
    }
    async fn claim_client_tool_call(
        &self,
        id: openwave_core::CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::ClaimClientToolCallOutcome> {
        self.inner
            .claim_client_tool_call(id, chat_id, executor_id, lease_token, now, lease_expires_at)
            .await
    }
    async fn heartbeat_client_tool_call(
        &self,
        id: openwave_core::CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::HeartbeatClientToolCallOutcome> {
        self.inner
            .heartbeat_client_tool_call(id, chat_id, lease_token, now, lease_expires_at)
            .await
    }
    async fn resolve_server_tool_call(
        &self,
        id: openwave_core::CallId,
        resolution: &openwave_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::ResolveToolCallOutcome> {
        self.inner
            .resolve_server_tool_call(id, resolution, resolved_at)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn abandon_inherited_server_tool_call(
        &self,
        id: openwave_core::CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &openwave_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::ResolveToolCallOutcome> {
        self.inner
            .abandon_inherited_server_tool_call(
                id,
                chat_id,
                turn_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await
    }
    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: openwave_core::CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &openwave_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::JournaledClientToolCallOutcome> {
        self.inner
            .resolve_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await
    }
    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: openwave_core::CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &openwave_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<openwave_core::JournaledClientToolCallOutcome> {
        self.inner
            .resolve_expired_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await
    }
    async fn list_pending_client_tool_calls(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<openwave_core::ToolCallRecord>> {
        self.inner.list_pending_client_tool_calls(chat_id).await
    }
    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<openwave_core::ToolCallRecord>> {
        self.inner.list_tool_calls(chat_id).await
    }
    async fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.inner.get_setting(key).await
    }
    async fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.inner.set_setting(key, value).await
    }
    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
        if matches!(
            event,
            AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnFailed { .. }
                | AgentEvent::TurnCancelled { .. }
        ) && self
            .blocked
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
        self.inner.append_event(chat_id, event).await
    }
    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        self.inner.list_events(chat_id, after).await
    }
}

/// A router over a fresh temp SQLite store with the given provider; returns
/// the router, token, the store (to inspect the journal), and the tempdir.
async fn test_app_with(
    provider: Arc<dyn ModelProvider>,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("t.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    test_app_from_parts(provider, store, dir)
}

/// The same router with no turn lane behind it.
///
/// A test that drives a turn by hand — accepting it and then claiming its
/// lease straight from the store — has to be that turn's only claimant. A live
/// `TurnWorker` scans the same queue every few hundred milliseconds, so under
/// load it can take the queued turn between the accept and the claim, and the
/// test's claim then finds nothing due. Routes that never run a turn should
/// use this instead of `test_app`.
async fn test_app_without_turn_worker() -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("t.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    (app(state), token, store, dir)
}

fn test_app_from_parts(
    provider: Arc<dyn ModelProvider>,
    store: Arc<dyn Store>,
    dir: tempfile::TempDir,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    test_app_from_parts_with_worker_config(
        provider,
        store,
        dir,
        turn_worker::TurnWorkerConfig::default(),
    )
}

fn test_app_from_parts_with_worker_config(
    provider: Arc<dyn ModelProvider>,
    store: Arc<dyn Store>,
    dir: tempfile::TempDir,
    worker_config: turn_worker::TurnWorkerConfig,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(&state, worker_config);
    (app(state), token, store, dir)
}

async fn test_app_with_scanner_resolution_race(
    provider: Arc<dyn ModelProvider>,
    configure: impl FnOnce(&PauseTerminalStore),
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, inner) = temp_db_store("t.db").await;
    let inner: Arc<dyn Store> = Arc::new(inner);
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    configure(&injected);
    let store: Arc<dyn Store> = injected;
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(
        &state,
        turn_worker::TurnWorkerConfig {
            max_concurrency: 1,
            ..turn_worker::TurnWorkerConfig::default()
        },
    );
    (app(state), token, store, dir)
}

/// Shrink the retry backoff so failure-injection tests observe the next
/// attempt without waiting out the production schedule.
fn fast_retry_schedule() -> crate::retry::RetrySchedule {
    crate::retry::RetrySchedule::new(
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_secs(600),
    )
}

fn spawn_turn_worker(state: &AppState) {
    spawn_turn_worker_with_config(state, turn_worker::TurnWorkerConfig::default());
}

fn spawn_turn_worker_with_config(state: &AppState, config: turn_worker::TurnWorkerConfig) {
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        None,
        config,
    );
    tokio::spawn(worker.run());
}

async fn test_app() -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    test_app_with(Arc::new(FakeProvider)).await
}

async fn test_app_with_state() -> (
    Router,
    Arc<str>,
    AppState,
    Arc<dyn Store>,
    tempfile::TempDir,
) {
    let (dir, store) = temp_db_store("stateful-test.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    (app(state.clone()), token, state, store, dir)
}

/// Admit a background agent run under a turn this test owns outright.
///
/// The claim below is a queue scan, so the app under test must not be running
/// a turn worker — see `test_app_without_turn_worker`.
async fn admit_sandbox_for_test(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    input: &str,
) -> openwave_core::AgentRun {
    let turn_id = TurnId::new();
    store
        .accept_turn(
            turn_id,
            chat_id,
            "sandbox-test-model",
            "sandbox server test",
        )
        .await
        .unwrap();
    let turn_lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let turn = store
        .claim_turn_run(turn_lease, now, now + chrono::Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("the hand-driven turn is this test's to claim");
    match store
        .admit_sandbox_agent_run(
            turn.id,
            CallId::new(),
            input,
            turn_lease,
            turn.steer_revision,
            1,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .expect("sandbox test admission should resolve")
    {
        openwave_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected sandbox test admission: {outcome:?}"),
    }
}

/// A normal authenticated local API plus a handle to its test-only secret
/// store, for asserting web-search credential routes never touch other keys.
async fn test_app_with_secrets() -> (Router, Arc<str>, Arc<MemSecrets>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("t.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let secrets = Arc::new(MemSecrets::default());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    (app(state), token, secrets, dir)
}

/// The bearer is the gate; this is the second condition that keeps a leaked
/// one from being enough. CORS does not apply to WebSocket upgrades at all, so
/// without this a page holding the token could open the event stream — and
/// `Origin` is the header its script cannot set.
#[tokio::test]
async fn a_foreign_origin_is_refused_even_holding_the_bearer() {
    let (router, token, _store, _dir) = test_app().await;

    let refused = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // Same for the socket, which CORS never covered.
    let refused_upgrade = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats/events")
                .header(header::UPGRADE, "websocket")
                .header(
                    header::SEC_WEBSOCKET_PROTOCOL,
                    format!("openwave-v1, openwave-token.{token}"),
                )
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused_upgrade.status(), StatusCode::FORBIDDEN);

    // A name that resolved to loopback still carries the name it was reached
    // by, which is what makes rebinding visible.
    let rebound = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::HOST, "rebind.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rebound.status(), StatusCode::FORBIDDEN);

    // The packaged webview's own origin is served as before.
    let allowed = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "tauri://localhost")
                .header(header::HOST, "127.0.0.1:7777")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test]
async fn app_state_roots_blob_storage_under_the_data_directory() {
    let (dir, store) = temp_db_store("t.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    let id = uuid::Uuid::new_v4();

    state.blobs.put(id, b"source bytes".to_vec()).await.unwrap();

    assert_eq!(
        state.blobs.get(id).await.unwrap().as_deref(),
        Some(&b"source bytes"[..])
    );
    assert!(dir
        .path()
        .join("blobs")
        .join(format!("{id}.blob"))
        .is_file());
}

async fn json_body<T: DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The connected-app record id behind one configured server name — how app
/// fixtures learn what to bind after a `PUT /mcp/servers` created the record.
async fn connected_app_id(store: &Arc<dyn Store>, name: &str) -> openwave_core::id::ConnectedAppId {
    store
        .list_connected_apps()
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.name == name)
        .map(|record| record.id)
        .unwrap_or_else(|| panic!("no connected app named {name:?} is configured"))
}

/// Create a chat and return it.
async fn make_chat(router: &Router, bearer: &str) -> Chat {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await
}

/// POST a message to a chat, returning the response status.
async fn send_message(router: &Router, bearer: &str, chat: ChatId, content: &str) -> StatusCode {
    send_message_with_id(router, bearer, chat, TurnId::new(), content).await
}

async fn send_message_with_id(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    turn_id: TurnId,
    content: &str,
) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/messages"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"turn_id": turn_id, "content": content}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// POST `/chats/{id}/cancel`, returning the response status.
async fn cancel_turn(router: &Router, bearer: &str, chat: ChatId, turn_id: TurnId) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/cancel"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"turn_id": turn_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// Poll the journal until the turn terminates (or time out), returning its
/// events in sequence order.
async fn wait_for_turn(store: &Arc<dyn Store>, chat: ChatId) -> Vec<SequencedEvent> {
    for _ in 0..500 {
        let events = store.list_events(chat, 0).await.unwrap();
        if events.iter().any(|e| {
            matches!(
                e.event,
                AgentEvent::TurnCompleted { .. }
                    | AgentEvent::TurnRefused { .. }
                    | AgentEvent::TurnFailed { .. }
                    | AgentEvent::TurnCancelled { .. }
            )
        }) {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("turn did not finish within the timeout");
}

/// POST exact bytes to a raw document route.
async fn post_raw(
    router: &Router,
    bearer: &str,
    uri: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, bearer);
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

/// POST a chunked body to the native streamed-document route.
async fn post_streamed_raw(
    router: &Router,
    bearer: &str,
    uri: &str,
    content_type: &str,
    chunks: Vec<Vec<u8>>,
) -> axum::response::Response {
    let body = Body::from_stream(stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<_, std::convert::Infallible>(Bytes::from(chunk))),
    ));
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, content_type)
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn request_document_file_content(
    router: &Router,
    method: axum::http::Method,
    uri: &str,
    bearer: Option<&str>,
    range: Option<&str>,
    if_range: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, bearer);
    }
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    if let Some(if_range) = if_range {
        request = request.header(header::IF_RANGE, if_range);
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Create a project and return it.
async fn make_project(router: &Router, bearer: &str) -> Project {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"title": "p"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await
}

#[tokio::test]
async fn malformed_requests_get_json_errors_not_plaintext() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // A non-UUID path segment: 400 with a parseable `{ kind, message }` body,
    // not axum's default plain-text rejection.
    let bad_path = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats/not-a-uuid")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_path.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(bad_path).await;
    assert_eq!(info.kind, "bad_request");

    // A body with no `Content-Type: application/json`: also a JSON 400.
    let no_content_type = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_content_type.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(no_content_type).await;
    assert_eq!(info.kind, "bad_request");
}

#[test]
fn one_server_process_owns_a_desktop_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());
    let first = InstanceLock::acquire(&config).unwrap();
    assert!(InstanceLock::acquire(&config).is_err());

    drop(first);
    InstanceLock::acquire(&config).expect("dropping the server releases its directory lock");
}

/// The MCP App view frame contract, at the HTTP boundary: minting requires
/// the bearer, the frame route does not (an iframe carries no headers), the
/// served document brings its own strict CSP, and a token redeems exactly
/// once.
#[tokio::test]
async fn mcp_view_frames_are_single_use_capabilities_with_their_own_csp() {
    use axum::routing::post as axum_post;

    async fn fake_mcp(body: String) -> ([(&'static str, &'static str); 1], String) {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => serde_json::json!({
                "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "frame-fixture", "version": "1"}
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "viewer",
                    "description": "Tool with a declared view",
                    "inputSchema": {"type": "object"},
                    "_meta": {"ui": {"resourceUri": "ui://fixture/app.html"}}
                }]
            }),
            "resources/read" => serde_json::json!({
                "contents": [{
                    "uri": "ui://fixture/app.html",
                    "mimeType": "text/html;profile=mcp-app",
                    "text": "<html><script>render()</script></html>"
                }]
            }),
            _ => serde_json::json!({}),
        };
        (
            [("content-type", "application/json")],
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/mcp", axum_post(fake_mcp)))
            .await
            .unwrap();
    });

    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header("authorization", &bearer)
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    "{{\"servers\":[{{\"name\":\"gateway\",\"url\":\"http://{mcp_address}/mcp\",\"request_timeout_ms\":30000,\"enabled\":true}}]}}"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    // Minting requires the bearer.
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/mcp/servers/gateway/view-session")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"uri":"ui://fixture/app.html"}"#))
            .unwrap()
    };
    let unauthenticated = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let minted = router
        .clone()
        .oneshot({
            let mut request = request();
            request
                .headers_mut()
                .insert("authorization", bearer.parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let session: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(minted.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let frame_path = session["frame_path"].as_str().unwrap().to_string();
    assert!(frame_path.starts_with("/mcp/view-frames/"));

    // The frame redeems once, without auth, with its own strict policy.
    let frame = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&frame_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(frame.status(), StatusCode::OK);
    let csp = frame
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'unsafe-inline'"));
    assert!(csp.contains("connect-src 'none'"));
    // The opaque origin is asserted by the response, not left to whoever
    // embeds it: without this an embedder that forgot `sandbox` would give the
    // document the API server's own origin, and with it the bearer.
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"));
    assert_eq!(
        frame
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let body = axum::body::to_bytes(frame.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"<html><script>render()</script></html>");

    // Replay is refused: the capability is spent.
    let replay = router
        .oneshot(
            Request::builder()
                .uri(&frame_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NOT_FOUND);
}

/// Stored local-app revisions ride the same frame contract as MCP views:
/// bearer-guarded minting, unauthenticated single-use redemption, the same
/// strict CSP — with the document loaded from the revision's write-once
/// profile bytes instead of a live MCP session, and a soft-deleted app
/// minting nothing.
#[tokio::test]
async fn app_view_frames_serve_stored_revisions_under_the_same_contract() {
    use openwave_core::id::{AppId, AppRevisionId};
    use openwave_core::local_app::{
        app_revision_relative_path, AppManifest, CreateApp, NewAppRevision,
    };

    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let app_id = AppId::new();
    let revision_id = AppRevisionId::new();
    let bundle = b"<html><script>renderApp()</script></html>".to_vec();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: revision_id,
                manifest: AppManifest {
                    name: "Fixture app".into(),
                    bindings: Vec::new(),
                },
                byte_len: bundle.len() as u64,
                sha256: [7u8; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    let bundle_path = dir
        .path()
        .join(app_revision_relative_path(app_id, revision_id));
    std::fs::create_dir_all(bundle_path.parent().unwrap()).unwrap();
    std::fs::write(&bundle_path, &bundle).unwrap();

    let mint = |id: AppId, authorization: Option<&str>| {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/apps/{id}/view-session"))
            .header("content-type", "application/json");
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        request.body(Body::from("{}")).unwrap()
    };

    // Minting requires the bearer and an existing app.
    let unauthenticated = router.clone().oneshot(mint(app_id, None)).await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let unknown = router
        .clone()
        .oneshot(mint(AppId::new(), Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let minted = router
        .clone()
        .oneshot(mint(app_id, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let session: serde_json::Value = json_body(minted).await;
    let frame_path = session["frame_path"].as_str().unwrap().to_string();
    assert!(frame_path.starts_with("/apps/view-frames/"));

    // The frame redeems once, without auth, under the same strict policy the
    // MCP view frame carries.
    let frame = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&frame_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(frame.status(), StatusCode::OK);
    let csp = frame
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'unsafe-inline'"));
    assert!(csp.contains("connect-src 'none'"));
    // The opaque origin is asserted by the response, not left to whoever
    // embeds it: without this an embedder that forgot `sandbox` would give the
    // document the API server's own origin, and with it the bearer.
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"));
    assert_eq!(
        frame
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let body = to_bytes(frame.into_body(), 2 * 1024 * 1024).await.unwrap();
    assert_eq!(body.as_ref(), bundle.as_slice());

    // Replay is refused: the capability is spent.
    let replay = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&frame_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NOT_FOUND);

    // A soft-deleted app mints nothing until restored.
    assert!(store.delete_app(app_id, chrono::Utc::now()).await.unwrap());
    let deleted = router.oneshot(mint(app_id, Some(&bearer))).await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
}

/// A checkpoint the host expects an executor lane to run.
pub(crate) fn dispatchable(
    call: &SandboxToolCallRequest,
) -> openwave_core::SandboxToolCallParkEntry {
    openwave_core::SandboxToolCallParkEntry {
        call: call.clone(),
        resolution: None,
    }
}
