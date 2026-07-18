use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use futures::stream::{self, BoxStream, StreamExt};
// Tests use the in-memory store; production wires LanceDB in `bind`.
use openwave_core::{
    AcceptAgentRunOutcome, AgentErrorInfo, AgentEvent, AgentRunExecution, AgentRunId,
    AgentRunInboxStatus, AgentRunStatus, ApprovalClass, BeginRootAttachmentChange, BlobStore,
    CallId, Chat, ChatId, ChatRequest, ChatRootAttachment, ClientToolCallRequest, ContentBlock,
    HostRootId, Message, MessageId, ModelProvider, ParkSandboxToolCallOutcome,
    ParkTurnForClientCallOutcome, Project, ProjectId, ProviderEvent, ProviderId, Role,
    RootAttachmentChangeAction, RootAttachmentChangeId, RootAttachmentOrigin,
    SandboxToolCallRequest, SecretProvider, SequencedEvent, StopReason, Tool, ToolCallExecution,
    ToolCallRecord, ToolCallResolution, ToolCallStatus, ToolCtx, ToolOutput, ToolSpec,
    TurnCheckpointProgress, TurnId, TurnRunStatus, TurnSteerId, Usage,
};
use openwave_retrieval::{
    Embedding, InMemoryVectorStore, RetrievalError, ScoredChunk, VectorRecord,
};
use resolver::ProviderResolver;
use serde::de::DeserializeOwned;
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::event_projection::{RendererAgentEvent, RendererSequencedEvent};

mod root_attachment;

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

/// Drives the complete foreground-child-foreground round trip. The sandbox
/// request is identified by its absence of the foreground delegation contract;
/// its fixed web-search capability is deliberately separate.
#[derive(Default)]
struct SandboxRoundTripProvider {
    foreground_calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for SandboxRoundTripProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-round-trip")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let sandbox = !request
            .tools
            .iter()
            .any(|tool| tool.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL);
        self.requests.lock().unwrap().push(request);
        let events = if sandbox {
            vec![
                ProviderEvent::TextDelta {
                    text: "child result".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        } else if self.foreground_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "delegate-1".into(),
                    name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"task":"Return a concise child result."}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "parent resumed".into(),
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
        Ok(stream::iter(vec![ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        }])
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
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }
        })
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

struct FailingEmbedder;

#[async_trait]
impl Embedder for FailingEmbedder {
    fn dimensions(&self) -> usize {
        8
    }

    async fn embed_documents(
        &self,
        _texts: &[String],
    ) -> openwave_retrieval::Result<Vec<Embedding>> {
        Err(RetrievalError::embed("injected embedding failure"))
    }
}

struct FailAfterFirstBatchEmbedder {
    inner: HashEmbedder,
    calls: AtomicUsize,
}

struct FailNextDeleteVectorStore {
    inner: InMemoryVectorStore,
    fail_delete: std::sync::atomic::AtomicBool,
}

impl FailNextDeleteVectorStore {
    fn new(dimensions: usize) -> Self {
        Self {
            inner: InMemoryVectorStore::new(dimensions),
            fail_delete: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn fail_next_delete(&self) {
        self.fail_delete.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl VectorStore for FailNextDeleteVectorStore {
    async fn upsert(&self, records: Vec<VectorRecord>) -> openwave_retrieval::Result<()> {
        self.inner.upsert(records).await
    }

    async fn query_with_options(
        &self,
        query_text: &str,
        query: &Embedding,
        k: usize,
        options: openwave_retrieval::SearchOptions,
    ) -> openwave_retrieval::Result<Vec<ScoredChunk>> {
        self.inner
            .query_with_options(query_text, query, k, options)
            .await
    }

    async fn replace_document(
        &self,
        document_id: openwave_core::DocumentId,
        records: Vec<VectorRecord>,
    ) -> openwave_retrieval::Result<()> {
        if records.is_empty() && self.fail_delete.swap(false, Ordering::SeqCst) {
            return Err(RetrievalError::vector_store("injected delete failure"));
        }
        self.inner.replace_document(document_id, records).await
    }

    async fn stage_document_generation(
        &self,
        document_id: openwave_core::DocumentId,
        generation: openwave_core::DocumentGeneration,
        records: Vec<VectorRecord>,
    ) -> openwave_retrieval::Result<openwave_retrieval::GenerationStageOutcome> {
        if records.is_empty() && self.fail_delete.swap(false, Ordering::SeqCst) {
            return Err(RetrievalError::vector_store("injected tombstone failure"));
        }
        self.inner
            .stage_document_generation(document_id, generation, records)
            .await
    }

    async fn activate_document_generation(
        &self,
        document_id: openwave_core::DocumentId,
        generation: openwave_core::DocumentGeneration,
    ) -> openwave_retrieval::Result<bool> {
        self.inner
            .activate_document_generation(document_id, generation)
            .await
    }

    async fn active_document_generation(
        &self,
        document_id: openwave_core::DocumentId,
    ) -> openwave_retrieval::Result<Option<openwave_core::DocumentGeneration>> {
        self.inner.active_document_generation(document_id).await
    }

    async fn newest_document_generation(
        &self,
        document_id: openwave_core::DocumentId,
    ) -> openwave_retrieval::Result<Option<openwave_retrieval::DocumentGenerationState>> {
        self.inner.newest_document_generation(document_id).await
    }

    async fn document_len(
        &self,
        document_id: openwave_core::DocumentId,
    ) -> openwave_retrieval::Result<Option<usize>> {
        self.inner.document_len(document_id).await
    }

    async fn len(&self) -> openwave_retrieval::Result<usize> {
        self.inner.len().await
    }
}

#[async_trait]
impl Embedder for FailAfterFirstBatchEmbedder {
    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn fingerprint(&self) -> String {
        "test-fail-after-first-v1".into()
    }

    async fn embed_documents(
        &self,
        texts: &[String],
    ) -> openwave_retrieval::Result<Vec<Embedding>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err(RetrievalError::embed("injected update failure"));
        }
        self.inner.embed_documents(texts).await
    }

    async fn embed_query(&self, text: &str) -> openwave_retrieval::Result<Embedding> {
        self.inner.embed_query(text).await
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
    fail_after_park_commit: std::sync::atomic::AtomicBool,
    fail_after_apply_steer_commit: std::sync::atomic::AtomicBool,
    cancel_after_apply_steer_commit: std::sync::atomic::AtomicBool,
    pause_before_steer_read: std::sync::atomic::AtomicBool,
    advance_before_steer_read: std::sync::atomic::AtomicBool,
    fail_terminal_recovery: std::sync::atomic::AtomicBool,
    terminal_recovery_calls: AtomicUsize,
    scan_before_failure_resolution: std::sync::atomic::AtomicBool,
    scan_before_cancellation_ack: std::sync::atomic::AtomicBool,
    pause_nonterminal_event: std::sync::atomic::AtomicBool,
    pause_accept: std::sync::atomic::AtomicBool,
    fail_document_delete: std::sync::atomic::AtomicBool,
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
            fail_after_park_commit: std::sync::atomic::AtomicBool::new(false),
            fail_after_apply_steer_commit: std::sync::atomic::AtomicBool::new(false),
            cancel_after_apply_steer_commit: std::sync::atomic::AtomicBool::new(false),
            pause_before_steer_read: std::sync::atomic::AtomicBool::new(false),
            advance_before_steer_read: std::sync::atomic::AtomicBool::new(false),
            fail_terminal_recovery: std::sync::atomic::AtomicBool::new(false),
            terminal_recovery_calls: AtomicUsize::new(0),
            scan_before_failure_resolution: std::sync::atomic::AtomicBool::new(false),
            scan_before_cancellation_ack: std::sync::atomic::AtomicBool::new(false),
            pause_nonterminal_event: std::sync::atomic::AtomicBool::new(false),
            pause_accept: std::sync::atomic::AtomicBool::new(false),
            fail_document_delete: std::sync::atomic::AtomicBool::new(false),
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

    fn fail_after_next_park_commit(&self) {
        self.fail_after_park_commit.store(true, Ordering::SeqCst);
    }

    fn fail_after_next_apply_steer_commit(&self) {
        self.fail_after_apply_steer_commit
            .store(true, Ordering::SeqCst);
    }

    fn cancel_after_next_apply_steer_commit(&self) {
        self.cancel_after_apply_steer_commit
            .store(true, Ordering::SeqCst);
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
        let outcome = self
            .inner
            .claim_turn_run(
                uuid::Uuid::new_v4(),
                now,
                now + chrono::Duration::seconds(1),
            )
            .await?;
        if outcome.terminal_event.is_none() {
            return Err(AgentError::Store(
                "injected scan did not terminalize the turn".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl Store for PauseTerminalStore {
    async fn create_project(&self, project: &Project) -> Result<()> {
        self.inner.create_project(project).await
    }
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        self.inner.get_project(id).await
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
    async fn get_document_generation(
        &self,
        id: openwave_core::DocumentId,
    ) -> Result<Option<openwave_core::DocumentGeneration>> {
        self.inner.get_document_generation(id).await
    }
    async fn list_pending_document_retirements(
        &self,
        after: Option<openwave_core::DocumentId>,
        limit: u64,
    ) -> Result<Vec<(openwave_core::DocumentId, openwave_core::DocumentGeneration)>> {
        self.inner
            .list_pending_document_retirements(after, limit)
            .await
    }
    async fn get_pending_document_retirement(
        &self,
        id: openwave_core::DocumentId,
    ) -> Result<Option<openwave_core::DocumentGeneration>> {
        self.inner.get_pending_document_retirement(id).await
    }
    async fn complete_document_retirement(
        &self,
        id: openwave_core::DocumentId,
        generation: openwave_core::DocumentGeneration,
    ) -> Result<bool> {
        self.inner
            .complete_document_retirement(id, generation)
            .await
    }
    async fn delete_document(
        &self,
        id: openwave_core::DocumentId,
    ) -> Result<openwave_core::DocumentGeneration> {
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
    async fn upsert_document_and_enqueue_index(
        &self,
        document: &openwave_core::DocumentUpsert,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<(openwave_core::DocumentRecord, openwave_core::DocumentJob)> {
        self.inner
            .upsert_document_and_enqueue_index(document, pipeline_fingerprint, max_attempts)
            .await
    }
    async fn accept_document_source_and_enqueue_parse(
        &self,
        source: &openwave_core::DocumentSourceUpsert,
        parser_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<(openwave_core::DocumentRecord, openwave_core::DocumentJob)> {
        self.inner
            .accept_document_source_and_enqueue_parse(source, parser_fingerprint, max_attempts)
            .await
    }
    async fn get_document_job(
        &self,
        id: openwave_core::DocumentJobId,
    ) -> Result<Option<openwave_core::DocumentJob>> {
        self.inner.get_document_job(id).await
    }
    async fn list_document_jobs(
        &self,
        document_id: openwave_core::DocumentId,
    ) -> Result<Vec<openwave_core::DocumentJob>> {
        self.inner.list_document_jobs(document_id).await
    }
    async fn claim_document_job(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<openwave_core::DocumentJob>> {
        self.inner.claim_document_job(now, lease_expires_at).await
    }
    async fn heartbeat_document_job(
        &self,
        id: openwave_core::DocumentJobId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        self.inner
            .heartbeat_document_job(id, lease_token, now, lease_expires_at)
            .await
    }
    async fn complete_document_index_job(
        &self,
        id: openwave_core::DocumentJobId,
        lease_token: uuid::Uuid,
        completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        self.inner
            .complete_document_index_job(id, lease_token, completed_at)
            .await
    }
    async fn record_document_job_failure(
        &self,
        id: openwave_core::DocumentJobId,
        lease_token: uuid::Uuid,
        failed_at: chrono::DateTime<chrono::Utc>,
        retry_at: Option<chrono::DateTime<chrono::Utc>>,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<openwave_core::DocumentJobStatus>> {
        self.inner
            .record_document_job_failure(
                id,
                lease_token,
                failed_at,
                retry_at,
                error_code,
                error_detail,
            )
            .await
    }
    async fn mark_document_indexed(
        &self,
        id: openwave_core::DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
        fingerprint: &str,
        indexed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        self.inner
            .mark_document_indexed(id, revision, revision_token, fingerprint, indexed_at)
            .await
    }
    async fn clear_document_index(
        &self,
        id: openwave_core::DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
    ) -> Result<bool> {
        self.inner
            .clear_document_index(id, revision, revision_token)
            .await
    }
    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        self.inner.create_chat(chat).await
    }
    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat> {
        self.inner.create_chat_with_project_defaults(chat).await
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
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
    ) -> Result<bool> {
        self.inner.update_chat_metadata(id, title, model).await
    }
    async fn get_turn_run(&self, id: TurnId) -> Result<Option<openwave_core::TurnRun>> {
        self.inner.get_turn_run(id).await
    }
    async fn list_turn_runs(&self, chat_id: ChatId) -> Result<Vec<openwave_core::TurnRun>> {
        self.inner.list_turn_runs(chat_id).await
    }
    async fn accept_turn(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
    ) -> Result<openwave_core::AcceptTurnOutcome> {
        if self.pause_accept.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.accept_turn(id, chat_id, model, content).await
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
                now,
            )
            .await?;
        if applied.is_some()
            && self
                .cancel_after_apply_steer_commit
                .swap(false, Ordering::SeqCst)
        {
            self.inner
                .request_turn_cancellation_and_append_event(turn_id, chrono::Utc::now())
                .await?
                .ok_or_else(|| {
                    AgentError::Store(
                        "injected cancellation could not follow steer application".into(),
                    )
                })?;
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
            .finish_turn_cancellation_and_append_event(id, lease_token, now, usage)
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
    async fn append_message(&self, message: &Message) -> Result<()> {
        self.inner.append_message(message).await
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
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    test_app_with_retrieval(provider, retrieval).await
}

async fn test_app_with_retrieval(
    provider: Arc<dyn ModelProvider>,
    retrieval: Arc<Retriever>,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    test_app_from_parts(provider, retrieval, store, dir)
}

async fn test_app_with_worker() -> (
    Router,
    Arc<str>,
    Arc<dyn Store>,
    tempfile::TempDir,
    document_worker::DocumentWorker,
) {
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await
}

async fn test_app_with_retrieval_and_worker(
    provider: Arc<dyn ModelProvider>,
    retrieval: Arc<Retriever>,
) -> (
    Router,
    Arc<str>,
    Arc<dyn Store>,
    tempfile::TempDir,
    document_worker::DocumentWorker,
) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let worker = document_worker::DocumentWorker::new(
        store.clone(),
        state.blobs.clone(),
        retrieval,
        state.document_job_wake.clone(),
        state.document_writes.clone(),
        document_worker::DocumentWorkerConfig::default(),
    );
    spawn_turn_worker(&state);
    (app(state), token, store, dir, worker)
}

async fn run_parse_and_index(worker: &document_worker::DocumentWorker) {
    for _ in 0..2 {
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
    }
}

fn test_app_from_parts(
    provider: Arc<dyn ModelProvider>,
    retrieval: Arc<Retriever>,
    store: Arc<dyn Store>,
    dir: tempfile::TempDir,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    (app(state), token, store, dir)
}

async fn test_app_with_scanner_resolution_race(
    provider: Arc<dyn ModelProvider>,
    configure: impl FnOnce(&PauseTerminalStore),
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    configure(&injected);
    let store: Arc<dyn Store> = injected;
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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

fn spawn_turn_worker(state: &AppState) {
    spawn_turn_worker_with_config(state, turn_worker::TurnWorkerConfig::default());
}

fn spawn_turn_worker_with_config(state: &AppState, config: turn_worker::TurnWorkerConfig) {
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
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

/// A normal authenticated local API plus a handle to its test-only secret
/// store, for asserting web-search credential routes never touch other keys.
async fn test_app_with_web_search_secrets() -> (Router, Arc<str>, Arc<MemSecrets>, tempfile::TempDir)
{
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let secrets = Arc::new(MemSecrets::default());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    (app(state), token, secrets, dir)
}

#[tokio::test]
async fn app_state_roots_blob_storage_under_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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

#[tokio::test]
async fn cancel_stops_a_running_turn() {
    // A turn that blocks in the provider (a stand-in for a long model call),
    // so it stays running until we cancel it.
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );

    // Acceptance is durable before 202, so cancellation works whether the
    // asynchronous worker still sees queued work or already holds its lease.
    let cancel_status = cancel_turn(&router, &bearer, chat.id, turn_id).await;
    assert_eq!(
        cancel_status,
        StatusCode::ACCEPTED,
        "turn after cancel response: {:?}",
        store.get_turn_run(turn_id).await.unwrap()
    );

    // The turn preempts the blocked provider call and ends as cancelled —
    // note we never release `gate`, so only the cancel can end it.
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_drains_buffered_preassigned_event_ordinals() {
    struct TwoDeltasThenPark {
        second_yielded: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for TwoDeltasThenPark {
        fn id(&self) -> ProviderId {
            ProviderId::new("two-deltas-then-park")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let second_yielded = self.second_yielded.clone();
            Ok(stream::iter(vec![ProviderEvent::TextDelta {
                text: "first".into(),
            }])
            .chain(stream::once(async move {
                second_yielded.notify_one();
                ProviderEvent::TextDelta {
                    text: "second".into(),
                }
            }))
            .chain(stream::pending())
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let append_entered = Arc::new(Notify::new());
    let append_release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        append_entered.clone(),
        append_release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_nonterminal_event();
    let store: Arc<dyn Store> = injected;
    let second_yielded = Arc::new(Notify::new());
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(TwoDeltasThenPark {
            second_yielded: second_yielded.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let append_blocked = append_entered.notified();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), append_blocked)
        .await
        .expect("worker reached the first buffered event append");
    tokio::time::timeout(Duration::from_secs(2), second_yielded.notified())
        .await
        .expect("agent yielded the following preassigned event");

    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    append_release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test]
async fn cancel_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // Known chat, nothing running → 409.
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, TurnId::new()).await,
        StatusCode::CONFLICT
    );
    // Unknown chat → 404.
    assert_eq!(
        cancel_turn(&router, &bearer, ChatId::new(), TurnId::new()).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cancel_cannot_target_a_turn_through_another_chat() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(GatedProvider { gate })).await;
    let bearer = format!("Bearer {token}");
    let owner = make_chat(&router, &bearer).await;
    let other = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, owner.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        cancel_turn(&router, &bearer, other.id, turn_id).await,
        StatusCode::CONFLICT
    );
    assert_ne!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        openwave_core::TurnRunStatus::Cancelled
    );
    assert_eq!(
        cancel_turn(&router, &bearer, owner.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, owner.id).await;
}

/// POST `/chats/{id}/steer`, returning the response status.
async fn steer_turn(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    turn_id: TurnId,
    content: &str,
    interrupt: bool,
) -> StatusCode {
    steer_turn_with_id(
        router,
        bearer,
        chat,
        TurnSteerId::new(),
        turn_id,
        content,
        interrupt,
    )
    .await
}

async fn steer_turn_with_id(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    steer_id: TurnSteerId,
    turn_id: TurnId,
    content: &str,
    interrupt: bool,
) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/steer"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "steer_id": steer_id,
                        "turn_id": turn_id,
                        "content": content,
                        "interrupt": interrupt
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn steer_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        steer_turn(&router, &bearer, chat.id, TurnId::new(), "hi", false).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        steer_turn(&router, &bearer, ChatId::new(), TurnId::new(), "hi", false,).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, TurnId::new(), "  ", false).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            TurnSteerId(uuid::Uuid::nil()),
            TurnId::new(),
            "hi",
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            "contains\0nul",
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            &"x".repeat(openwave_core::TurnSteer::MAX_CONTENT_LEN + 1),
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn interrupt_steer_preempts_a_running_turn_and_continues() {
    // Stall after the first delta so steer can interrupt; then finish.
    struct StallThenFinish {
        calls: AtomicUsize,
        entered: Arc<Notify>,
    }
    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("stall-then-finish")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
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

    let entered = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(StallThenFinish {
        calls: AtomicUsize::new(0),
        entered: entered.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered before the interrupt steer");
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
        )
        .await,
        StatusCode::ACCEPTED,
        "an exact admission retry is idempotent"
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "different request data",
            true,
        )
        .await,
        StatusCode::CONFLICT,
        "reusing an identity for different input must fail"
    );

    let events = wait_for_turn(&store, chat.id).await;
    let stream_interrupted_at = events
        .iter()
        .position(|e| matches!(e.event, AgentEvent::StreamInterrupted));
    let user_steered_at = events.iter().position(|e| {
        matches!(
            &e.event,
            AgentEvent::UserSteered { content, .. } if content == "change course"
        )
    });
    assert!(
        matches!((stream_interrupted_at, user_steered_at), (Some(a), Some(b)) if a < b),
        "interrupted stream is marked before steer is injected"
    );
    assert!(events.iter().any(|e| matches!(
        &e.event,
        AgentEvent::UserSteered { content, .. } if content == "change course"
    )));
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let mut visible_assistant = String::new();
    for event in events.iter().map(|e| &e.event) {
        match event {
            AgentEvent::TextDelta { text } => visible_assistant.push_str(text),
            AgentEvent::StreamInterrupted => visible_assistant.clear(),
            _ => {}
        }
    }
    assert_eq!(visible_assistant, "after steer");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::TurnCancelled { .. })),
        "steer continues the turn"
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "change course",
            ),
            (
                messages[2].id,
                openwave_core::Role::Assistant,
                "after steer"
            ),
        ]
    );
    assert!(matches!(
        store
            .accept_turn_steer(steer_id, turn_id, chat.id, "change course", true)
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Existing(openwave_core::TurnSteer {
            status: openwave_core::TurnSteerStatus::Applied,
            ..
        })
    ));
}

#[tokio::test]
async fn boundary_steer_commits_the_candidate_and_instruction_atomically() {
    struct FinishAfterGate {
        calls: Arc<AtomicUsize>,
        entered: Arc<Notify>,
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for FinishAfterGate {
        fn id(&self) -> ProviderId {
            ProviderId::new("finish-after-gate")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                let gate = self.gate.clone();
                return Ok(stream::iter(vec![ProviderEvent::TextDelta {
                    text: "candidate".into(),
                }])
                .chain(stream::once(async move {
                    gate.notified().await;
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    }
                }))
                .boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "final".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(FinishAfterGate {
        calls: calls.clone(),
        entered: entered.clone(),
        gate: gate.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered the first boundary generation");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "continue with this",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    gate.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "continue with this"
            ))
            .count(),
        1,
        "boundary steering must publish its committed event once"
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (messages[1].id, openwave_core::Role::Assistant, "candidate",),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "continue with this",
            ),
            (messages[3].id, openwave_core::Role::Assistant, "final"),
        ]
    );
}

#[tokio::test]
async fn durable_steer_poll_recovers_a_missing_local_notification() {
    struct StallThenFinish {
        calls: AtomicUsize,
        entered: Arc<Notify>,
    }
    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("durable-steer-poll")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                return Ok(stream::pending().boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "after durable poll".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let provider = Arc::new(StallThenFinish {
        calls: AtomicUsize::new(0),
        entered: Arc::new(Notify::new()),
    });
    let entered = provider.entered.clone();
    let (router, token, store, _dir) = test_app_with(provider.clone()).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered the generation before durable admission");

    let steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                steer_id,
                turn_id,
                chat.id,
                "recover from the database",
                true,
            )
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Accepted(_)
    ));

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(events.iter().any(|event| matches!(
        &event.event,
        AgentEvent::UserSteered { content, .. } if content == "recover from the database"
    )));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert!(matches!(
        store
            .accept_turn_steer(
                steer_id,
                turn_id,
                chat.id,
                "recover from the database",
                true,
            )
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Existing(openwave_core::TurnSteer {
            status: openwave_core::TurnSteerStatus::Applied,
            ..
        })
    ));
}

#[tokio::test]
async fn durable_steer_retries_heartbeat_races_and_ambiguous_application() {
    struct StallThenFinish {
        calls: Arc<AtomicUsize>,
        entered: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("durable-steer-retry")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                return Ok(stream::pending().boxed());
            }
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

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let steer_read_entered = Arc::new(Notify::new());
    let release_steer_read = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        steer_read_entered.clone(),
        release_steer_read.clone(),
    ));
    injected.do_not_pause_terminal();
    let store: Arc<dyn Store> = injected.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_entered = Arc::new(Notify::new());
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(StallThenFinish {
            calls: calls.clone(),
            entered: provider_entered.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), provider_entered.notified())
        .await
        .expect("provider entered before the ambiguous application race");

    injected.pause_before_next_steer_read();
    tokio::time::timeout(Duration::from_secs(2), steer_read_entered.notified())
        .await
        .expect("steer poll paused before reading the durable queue");
    injected.advance_before_next_steer_read();
    injected.fail_after_next_apply_steer_commit();
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "recover exactly",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release_steer_read.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "recover exactly"
            ))
            .count(),
        1,
        "ambiguous application recovery must publish its committed event once"
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "recover exactly",
            ),
            (messages[2].id, openwave_core::Role::Assistant, "recovered"),
        ]
    );
}

#[tokio::test]
async fn committed_steer_event_recovers_when_cancellation_wins_ambiguous_response() {
    struct NeverFinish {
        entered: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for NeverFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("never-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.entered.notify_one();
            Ok(stream::pending().boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.cancel_after_next_apply_steer_commit();
    let store: Arc<dyn Store> = injected;
    let entered = Arc::new(Notify::new());
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(NeverFinish {
            entered: entered.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(
        &state,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(20),
            steer_poll: Duration::from_millis(5),
            idle_min: Duration::from_millis(5),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(5),
            max_concurrency: 1,
        },
    );
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered before the cancellation race");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "apply before cancellation",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "apply before cancellation"
            ))
            .count(),
        1,
        "exact recovery must publish the atomically committed steer event"
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "apply before cancellation",
            ),
        ]
    );
}

#[tokio::test]
async fn queued_steer_is_applied_when_the_worker_claims_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "queued direction",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );

    spawn_turn_worker(&state);
    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        &event.event,
        AgentEvent::UserSteered { content, .. } if content == "queued direction"
    )));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "queued direction",
            ),
            (messages[2].id, openwave_core::Role::Assistant, "hi"),
        ]
    );
}

/// POST a JSON body to `uri`, returning the response.
async fn post_json(
    router: &Router,
    bearer: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST a JSON body through the native-only client-executor boundary.
async fn post_native_json(
    router: &Router,
    bearer: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn park_client_wait_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
    progress: TurnCheckpointProgress,
) -> (TurnId, ClientToolCallRequest) {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "fake", "native action")
        .await
        .unwrap();
    let turn_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    let claimed = store
        .claim_turn_run(
            turn_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.id, turn_id);
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_token,
                0,
                progress,
                chrono::Utc::now(),
                &call,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Parked { .. }
    ));
    (turn_id, call)
}

fn test_client_checkpoint_progress(model_steps: i32) -> TurnCheckpointProgress {
    TurnCheckpointProgress {
        model_steps,
        usage: Usage {
            input_tokens: 13,
            output_tokens: 8,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 3,
        },
    }
}

async fn resolve_parked_client_call(
    store: &dyn Store,
    chat_id: ChatId,
    call: &ClientToolCallRequest,
) {
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            call.id,
            chat_id,
            uuid::Uuid::new_v4(),
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let resolved_at = chrono::Utc::now();
    let resolved = store
        .resolve_client_tool_call_and_append_event(
            call.id,
            chat_id,
            lease_token,
            resolved_at,
            &ToolCallResolution::Completed {
                result: "connected-root".into(),
            },
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(
        resolved.outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    );
    assert!(matches!(
        resolved.turn,
        Some(turn) if turn.status == TurnRunStatus::Resuming
    ));
}

#[tokio::test]
async fn client_execution_api_polls_claims_heartbeats_and_resolves_idempotently() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let proposed_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_1".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"suggested_name": "Documents"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let call = match store.accept_tool_call(&proposed_call).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call)
        | openwave_core::AcceptToolCallOutcome::Existing(call) => call,
        openwave_core::AcceptToolCallOutcome::IdentityConflict => {
            panic!("fresh client tool call identity conflicted")
        }
    };

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending/raw", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::OK);
    let pending: Vec<ToolCallRecord> = json_body(pending).await;
    assert_eq!(pending, vec![call.clone()]);

    let executor_id = uuid::Uuid::new_v4();
    let lease_token = uuid::Uuid::new_v4();
    let claim_uri = format!("/chats/{}/client-executions/{}/claim", chat.id, call.id);
    let claim_body = serde_json::json!({
        "executor_id": executor_id,
        "lease_token": lease_token,
    });
    let renderer_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&claim_uri)
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(claim_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer_only.status(), StatusCode::UNAUTHORIZED);

    let first = post_native_json(&router, &bearer, &claim_uri, claim_body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = json_body(first).await;
    assert_eq!(first["disposition"], "claimed");
    assert_eq!(first["lease_token"], lease_token.to_string());
    assert_eq!(first["call"]["arguments"], call.arguments);

    // A lost response can be retried with the stable secret token even though
    // the server calculates a fresh proposed expiry for the second request.
    let retry = post_native_json(&router, &bearer, &claim_uri, claim_body).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");
    assert_eq!(retry["lease_token"], lease_token.to_string());

    let stolen = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": executor_id,
            "lease_token": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    assert_eq!(stolen.status(), StatusCode::CONFLICT);

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pending_bytes = to_bytes(pending.into_body(), usize::MAX).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&pending_bytes).contains(&lease_token.to_string()),
        "authoritative polling must never disclose the secret lease token"
    );

    let wrong_chat_heartbeat = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/heartbeat",
            other_chat.id, call.id
        ),
        serde_json::json!({"lease_token": lease_token}),
    )
    .await;
    assert_eq!(wrong_chat_heartbeat.status(), StatusCode::CONFLICT);

    tokio::time::sleep(Duration::from_millis(2)).await;
    let heartbeat = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/heartbeat", chat.id, call.id),
        serde_json::json!({"lease_token": lease_token}),
    )
    .await;
    assert_eq!(heartbeat.status(), StatusCode::OK);
    let heartbeat: serde_json::Value = json_body(heartbeat).await;
    assert_eq!(heartbeat["disposition"], "extended");

    let resolve_uri = format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id);
    let resolution = serde_json::json!({
        "lease_token": lease_token,
        "resolution": {"status": "completed", "result": "folder connected"},
    });
    let wrong_chat_resolve = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/resolve",
            other_chat.id, call.id
        ),
        resolution.clone(),
    )
    .await;
    assert_eq!(wrong_chat_resolve.status(), StatusCode::CONFLICT);
    let wrong_token = post_native_json(
        &router,
        &bearer,
        &resolve_uri,
        serde_json::json!({
            "lease_token": uuid::Uuid::new_v4(),
            "resolution": {"status": "completed", "result": "folder connected"},
        }),
    )
    .await;
    assert_eq!(wrong_token.status(), StatusCode::CONFLICT);

    let resolved = post_native_json(&router, &bearer, &resolve_uri, resolution.clone()).await;
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved: serde_json::Value = json_body(resolved).await;
    assert_eq!(resolved["disposition"], "resolved");

    // Resolution time is server-owned metadata, not part of the stable command
    // identity, so an ambiguous retry converges on token + terminal payload.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let retry = post_native_json(&router, &bearer, &resolve_uri, resolution).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");

    let conflicting = post_native_json(
        &router,
        &bearer,
        &resolve_uri,
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {"status": "cancelled", "result": "not connected"},
        }),
    )
    .await;
    assert_eq!(conflicting.status(), StatusCode::CONFLICT);
    assert!(store
        .list_pending_client_tool_calls(chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn renderer_pending_client_executions_are_a_closed_folder_consent_projection() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let request = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "provider-secret".into(),
        name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let request = match store.accept_tool_call(&request).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call)
        | openwave_core::AcceptToolCallOutcome::Existing(call) => call,
        openwave_core::AcceptToolCallOutcome::IdentityConflict => panic!("fresh call conflicted"),
    };
    let unrelated = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "other-provider-secret".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"host_path": "/Users/private"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&unrelated).await.unwrap();
    let malformed = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "malformed-provider-secret".into(),
        name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read /Users/private",
            "requested_capabilities": ["read_files"],
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&malformed).await.unwrap();
    let dangerous_reasons = [
        "Read `/Users/private/report.pdf` sentinel-backtick",
        "Read [/Users/private/report.pdf] sentinel-markdown",
        "Read file:///Users/private/report.pdf sentinel-file-uri",
        r"Read `\\server\share\secret.txt` sentinel-unc",
        r"Read `C:\Users\private\secret.txt` sentinel-drive",
        "ordinary-secret-prose",
    ];
    for reason in dangerous_reasons {
        let dangerous = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            provider_id: "dangerous-provider-secret".into(),
            name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": reason,
                "requested_capabilities": ["read_files"],
            }),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        };
        store.accept_tool_call(&dangerous).await.unwrap();
    }

    let renderer = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer.status(), StatusCode::OK);
    let renderer: serde_json::Value = json_body(renderer).await;
    let renderer_requests = renderer.as_array().unwrap();
    assert_eq!(renderer_requests.len(), dangerous_reasons.len() + 1);
    assert!(renderer_requests.iter().any(|value| {
        value["call_id"] == request.id.to_string()
            && value["turn_id"] == request.turn_id.to_string()
            && value["folder_hint"] == "documents"
            && value["claimed"] == false
    }));
    assert!(renderer_requests.iter().all(|value| {
        value["reason"]
            == "The assistant needs read access to files outside the folders connected to this conversation."
    }));
    let serialized = renderer.to_string();
    for forbidden in [
        "provider-secret",
        "other-provider-secret",
        "malformed-provider-secret",
        "dangerous-provider-secret",
        "request_folder_access",
        "connect_folder",
        "arguments",
        "chat_id",
        "provider_id",
        "client_executor_id",
        "status",
        "execution",
        "/Users/private",
        "sentinel-backtick",
        "sentinel-markdown",
        "sentinel-file-uri",
        "sentinel-unc",
        "sentinel-drive",
        "ordinary-secret-prose",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }

    let raw_uri = format!("/chats/{}/client-executions/pending/raw", chat.id);
    let renderer_raw = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&raw_uri)
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer_raw.status(), StatusCode::UNAUTHORIZED);

    let native_raw = router
        .oneshot(
            Request::builder()
                .uri(raw_uri)
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(native_raw.status(), StatusCode::OK);
    let native: Vec<ToolCallRecord> = json_body(native_raw).await;
    assert_eq!(native.len(), dangerous_reasons.len() + 3);
    assert!(native.iter().any(|call| call == &request));
    assert!(native.iter().any(|call| call.name == "connect_folder"));
}

#[tokio::test]
async fn client_resolution_publishes_cancellation_and_wakes_resumable_turns() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");

    let resume_chat = make_chat(&router, &bearer).await;
    let (resume_turn, resume_call) = park_client_wait_for_route_test(
        &*store,
        resume_chat.id,
        test_client_checkpoint_progress(1),
    )
    .await;
    let resume_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            resume_call.id,
            resume_chat.id,
            uuid::Uuid::new_v4(),
            resume_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let resume_response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/resolve",
            resume_chat.id, resume_call.id
        ),
        serde_json::json!({
            "lease_token": resume_token,
            "resolution": {"status": "completed", "result": "root-1"},
        }),
    )
    .await;
    assert_eq!(resume_response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), state.turn_job_wake.notified())
        .await
        .expect("resumable client resolution must wake the turn worker");
    assert_eq!(
        store
            .get_turn_run(resume_turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Resuming
    );
    store
        .request_turn_cancellation(resume_turn, chrono::Utc::now())
        .await
        .unwrap();

    let cancel_chat = make_chat(&router, &bearer).await;
    let (cancel_turn, cancel_call) = park_client_wait_for_route_test(
        &*store,
        cancel_chat.id,
        test_client_checkpoint_progress(1),
    )
    .await;
    let cancel_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            cancel_call.id,
            cancel_chat.id,
            uuid::Uuid::new_v4(),
            cancel_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .request_turn_cancellation(cancel_turn, chrono::Utc::now())
            .await
            .unwrap()
            .unwrap(),
        openwave_core::RequestTurnCancellationOutcome::Requested(turn)
            if turn.status == TurnRunStatus::CancellingClient
    ));
    let mut live = state.events.subscribe(cancel_chat.id);
    let resolve_uri = format!(
        "/chats/{}/client-executions/{}/resolve",
        cancel_chat.id, cancel_call.id
    );
    let body = serde_json::json!({
        "lease_token": cancel_token,
        "resolution": {"status": "cancelled", "result": "cancelled by user"},
    });
    let cancelled = post_native_json(&router, &bearer, &resolve_uri, body.clone()).await;
    assert_eq!(cancelled.status(), StatusCode::OK);
    let first_live = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("client-owned cancellation must publish live")
        .unwrap();
    assert!(matches!(first_live.event, AgentEvent::TurnCancelled { .. }));

    let retry = post_native_json(&router, &bearer, &resolve_uri, body).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");
    let recovered_live = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("exact retry must recover the terminal publication receipt")
        .unwrap();
    assert_eq!(recovered_live, first_live);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_worker_preserves_checkpoint_usage_and_step_budget() {
    struct CountingUsageProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for CountingUsageProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("counting-usage")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "resumed".into(),
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(CountingUsageProvider {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");

    let completed_chat = make_chat(&router, &bearer).await;
    let completed_progress = test_client_checkpoint_progress(1);
    let (_, completed_call) =
        park_client_wait_for_route_test(&*store, completed_chat.id, completed_progress).await;
    resolve_parked_client_call(&*store, completed_chat.id, &completed_call).await;
    spawn_turn_worker(&state);
    state.turn_job_wake.notify_one();
    let completed_events = wait_for_turn(&store, completed_chat.id).await;
    assert!(matches!(
        completed_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage, .. })
            if *usage == Usage {
                input_tokens: completed_progress.usage.input_tokens + 2,
                output_tokens: completed_progress.usage.output_tokens + 1,
                cache_read_input_tokens: completed_progress.usage.cache_read_input_tokens,
                cache_creation_input_tokens: completed_progress.usage.cache_creation_input_tokens,
            }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let exhausted_chat = make_chat(&router, &bearer).await;
    let exhausted_progress = test_client_checkpoint_progress(2);
    let (_, exhausted_call) =
        park_client_wait_for_route_test(&*store, exhausted_chat.id, exhausted_progress).await;
    resolve_parked_client_call(&*store, exhausted_chat.id, &exhausted_call).await;
    state.turn_job_wake.notify_one();
    let exhausted_events = wait_for_turn(&store, exhausted_chat.id).await;
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_checkpoints_a_client_tool_and_resumes_after_its_result() {
    struct ClientThenFinishProvider {
        requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for ClientThenFinishProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("client-then-finish")
        }

        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            let events = if requests.len() == 1 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "native_1".into(),
                        name: "connect_folder".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"hint":"Documents"}"#.into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "folder connected".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            drop(requests);
            Ok(stream::iter(events).boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.fail_after_next_park_commit();
    let store: Arc<dyn Store> = injected;
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let mut tools = ToolRegistry::new();
    tools.register_client(ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    });
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ClientThenFinishProvider {
            requests: requests.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "connect documents").await,
        StatusCode::ACCEPTED
    );

    let pending = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pending = store.list_pending_client_tool_calls(chat.id).await.unwrap();
            if let Some(call) = pending.into_iter().next() {
                break call;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker should durably checkpoint the client tool");
    let parked = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(parked.status, TurnRunStatus::WaitingForClient);
    assert_eq!(parked.model_steps, 1);
    assert_eq!(parked.usage.input_tokens, 5);
    assert_eq!(parked.usage.output_tokens, 2);
    assert_eq!(pending.name, "connect_folder");
    assert_eq!(pending.execution, ToolCallExecution::Client);
    assert_eq!(pending.arguments, serde_json::json!({"hint": "Documents"}));

    resolve_parked_client_call(
        &*store,
        chat.id,
        &ClientToolCallRequest {
            id: pending.id,
            chat_id: pending.chat_id,
            turn_id: pending.turn_id,
            provider_id: pending.provider_id.clone(),
            name: pending.name.clone(),
            arguments: pending.arguments.clone(),
        },
    )
    .await;
    state.turn_job_wake.notify_one();
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage, .. })
            if usage.input_tokens == 8 && usage.output_tokens == 6
    ));
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    openwave_core::ContentBlock::ToolUse { id, name, .. }
                        if id == "native_1" && name == "connect_folder"
                )
            })
        }));
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    openwave_core::ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "native_1" && content == "connected-root"
                )
            })
        }));
    }

    let exhausted_dir = tempfile::tempdir().unwrap();
    let exhausted_store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            exhausted_dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (exhausted_retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let mut exhausted_tools = ToolRegistry::new();
    exhausted_tools.register_client(ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    });
    let exhausted_state = AppState::new(
        Config::desktop(exhausted_dir.path()),
        exhausted_store.clone(),
        Arc::new(FixedResolver(Arc::new(ClientThenFinishProvider {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(exhausted_tools),
        exhausted_retrieval,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let exhausted_token = exhausted_state.token.clone();
    spawn_turn_worker(&exhausted_state);
    let exhausted_router = app(exhausted_state);
    let exhausted_bearer = format!("Bearer {exhausted_token}");
    let exhausted_chat = make_chat(&exhausted_router, &exhausted_bearer).await;
    let exhausted_turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(
            &exhausted_router,
            &exhausted_bearer,
            exhausted_chat.id,
            exhausted_turn_id,
            "connect documents",
        )
        .await,
        StatusCode::ACCEPTED
    );
    let exhausted_events = wait_for_turn(&exhausted_store, exhausted_chat.id).await;
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    let exhausted_turn = exhausted_store
        .get_turn_run(exhausted_turn_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exhausted_turn.model_steps, 1);
    assert_eq!(exhausted_turn.usage.input_tokens, 5);
    assert_eq!(exhausted_turn.usage.output_tokens, 2);
    assert!(exhausted_store
        .list_pending_client_tool_calls(exhausted_chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn client_execution_api_reconciles_a_known_result_after_exact_lease_expiry() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_expired".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::milliseconds(2),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    tokio::time::sleep(Duration::from_millis(5)).await;

    let resolved = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {
                "status": "cancelled",
                "result": "folder picker was cancelled",
            },
        }),
    )
    .await;
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved: serde_json::Value = json_body(resolved).await;
    assert_eq!(resolved["disposition"], "resolved");
}

#[tokio::test]
async fn client_execution_api_validates_scope_identity_and_terminal_payloads() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let missing_chat = ChatId::new();
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{missing_chat}/client-executions/pending"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_validation".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let claim_uri = format!("/chats/{}/client-executions/{}/claim", chat.id, call.id);
    let nil = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": uuid::Uuid::nil(),
            "lease_token": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    assert_eq!(nil.status(), StatusCode::BAD_REQUEST);

    let lease_token = uuid::Uuid::new_v4();
    let claimed = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": uuid::Uuid::new_v4(),
            "lease_token": lease_token,
        }),
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let oversized = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {
                "status": "failed",
                "result": "failure",
                "error_code": "x".repeat(ToolCallRecord::MAX_ERROR_CODE_LEN + 1),
            },
        }),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
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

#[tokio::test]
async fn raw_ingest_retains_exact_bytes_and_runs_the_async_pipeline() {
    let (router, token, store, dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let raw = b"raw \xff source\n".to_vec();
    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fraw.txt",
        Some("text/plain; charset=utf-8"),
        raw.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        document_id,
        openwave_core::DocumentId::derive("file:///raw.txt")
    );

    let pending = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(pending.media_type, "text/plain; charset=utf-8");
    let source_blob = pending.source_blob.unwrap();
    let blobs = openwave_core::FsBlobStore::new(dir.path().join("blobs"));
    assert_eq!(blobs.get(source_blob.id).await.unwrap().unwrap(), raw);

    run_parse_and_index(&worker).await;
    let ready = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(ready.canonical_text, String::from_utf8_lossy(&raw));
    assert_eq!(
        ready.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
}

#[tokio::test]
async fn raw_ingest_enforces_media_type_body_and_project_scope() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let missing_type = post_raw(&router, &bearer, "/documents/raw", None, b"body".to_vec()).await;
    assert_eq!(missing_type.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(missing_type).await;
    assert_eq!(error.kind, "bad_request");

    let empty = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        Vec::new(),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let unsupported = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("application/octet-stream"),
        b"binary".to_vec(),
    )
    .await;
    assert_eq!(unsupported.status(), StatusCode::BAD_REQUEST);

    let project = make_project(&router, &bearer).await;
    let response = post_raw(
        &router,
        &bearer,
        &format!("/projects/{}/documents/raw", project.id),
        Some("text/markdown"),
        b"# scoped".to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id = accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        store
            .get_document(document_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project.id)
    );
}

#[tokio::test]
async fn raw_ingest_persists_a_safe_title_without_requiring_a_source_path() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?title=meeting%20notes.md",
        Some("text/markdown"),
        b"# Notes".to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id = accepted["document_id"].as_str().unwrap().parse().unwrap();
    let document = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(document.title.as_deref(), Some("meeting notes.md"));
    assert_eq!(document.source_uri, None);

    let spoofed = post_raw(
        &router,
        &bearer,
        "/documents/raw?title=report%E2%80%AEtxt.md",
        Some("text/markdown"),
        b"# Notes".to_vec(),
    )
    .await;
    assert_eq!(spoofed.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn headless_embedding_keeps_document_api_on_its_primary_bearer() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .oneshot(
            Request::builder()
                .uri("/documents")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn raw_ingest_has_an_explicit_limit_and_preserves_payload_too_large() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let boundary = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        vec![b'x'; MAX_RAW_DOCUMENT_BYTES],
    )
    .await;
    assert_eq!(boundary.status(), StatusCode::ACCEPTED);

    let too_large = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        vec![b'x'; MAX_RAW_DOCUMENT_BYTES + 1],
    )
    .await;
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: AgentErrorInfo = json_body(too_large).await;
    assert_eq!(error.kind, "payload_too_large");
}

#[tokio::test]
async fn ingest_then_search_finds_the_passage() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");

    let ingest = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({
            "uri": "file:///solar.txt",
            "content": "Jupiter is the largest planet in the Solar System, a gas giant.",
        }),
    )
    .await;
    assert_eq!(ingest.status(), StatusCode::ACCEPTED);
    let ingest: serde_json::Value = json_body(ingest).await;
    assert!(ingest["document_id"].is_string());
    assert!(ingest["job_id"].is_string());
    assert_eq!(ingest["processing_status"], "queued");
    let document_id = ingest["document_id"].as_str().unwrap().parse().unwrap();
    let record = store
        .get_document(document_id)
        .await
        .unwrap()
        .expect("source record should be durable before the response");
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 1);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );

    run_parse_and_index(&worker).await;

    // The worker's activated generation is searchable over the shared index.
    let search = post_json(
        &router,
        &bearer,
        "/search",
        serde_json::json!({ "query": "largest gas giant planet", "k": 1 }),
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let results: serde_json::Value = json_body(search).await;
    let citations = results["citations"].as_array().unwrap();
    assert_eq!(citations.len(), 1);
    assert!(citations[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("Jupiter"));
    assert_eq!(citations[0]["document_id"], ingest["document_id"]);
    let response_json = serde_json::to_string(&results).unwrap();
    assert!(!response_json.contains("file:///solar.txt"));
    assert!(!response_json.contains(&record.revision_token.to_string()));
    assert!(!response_json.contains("revision_token"));
    assert!(!response_json.contains("content_revision"));
    assert!(citations[0].get("source").is_none());
    assert!(citations[0].get("generation").is_none());
}

#[tokio::test]
async fn maximum_search_output_and_private_evidence_commit_together() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let embedder = Arc::new(HashEmbedder::default());
    let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    for ordinal in 0..openwave_retrieval::MAX_SEARCH_RESULTS {
        let document_id = openwave_core::DocumentId::new();
        let generation = openwave_core::DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
        };
        let mut snippet = format!("fact {ordinal} ");
        snippet.push_str(&"x".repeat(1_024 - snippet.len()));
        let embedding = embedder.embed_query(&snippet).await.unwrap();
        vectors
            .stage_document_generation(
                document_id,
                generation,
                vec![VectorRecord {
                    project_id: None,
                    source: openwave_retrieval::DocumentSource::Inline,
                    generation: Some(generation),
                    chunk: openwave_retrieval::Chunk::new(
                        document_id,
                        0,
                        openwave_core::ByteSpan::new(0, snippet.len()),
                        snippet,
                    ),
                    embedding,
                }],
            )
            .await
            .unwrap();
        assert!(vectors
            .activate_document_generation(document_id, generation)
            .await
            .unwrap());
    }
    let tool = openwave_retrieval::SearchTool::new(embedder, vectors);
    let output = tool
        .execute(
            &ToolCtx::new_legacy_workspace(chat.id, None, dir.path().to_path_buf()),
            serde_json::json!({"query": "fact", "k": 9999}),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(
        output.private_evidence.len(),
        openwave_retrieval::MAX_SEARCH_RESULTS
    );
    assert!(output.content.len() <= ToolCallRecord::MAX_RESULT_BYTES);

    let created_at = chrono::Utc::now();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "max_search".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "fact", "k": 9999}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &ToolCallResolution::Completed {
                    result: output.content.clone(),
                },
                created_at + chrono::Duration::seconds(1),
                &output.private_evidence,
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store.list_retrieval_evidence(call.id).await.unwrap().len(),
        output.private_evidence.len()
    );
}

#[tokio::test]
async fn project_document_routes_enforce_corpus_identity_and_ownership() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let project_a = make_project(&router, &bearer).await;
    let project_b = make_project(&router, &bearer).await;
    let uri = "file:///shared-source.txt";

    let root: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "loose corpus zephyr"}),
        )
        .await,
    )
    .await;
    let a: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_a.id),
            serde_json::json!({"uri": uri, "content": "project alpha aurora"}),
        )
        .await,
    )
    .await;
    let b: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_b.id),
            serde_json::json!({"uri": uri, "content": "project beta nebula"}),
        )
        .await,
    )
    .await;

    assert_eq!(
        root["document_id"],
        openwave_core::DocumentId::derive(uri).to_string()
    );
    assert_eq!(
        a["document_id"],
        openwave_core::DocumentId::derive_for_project(project_a.id, uri).to_string()
    );
    assert_eq!(
        b["document_id"],
        openwave_core::DocumentId::derive_for_project(project_b.id, uri).to_string()
    );
    assert_ne!(root["document_id"], a["document_id"]);
    assert_ne!(a["document_id"], b["document_id"]);

    for _ in 0..6 {
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
    }

    let request = |method: axum::http::Method, uri: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let a_id = a["document_id"].as_str().unwrap();
    let b_id = b["document_id"].as_str().unwrap();
    let listing: serde_json::Value = json_body(
        request(
            axum::http::Method::GET,
            format!("/projects/{}/documents", project_a.id),
        )
        .await,
    )
    .await;
    assert_eq!(listing["documents"].as_array().unwrap().len(), 1);
    assert_eq!(listing["documents"][0]["document_id"], a["document_id"]);
    assert_eq!(
        listing["documents"][0]["project_id"],
        project_a.id.to_string()
    );

    assert_eq!(
        request(
            axum::http::Method::GET,
            format!("/projects/{}/documents/{b_id}", project_a.id),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            axum::http::Method::DELETE,
            format!("/projects/{}/documents/{a_id}", project_b.id),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(axum::http::Method::DELETE, format!("/documents/{a_id}"),)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(axum::http::Method::GET, format!("/documents/{a_id}"),)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/documents/{a_id}/retry"),
            serde_json::Value::Null,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store
            .get_document(a_id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project_a.id)
    );

    let root_search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "loose corpus zephyr", "k": 1}),
        )
        .await,
    )
    .await;
    assert_eq!(
        root_search["citations"][0]["document_id"],
        root["document_id"]
    );
    let a_search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/search", project_a.id),
            serde_json::json!({"query": "project beta nebula", "k": 1}),
        )
        .await,
    )
    .await;
    assert_eq!(a_search["citations"][0]["document_id"], a["document_id"]);

    let unknown = ProjectId::new();
    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{unknown}/documents"),
            serde_json::json!({"uri": uri, "content": "orphan"}),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            axum::http::Method::DELETE,
            format!("/projects/{}/documents/{a_id}", project_a.id),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn failed_indexing_leaves_authoritative_source_stale_for_retry() {
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(FailingEmbedder),
        Arc::new(InMemoryVectorStore::new(8)),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");

    let response = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({
            "uri": "file:///retry.txt",
            "content": "authoritative even when embedding fails",
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(_)
    ));
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::RetryScheduled(_)
    ));

    let record = store
        .get_document(openwave_core::DocumentId::derive("file:///retry.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.canonical_text,
        "authoritative even when embedding fails"
    );
    assert_eq!(record.content_revision, 1);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );
}

#[tokio::test]
async fn explicit_retry_revives_the_exact_terminal_job() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///manual-retry.txt",
                "content": "retry the exact failed generation"
            }),
        )
        .await,
    )
    .await;
    let id: openwave_core::DocumentId = ingested["document_id"].as_str().unwrap().parse().unwrap();
    let job_id: openwave_core::DocumentJobId =
        ingested["job_id"].as_str().unwrap().parse().unwrap();
    let now = chrono::Utc::now();
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, job_id);
    assert_eq!(
        store
            .record_document_job_failure(
                job_id,
                claimed.lease_token.unwrap(),
                chrono::Utc::now(),
                None,
                "manual_test_failure",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/documents/{id}/retry"))
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried = store.get_document_job(job_id).await.unwrap().unwrap();
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.id, job_id);
}

#[tokio::test]
async fn explicit_retry_selects_the_failed_parse_stage() {
    let (router, token, store, dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let raw = b"parse retry succeeds through the durable worker".to_vec();
    let source_blob = openwave_core::DocumentSourceBlob::from_bytes(&raw);
    let blob_id = source_blob.id;
    let blobs = openwave_core::FsBlobStore::new(dir.path().join("blobs"));
    openwave_core::BlobStore::put(&blobs, blob_id, raw.clone())
        .await
        .unwrap();
    let source = openwave_core::DocumentSourceUpsert {
        id: openwave_core::DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///parse-retry.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        source_blob,
        updated_at: chrono::Utc::now(),
    };
    let (_, parse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "plain-text-lossy-v1", 1)
        .await
        .unwrap();
    let claim_at = parse_job.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, parse_job.id);
    assert_eq!(
        store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "parse_failed",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/documents/{}/retry", source.id))
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried = store.get_document_job(parse_job.id).await.unwrap().unwrap();
    assert_eq!(retried.id, parse_job.id);
    assert_eq!(retried.kind, openwave_core::DocumentJobKind::Parse);
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, document_stage::MAX_PARSE_ATTEMPTS);

    assert_eq!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(parse_job.id)
    );
    let jobs = store.list_document_jobs(source.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Succeeded);
    assert_eq!(jobs[1].kind, openwave_core::DocumentJobKind::Index);
    assert_eq!(jobs[1].status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(jobs[1].id)
    );
    let document = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(document.canonical_text.as_bytes(), raw);
    assert_eq!(
        document.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
}

#[tokio::test]
async fn project_retry_revives_only_the_owned_terminal_job() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project_a = make_project(&router, &bearer).await;
    let project_b = make_project(&router, &bearer).await;
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_a.id),
            serde_json::json!({
                "uri": "file:///project-manual-retry.txt",
                "content": "retry only within the owning project"
            }),
        )
        .await,
    )
    .await;
    let document_id: openwave_core::DocumentId =
        ingested["document_id"].as_str().unwrap().parse().unwrap();
    let job_id: openwave_core::DocumentJobId =
        ingested["job_id"].as_str().unwrap().parse().unwrap();
    let now = chrono::Utc::now();
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, job_id);
    assert_eq!(
        store
            .record_document_job_failure(
                job_id,
                claimed.lease_token.unwrap(),
                chrono::Utc::now(),
                None,
                "project_manual_test_failure",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/documents/{document_id}/retry"),
            serde_json::Value::Null,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents/{document_id}/retry", project_b.id),
            serde_json::Value::Null,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store
            .get_document_job(job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        openwave_core::DocumentJobStatus::Failed
    );

    let response = post_json(
        &router,
        &bearer,
        &format!("/projects/{}/documents/{document_id}/retry", project_a.id),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response: serde_json::Value = json_body(response).await;
    assert_eq!(response["document_id"], document_id.to_string());
    assert_eq!(response["job_id"], job_id.to_string());
    let retried = store.get_document_job(job_id).await.unwrap().unwrap();
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.id, job_id);
}

#[tokio::test]
async fn failed_update_keeps_the_prior_active_generation_searchable() {
    let embedder = Arc::new(FailAfterFirstBatchEmbedder {
        inner: HashEmbedder::default(),
        calls: AtomicUsize::new(0),
    });
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        embedder,
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///updated.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "obsolete searchable phrase"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    run_parse_and_index(&worker).await;
    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "replacement failed to embed"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(_)
    ));
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::RetryScheduled(_)
    ));

    let search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "obsolete searchable phrase"}),
        )
        .await,
    )
    .await;
    assert_eq!(search["citations"].as_array().unwrap().len(), 1);
    assert!(search["citations"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("obsolete"));
    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.canonical_text, "replacement failed to embed");
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
}

#[tokio::test]
async fn update_enqueues_without_calling_legacy_vector_retirement() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store.clone(),
    ));
    let (router, token, store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///retirement.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "still authoritative"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    vector_store.fail_next_delete();
    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "must not publish"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );

    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(record.indexed_at, None);
}

#[tokio::test]
async fn first_ingest_persists_source_without_attempting_vector_retirement() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    vector_store.fail_next_delete();
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store,
    ));
    let (router, token, store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///first-source.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "source comes first"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.indexed_revision, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );
}

#[tokio::test]
async fn document_catalog_pages_metadata_and_keeps_project_content_private() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///catalog.txt",
                "media_type": "text/markdown",
                "content": "# Catalog\n\nDurable source",
            }),
        )
        .await,
    )
    .await;
    let id = ingested["document_id"].as_str().unwrap().to_owned();

    for suffix in ["second", "third"] {
        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({
                    "uri": format!("file:///{suffix}.txt"),
                    "content": format!("{suffix} document"),
                }),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
    }

    let project = make_project(&router, &bearer).await;
    let project_document_id = openwave_core::DocumentId::new();
    let now = chrono::Utc::now();
    store
        .create_document(&openwave_core::DocumentRecord {
            id: project_document_id,
            project_id: Some(project.id),
            source_uri: Some("file:///project-secret.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            source_blob: None,
            canonical_text: "project-only source".into(),
            canonical_fingerprint: None,
            source_regions: Vec::new(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: openwave_core::DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at: now,
            updated_at: now,
            indexed_at: None,
        })
        .await
        .unwrap();

    let get = |uri: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let first = get("/documents?limit=2".into()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = json_body(first).await;
    let first_documents = first["documents"].as_array().unwrap();
    assert_eq!(first_documents.len(), 2);
    let cursor = first["next_cursor"].as_str().expect("a second page");
    assert!(first_documents.iter().all(|summary| {
        summary.get("content").is_none() && summary.get("revision_token").is_none()
    }));

    let second = get(format!("/documents?limit=2&cursor={cursor}")).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = json_body(second).await;
    let second_documents = second["documents"].as_array().unwrap();
    assert_eq!(second_documents.len(), 1);
    assert!(second["next_cursor"].is_null());

    let listed_ids: std::collections::HashSet<_> = first_documents
        .iter()
        .chain(second_documents)
        .map(|summary| summary["document_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed_ids.len(), 3);
    assert!(listed_ids.contains(id.as_str()));
    assert!(!listed_ids.contains(project_document_id.to_string().as_str()));

    let catalog_summary = first_documents
        .iter()
        .chain(second_documents)
        .find(|summary| summary["document_id"] == id)
        .unwrap();
    assert_eq!(catalog_summary["uri"], "file:///catalog.txt");
    assert_eq!(catalog_summary["media_type"], "text/markdown");
    assert_eq!(catalog_summary["content_revision"], 1);
    assert_eq!(catalog_summary["processing_status"], "queued");
    assert!(catalog_summary["indexed_revision"].is_null());

    let detail = get(format!("/documents/{id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: serde_json::Value = json_body(detail).await;
    assert_eq!(detail["content"], "");
    assert_eq!(detail["document_id"], id);
    assert!(detail.get("revision_token").is_none());

    assert_eq!(
        get(format!("/documents/{project_document_id}"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        get("/documents?limit=0".into()).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get("/documents?cursor=garbage".into()).await.status(),
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        get(format!("/documents/{}", openwave_core::DocumentId::new()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn document_catalog_cursor_preserves_nanosecond_ordering() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let mut expected = Vec::new();

    for nanos in [900, 800, 700] {
        let id = openwave_core::DocumentId::new();
        let created_at = chrono::DateTime::from_timestamp(1_700_000_000, nanos).unwrap();
        store
            .create_document(&openwave_core::DocumentRecord {
                id,
                project_id: None,
                source_uri: Some(format!("file:///{nanos}.txt")),
                media_type: "text/plain".into(),
                title: None,
                source_blob: None,
                canonical_text: nanos.to_string(),
                canonical_fingerprint: None,
                source_regions: Vec::new(),
                content_revision: 1,
                revision_token: uuid::Uuid::new_v4(),
                processing_status: openwave_core::DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at,
                updated_at: created_at,
                indexed_at: None,
            })
            .await
            .unwrap();
        expected.push(id.to_string());
    }

    let mut uri = "/documents?limit=1".to_owned();
    let mut actual = Vec::new();
    loop {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: serde_json::Value = json_body(response).await;
        let documents = page["documents"].as_array().unwrap();
        assert_eq!(documents.len(), 1);
        actual.push(documents[0]["document_id"].as_str().unwrap().to_owned());
        let Some(cursor) = page["next_cursor"].as_str() else {
            break;
        };
        uri = format!("/documents?limit=1&cursor={cursor}");
    }

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn concurrent_same_document_ingests_publish_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("concurrent-ingest.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let blobs = Arc::new(FirstPutGatedBlobStore {
        inner: openwave_core::FsBlobStore::new(dir.path().join("blobs")),
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.blobs = blobs.clone();
    let token = state.token.clone();
    let worker = document_worker::DocumentWorker::new(
        store.clone(),
        state.blobs.clone(),
        retrieval,
        state.document_job_wake.clone(),
        state.document_writes.clone(),
        document_worker::DocumentWorkerConfig::default(),
    );
    let router = app(state);
    let bearer = format!("Bearer {token}");

    let first_router = router.clone();
    let first_bearer = bearer.clone();
    let first = tokio::spawn(async move {
        post_json(
            &first_router,
            &first_bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "first version",
            }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), blobs.entered.notified())
        .await
        .expect("first request did not reach blob publication");

    let second_router = router.clone();
    let second_bearer = bearer.clone();
    let mut second = tokio::spawn(async move {
        post_json(
            &second_router,
            &second_bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "second version",
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "later request must not complete while the first publication is blocked"
    );
    assert_eq!(
        blobs.calls.load(Ordering::SeqCst),
        1,
        "later request must block before publishing its blob"
    );
    blobs.release.notify_one();
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    assert_eq!(blobs.calls.load(Ordering::SeqCst), 2);
    let record = store
        .get_document(openwave_core::DocumentId::derive("file:///concurrent.txt"))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
    let jobs = store.list_document_jobs(record.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Cancelled);
    assert_eq!(jobs[1].status, openwave_core::DocumentJobStatus::Queued);

    run_parse_and_index(&worker).await;
    let record = store.get_document(record.id).await.unwrap().unwrap();
    assert_eq!(record.canonical_text, "second version");
    assert_eq!(record.indexed_revision, Some(2));
}

#[tokio::test]
async fn deleting_a_document_removes_it_from_the_index() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "file:///doc.txt", "content": "Jupiter is a gas giant." }),
        )
        .await,
    )
    .await;
    let id = ingest["document_id"].as_str().unwrap().to_string();
    run_parse_and_index(&worker).await;

    let delete = |id: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/documents/{id}"))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Retired(_)
    ));
    // Gone from the index.
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "gas giant" }),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
    // Idempotent: deleting again is still accepted.
    assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
    assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
}

#[tokio::test]
async fn durable_worker_retries_a_failed_tombstone_publication() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store.clone(),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///retry-delete.txt",
                "content": "retire this searchable source"
            }),
        )
        .await,
    )
    .await;
    run_parse_and_index(&worker).await;
    let id = ingest["document_id"].as_str().unwrap();
    vector_store.fail_next_delete();

    let delete = |id: &str| {
        let router = router.clone();
        let bearer = bearer.clone();
        let uri = format!("/documents/{id}");
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(delete(id).await, StatusCode::ACCEPTED);
    assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
    assert!(worker.run_once().await.is_err());
    let visible: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "searchable source"}),
        )
        .await,
    )
    .await;
    assert_eq!(visible["citations"].as_array().unwrap().len(), 1);

    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Retired(_)
    ));
    let cleared: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "searchable source"}),
        )
        .await,
    )
    .await;
    assert!(cleared["citations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn re_ingesting_the_same_uri_is_idempotent() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let doc = serde_json::json!({
        "uri": "file:///notes.txt",
        "content": "one two three four five six seven eight nine ten",
    });

    let first: serde_json::Value =
        json_body(post_json(&router, &bearer, "/documents", doc.clone()).await).await;
    let second: serde_json::Value =
        json_body(post_json(&router, &bearer, "/documents", doc).await).await;
    // Same URI => same derived document id => replaced in place.
    assert_eq!(first["document_id"], second["document_id"]);
    assert_eq!(first["job_id"], second["job_id"]);
    assert_eq!(first["content_revision"], second["content_revision"]);
    let document_id = first["document_id"].as_str().unwrap().parse().unwrap();
    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, openwave_core::DocumentJobKind::Parse);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Queued);
    run_parse_and_index(&worker).await;

    // A broad search still returns each chunk once, not doubled.
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "three four five", "k": 50 }),
        )
        .await,
    )
    .await;
    let citations = results["citations"].as_array().unwrap();
    let ids: std::collections::HashSet<_> = citations
        .iter()
        .map(|c| c["chunk_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), citations.len());
}

#[tokio::test]
async fn a_padded_uri_targets_the_same_document() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Surrounding whitespace must not change the derived document id, or
    // "re-ingest the same file" would silently create a second document.
    let padded: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "  file:///a.txt  ", "content": "hello world" }),
        )
        .await,
    )
    .await;
    let clean: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "file:///a.txt", "content": "hello world" }),
        )
        .await,
    )
    .await;
    assert_eq!(padded["document_id"], clean["document_id"]);
}

#[tokio::test]
async fn ingest_rejects_empty_content_and_search_rejects_empty_query() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let bad_ingest = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({ "content": "   " }),
    )
    .await;
    assert_eq!(bad_ingest.status(), StatusCode::BAD_REQUEST);

    let bad_search = post_json(
        &router,
        &bearer,
        "/search",
        serde_json::json!({ "query": "  " }),
    )
    .await;
    assert_eq!(bad_search.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_rejects_unsupported_media_type() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({ "content": "%PDF-1.7", "media_type": "application/pdf" }),
    )
    .await;
    // A parser that can't handle the media type is the caller's problem: 400.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn search_on_an_empty_index_returns_no_citations() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "anything" }),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn root_search_never_returns_project_owned_vectors() {
    let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    vectors
        .upsert(vec![VectorRecord {
            project_id: Some(ProjectId::new()),
            source: openwave_retrieval::DocumentSource::Inline,
            generation: None,
            chunk: openwave_retrieval::Chunk::new(
                openwave_core::DocumentId::new(),
                0,
                openwave_retrieval::ByteSpan::new(0, 14),
                "project secret",
            ),
            embedding: Embedding(vec![0.0; HashEmbedder::DEFAULT_DIMS]),
        }])
        .await
        .unwrap();
    let (retrieval, _search) = build_retrieval(Arc::new(HashEmbedder::default()), vectors);
    let (router, token, _store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &format!("Bearer {token}"),
            "/search",
            serde_json::json!({"query": "project secret"}),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
}

#[test]
fn agent_deps_registers_server_tools_and_the_foreground_sandbox_contract() {
    let (_retrieval, tools, _config) = agent_deps(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let names: Vec<String> = tools.specs().into_iter().map(|s| s.name).collect();
    assert!(
        names.iter().any(|n| n == "search"),
        "search tool registered"
    );
    assert!(
        names.iter().any(|n| n == "read_file"),
        "file tools still present"
    );
    assert!(
        !names
            .iter()
            .any(|name| name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL),
        "the sandbox contract must only be advertised to a claimed foreground turn"
    );
    assert!(
        tools
            .specs_for_foreground(true)
            .iter()
            .any(|spec| spec.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL),
        "foreground turns must expose the durable sandbox delegation contract"
    );
    assert_eq!(
        tools.execution(openwave_core::REQUEST_FOLDER_ACCESS_TOOL),
        Some(ToolCallExecution::Client)
    );
    assert!(tools
        .get(openwave_core::REQUEST_FOLDER_ACCESS_TOOL)
        .is_none());
    let spec = tools
        .specs()
        .into_iter()
        .find(|spec| spec.name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL)
        .expect("folder consent tool is advertised");
    assert_eq!(spec, openwave_core::request_folder_access_tool_spec());
    assert!(tools.client_arguments_are_valid(
        openwave_core::REQUEST_FOLDER_ACCESS_TOOL,
        &serde_json::json!({
            "reason": "Read the reports needed for this project",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents"
        })
    ));
    assert!(!tools.client_arguments_are_valid(
        openwave_core::REQUEST_FOLDER_ACCESS_TOOL,
        &serde_json::json!({
            "reason": "Read reports",
            "requested_capabilities": ["write_files"],
            "path": "/Users/example/Documents"
        })
    ));
}

#[tokio::test]
async fn catalog_delete_failure_leaves_source_stale_and_repairable() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("delete-failure.db").display()
        ))
        .await
        .unwrap(),
    );
    let store = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    ));
    let (router, token, store_view, _dir) =
        test_app_from_parts(Arc::new(FakeProvider), retrieval, store.clone(), dir);
    let bearer = format!("Bearer {token}");
    let uri = "file:///delete-failure.txt";
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "rebuildable source"}),
        )
        .await,
    )
    .await;
    let id = ingested["document_id"].as_str().unwrap().to_string();

    store.fail_next_document_delete();
    let delete = |id: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/documents/{id}"))
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(
        delete(id.clone()).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let record = store_view
        .get_document(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(record.indexed_at, None);

    let search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "rebuildable source"}),
        )
        .await,
    )
    .await;
    assert!(search["citations"].as_array().unwrap().is_empty());
    assert_eq!(delete(id).await.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn resolve_embedder_uses_openai_only_when_enabled_and_keyed() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets = MemSecrets::default();
    providers::write_credential(
        &secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-openai-test"),
    )
    .await
    .unwrap();

    // Enabled + keyed → the real 1536-dim embedder. A stored credential takes
    // precedence over any env var, so this is deterministic; construction only,
    // no network call.
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
        },
    )
    .await
    .unwrap();
    let online = resolve_embedder(&*store, &secrets).await;
    assert_eq!(online.dimensions(), EMBED_DIMS);
    assert_ne!(EMBED_DIMS, HashEmbedder::default().dimensions());

    // Disabled but keyed → the key is ignored (no silent egress), even though
    // it's present. Deterministic regardless of any ambient OPENAI_API_KEY,
    // since a disabled provider never consults the key at all.
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: false,
            base_url: None,
        },
    )
    .await
    .unwrap();
    let offline = resolve_embedder(&*store, &secrets).await;
    assert_eq!(offline.dimensions(), HashEmbedder::default().dimensions());
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_vector_store_opens_a_durable_lance_index_under_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());

    // Ingest into the store, then reopen from the same data_dir and confirm the
    // chunk survived — i.e. bind()'s production path really persists to disk.
    {
        let store = connect_vector_store(&config, 2).await.unwrap();
        let doc = openwave_retrieval::DocumentId::new();
        let chunk =
            openwave_retrieval::Chunk::new(doc, 0, openwave_retrieval::ByteSpan::new(0, 4), "note");
        store
            .upsert(vec![openwave_retrieval::VectorRecord {
                project_id: None,
                source: openwave_retrieval::DocumentSource::Inline,
                generation: None,
                chunk,
                embedding: openwave_retrieval::Embedding(vec![1.0, 0.0]),
            }])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }
    assert!(
        dir.path().join("vectors").exists(),
        "lance dir created under data_dir"
    );
    let reopened = connect_vector_store(&config, 2).await.unwrap();
    assert_eq!(reopened.len().await.unwrap(), 1);
}

#[tokio::test]
async fn health_needs_no_token() {
    let (router, _token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_rejects_missing_and_wrong_tokens() {
    let (router, _token, _store, _dir) = test_app().await;
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/chats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let wrong = router
        .oneshot(
            Request::builder()
                .uri("/chats")
                .header(header::AUTHORIZATION, "Bearer not-the-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn retrieval_routes_require_a_token() {
    let (router, _token, _store, _dir) = test_app().await;
    // Both retrieval routes sit behind the bearer-token layer, not out in the
    // open like /healthz — a request with no token is rejected before it runs.
    for uri in ["/documents", "/search"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} must require a token"
        );
    }
}

#[tokio::test]
async fn create_then_get_and_list() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let created: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"title": "hi"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    };
    assert_eq!(created.title.as_deref(), Some("hi"));
    assert_eq!(created.attachment_revision, 0);
    assert!(created.root_attachments.is_empty());
    assert!(serde_json::to_value(&created)
        .unwrap()
        .get("workspace_dir")
        .is_none());

    let fetched: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", created.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(fetched, created);

    let agent_runs: Vec<serde_json::Value> = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}/agent-runs", created.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(agent_runs.len(), 1);
    assert_eq!(
        agent_runs[0].get("id"),
        Some(&serde_json::Value::String(
            openwave_core::AgentRunId::foreground_for_chat(created.id).to_string()
        ))
    );
    let snapshot = agent_runs[0].as_object().unwrap();
    assert!(snapshot.get("lease_token").is_none());
    assert!(snapshot.get("lease_expires_at").is_none());
    assert!(snapshot.get("input").is_none());
    assert!(snapshot.get("chat_id").is_none());

    let listed: Vec<Chat> = {
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(listed, vec![created]);
}

#[tokio::test]
async fn agent_run_snapshots_expose_only_safe_live_sandbox_activity() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    };

    let run = match store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("research"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected sandbox admission: {outcome:?}"),
    };
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let checkpoint = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "provider-call-identity".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({
            "query": "private query that must not reach the renderer",
            "api_key": "secret-value"
        }),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(run.id, worker_lease, &checkpoint)
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("activity"),
        Some(&serde_json::json!({"kind": "web_search", "status": "waiting"}))
    );

    // The projection is intentionally independent of the durable checkpoint's
    // sensitive executor data.
    let encoded = serde_json::to_string(snapshot).unwrap();
    for forbidden in [
        "private query that must not reach the renderer",
        "secret-value",
        "provider-call-identity",
        "arguments",
        "lease_token",
        "result",
    ] {
        assert!(!encoded.contains(forbidden), "snapshot leaked {forbidden}");
    }

    let executor_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(checkpoint.id, executor_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        openwave_core::ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("activity"),
        Some(&serde_json::json!({"kind": "web_search", "status": "running"}))
    );
}

#[tokio::test]
async fn agent_run_snapshots_expose_only_safe_live_foreground_folder_activity() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let historical_argument = "historical-private-argument".repeat(4_000);
    let historical_result = "historical-private-result".repeat(16_000);
    let historical = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "historical-provider-call-identity".into(),
        name: "read_connected_file".into(),
        arguments: serde_json::json!({"path": historical_argument}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let historical = match store.accept_tool_call(&historical).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call) => call,
        outcome => panic!("unexpected historical-call admission: {outcome:?}"),
    };
    let historical_lease = uuid::Uuid::new_v4();
    let historical_now = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                historical.id,
                chat.id,
                uuid::Uuid::new_v4(),
                historical_lease,
                historical_now,
                historical_now + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    assert!(matches!(
        store
            .resolve_client_tool_call_and_append_event(
                historical.id,
                chat.id,
                historical_lease,
                chrono::Utc::now(),
                &ToolCallResolution::Completed {
                    result: historical_result.clone(),
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap()
            .outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    ));

    let root_id = "5b3e9987-5ebf-4bb0-bc6f-0c041b156027";
    let relative_path = "taxes/2026/private-return.txt";
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "provider-call-identity".into(),
        name: "read_connected_file".into(),
        arguments: serde_json::json!({
            "root_id": root_id,
            "path": relative_path,
            "grant": "private-grant"
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let call = match store.accept_tool_call(&call).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call) => call,
        outcome => panic!("unexpected client-call admission: {outcome:?}"),
    };

    let snapshot = |router: Router| async {
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}/agent-runs", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let snapshots: Vec<serde_json::Value> = json_body(response).await;
        snapshots
            .into_iter()
            .find(|snapshot| snapshot.get("execution") == Some(&serde_json::json!("foreground")))
            .expect("foreground snapshot is returned")
    };

    let waiting = snapshot(router.clone()).await;
    assert_eq!(
        waiting.get("activity"),
        Some(&serde_json::json!({
            "kind": "read_connected_file",
            "status": "waiting"
        }))
    );
    let encoded = serde_json::to_string(&waiting).unwrap();
    for forbidden in [
        root_id,
        relative_path,
        "private-grant",
        "provider-call-identity",
        &historical_argument,
        &historical_result,
    ] {
        assert!(!encoded.contains(forbidden), "snapshot leaked {forbidden}");
    }

    let lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                lease,
                now,
                now + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    assert_eq!(
        snapshot(router.clone()).await.get("activity"),
        Some(&serde_json::json!({
            "kind": "read_connected_file",
            "status": "running"
        }))
    );

    assert!(matches!(
        store
            .resolve_client_tool_call_and_append_event(
                call.id,
                chat.id,
                lease,
                chrono::Utc::now(),
                &ToolCallResolution::Completed {
                    result: "private result".into(),
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap()
            .outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    ));
    assert_eq!(
        snapshot(router).await.get("activity"),
        Some(&serde_json::Value::Null)
    );
}

#[tokio::test]
async fn agent_run_snapshots_omit_persisted_raw_failure_detail() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;

    let run = match store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("research"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected sandbox admission: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(lease_token, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let raw_detail = "upstream request failed: Authorization: Bearer private-token";
    assert!(store
        .fail_agent_run(
            run.id,
            lease_token,
            "sandbox_transport_failed",
            raw_detail,
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store
            .get_agent_run(run.id)
            .await
            .unwrap()
            .expect("failed run remains persisted")
            .last_error_detail
            .as_deref(),
        Some(raw_detail)
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("last_error_code"),
        Some(&serde_json::json!("sandbox_transport_failed"))
    );
    assert!(snapshot.get("last_error_detail").is_none());
    assert!(!serde_json::to_string(snapshot)
        .unwrap()
        .contains(raw_detail));
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
async fn project_create_get_and_list() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let created = make_project(&router, &bearer).await;
    assert_eq!(created.title.as_deref(), Some("p"));
    assert_eq!(created.attachment_revision, 0);
    assert!(created.root_attachments.is_empty());
    assert!(serde_json::to_value(&created)
        .unwrap()
        .get("workspace_dir")
        .is_none());

    let fetched: Project = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{}", created.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(fetched, created);

    let listed: Vec<Project> = {
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(listed, vec![created]);
}

#[tokio::test]
async fn chat_can_be_filed_under_a_project() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project = make_project(&router, &bearer).await;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": project.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.project_id, Some(project.id));
}

#[tokio::test]
async fn project_chat_snapshots_ordered_opaque_root_defaults() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let root_b = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let root_a = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let project = Project {
        id: ProjectId::new(),
        title: Some("pathless".into()),
        attachment_revision: 3,
        root_attachments: vec![root_b, root_a],
        created_at: chrono::Utc::now(),
    };
    store.create_project(&project).await.unwrap();

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": project.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.attachment_revision, 1);
    assert_eq!(
        chat.root_attachments,
        vec![
            ChatRootAttachment {
                root_id: root_b,
                origin: RootAttachmentOrigin::ProjectDefault,
            },
            ChatRootAttachment {
                root_id: root_a,
                origin: RootAttachmentOrigin::ProjectDefault,
            },
        ]
    );
}

#[tokio::test]
async fn chat_referencing_an_unknown_project_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": ProjectId::new()}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn models_catalog_is_served() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    let models = catalog["models"].as_array().unwrap();
    assert!(!models.is_empty());
    assert!(models.iter().any(|m| m["provider"] == "anthropic"));
}

#[tokio::test]
async fn chat_created_with_a_model() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "claude-x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.model.as_deref(), Some("claude-x"));
}

#[tokio::test]
async fn chat_created_with_empty_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"model": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

/// PATCH a chat's model with a raw JSON body, returning the response.
async fn patch_chat(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/chats/{chat}"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn patch_chat_sets_and_clears_the_model() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(chat.model, None);

    let set = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": "m1"}),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(set).await.model.as_deref(), Some("m1"));

    let cleared = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(cleared).await.model, None);
}

#[tokio::test]
async fn patch_chat_sets_and_clears_a_trimmed_title() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let renamed = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "  Project notes  "}),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(
        json_body::<Chat>(renamed).await.title.as_deref(),
        Some("Project notes")
    );

    let rejected = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "must not persist", "model": ""}),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let fetched: Chat = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fetched.title.as_deref(), Some("Project notes"));

    let cleared = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(cleared).await.title, None);
}

#[tokio::test]
async fn delete_chat_removes_a_quiesced_conversation_and_reports_safe_conflicts() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let deleted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(store.get_chat(chat.id).await.unwrap().is_none());

    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let active = make_chat(&router, &bearer).await;
    store
        .accept_turn(TurnId::new(), active.id, "fake", "do not remove")
        .await
        .unwrap();
    let blocked = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", active.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(blocked).await;
    assert_eq!(info.kind, "chat_active");
    assert!(store.get_chat(active.id).await.unwrap().is_some());
}

#[tokio::test]
async fn chat_transcript_replays_only_visible_durable_messages() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    store
        .append_message(&Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: "remember this".into(),
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .append_event(
                chat.id,
                &AgentEvent::TextDelta {
                    text: "live".into()
                }
            )
            .await
            .unwrap(),
        1
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert_eq!(transcript["messages"][0]["role"], "user");
    assert_eq!(transcript["messages"][0]["content"], "remember this");
    assert_eq!(
        transcript["last_event_seq"], 0,
        "a nonterminal delta must replay after a durable transcript snapshot"
    );
}

#[tokio::test]
async fn transcript_tool_activity_is_allowlisted_and_redacts_canonical_tool_data() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "finish a turn first").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    let turn = store
        .list_turn_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("accepted turn exists");
    assert_eq!(turn.status, TurnRunStatus::Completed);

    let call_id = CallId::new();
    let started_at = chrono::Utc::now();
    let secret_path = "/Users/alice/Documents/payroll-secret.csv";
    let secret_result = "private tool result: 123-45-6789";
    let secret_provider_id = "provider-secret-call-id";
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: secret_provider_id.into(),
            name: "mcp__private_server__read_sensitive_path".into(),
            arguments: serde_json::json!({"path": secret_path}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at,
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                call_id,
                &ToolCallResolution::Failed {
                    result: secret_result.into(),
                    error_code: "private_error_code".into(),
                    error_detail: Some("private diagnostic detail".into()),
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    // An approval-in-flight call must stay on the event journal. Including it
    // in this durable snapshot could race its corresponding live event.
    store
        .accept_tool_call(&ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-pending".into(),
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "pending secret query"}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(2),
            resolved_at: None,
        })
        .await
        .unwrap();

    let cancelled_call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: cancelled_call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-cancelled".into(),
            name: "request_folder_access".into(),
            arguments: serde_json::json!({"path": "/private/ignored"}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(1),
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                cancelled_call_id,
                &ToolCallResolution::Cancelled {
                    result: "declined by the user".into(),
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let call_id_text = call_id.to_string();
    for hidden in [
        secret_path,
        secret_result,
        secret_provider_id,
        "/private/ignored",
        "declined by the user",
        "pending secret query",
        "private_error_code",
        "private diagnostic detail",
        call_id_text.as_str(),
        "mcp__private_server__read_sensitive_path",
        "arguments",
        "result",
        "provider_id",
        "execution",
        "error_code",
        "error_detail",
        "client_executor_id",
        "client_lease",
    ] {
        assert!(
            !body.contains(hidden),
            "renderer-safe transcript leaked canonical tool data: {hidden}"
        );
    }
    let transcript: serde_json::Value = serde_json::from_str(&body).unwrap();
    let activity = transcript["tool_activity"].as_array().unwrap();
    assert_eq!(activity.len(), 2);
    assert!(activity.iter().any(|card| {
        card["title"] == "Use a tool"
            && card["status"] == "failed"
            && card["started_at"].is_string()
            && card["finished_at"].is_string()
    }));
    assert!(activity
        .iter()
        .any(|card| { card["title"] == "Request folder access" && card["status"] == "cancelled" }));
}

#[tokio::test]
async fn transcript_cursor_replays_an_active_turn_after_the_durable_boundary() {
    let active_delta_entered = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(ReplayBoundaryProvider {
        calls: AtomicUsize::new(0),
        active_delta_entered: active_delta_entered.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "first turn").await,
        StatusCode::ACCEPTED
    );
    let first_events = wait_for_turn(&store, chat.id).await;
    let terminal_seq = first_events
        .iter()
        .find_map(|event| {
            matches!(event.event, AgentEvent::TurnCompleted { .. }).then_some(event.seq)
        })
        .expect("the completed first turn has a terminal journal event");

    assert_eq!(
        send_message(&router, &bearer, chat.id, "second turn").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), active_delta_entered.notified())
        .await
        .expect("second turn entered the live provider stream");
    for _ in 0..100 {
        if store
            .list_events(chat.id, terminal_seq)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(&event.event, AgentEvent::TextDelta { text } if text == "still streaming")
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert_eq!(transcript["last_event_seq"], terminal_seq);
    assert!(transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"] == "durable answer"));

    let replay = store
        .list_events(chat.id, transcript["last_event_seq"].as_i64().unwrap())
        .await
        .unwrap();
    assert!(replay
        .iter()
        .any(|event| matches!(event.event, AgentEvent::TurnStarted { .. })));
    assert!(replay.iter().any(
        |event| matches!(&event.event, AgentEvent::TextDelta { text } if text == "still streaming")
    ));
}

#[tokio::test]
async fn transcript_hydration_reconciles_an_active_steer_by_message_identity() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(GatedProvider { gate })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "start").await,
        StatusCode::ACCEPTED
    );
    for _ in 0..100 {
        if store
            .get_turn_run(turn_id)
            .await
            .unwrap()
            .is_some_and(|turn| turn.status == TurnRunStatus::Running)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, turn_id, "remember this", true).await,
        StatusCode::ACCEPTED
    );

    let mut steered = None;
    for _ in 0..200 {
        if let Some(event) = store
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|event| matches!(&event.event, AgentEvent::UserSteered { content, .. } if content == "remember this"))
        {
            steered = Some(event);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let steered = steered.expect("active steer was journaled");
    let message_id = match steered.event {
        AgentEvent::UserSteered { message_id, .. } => message_id.to_string(),
        _ => unreachable!("filtered steer event"),
    };

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert!(transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["id"] == message_id && message["content"] == "remember this"));

    let replay = store
        .list_events(chat.id, transcript["last_event_seq"].as_i64().unwrap())
        .await
        .unwrap();
    let replayed_message_id = replay.into_iter().find_map(|event| match event.event {
        AgentEvent::UserSteered { message_id, .. } => Some(message_id.to_string()),
        _ => None,
    });
    assert_eq!(replayed_message_id.as_deref(), Some(message_id.as_str()));

    // This is the renderer's reconciliation rule: the exact durable identity,
    // not matching text, suppresses a replayed steer already in the snapshot.
    let mut hydrated_ids = transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    assert!(!hydrated_ids.insert(message_id));
}

#[tokio::test]
async fn patch_chat_rejects_empty_model_and_unknown_chat() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let empty = patch_chat(&router, &bearer, chat.id, serde_json::json!({"model": ""})).await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let legacy_path = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"workspace_dir": "/tmp/legacy"}),
    )
    .await;
    assert_eq!(legacy_path.status(), StatusCode::BAD_REQUEST);

    let forged_roots = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"root_attachments": []}),
    )
    .await;
    assert_eq!(forged_roots.status(), StatusCode::BAD_REQUEST);

    let missing = patch_chat(
        &router,
        &bearer,
        ChatId::new(),
        serde_json::json!({"model": "m"}),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_model_takes_precedence_over_the_default() {
    let recorder = RecordingProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");

    // A global default is set, but the chat picks its own model — the chat wins.
    let set_default = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "default-model"}),
    )
    .await;
    assert_eq!(set_default.status(), StatusCode::OK);
    let chat = make_chat(&router, &bearer).await;
    let patched = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": "chat-model"}),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hi").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    assert!(
        recorder
            .models
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "chat-model"),
        "the chat's own model should win over the global default"
    );
}

#[tokio::test]
async fn settings_default_then_update_roundtrips() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Default: no model configured.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(response).await;
    assert!(settings["model"].is_null());

    // PUT a model, and it comes back.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "claude-x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(settings["model"], "claude-x");

    // GET reflects the update.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(settings["model"], "claude-x");
}

/// PUT /settings with a raw JSON body, returning the response.
async fn put_settings(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn put_empty_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = put_settings(&router, &bearer, serde_json::json!({"model": ""})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn put_non_string_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    // A number where a string is expected fails extraction as a JSON 400.
    let response = put_settings(&router, &bearer, serde_json::json!({"model": 5})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn explicit_null_model_clears_a_configured_one() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Set, then clear with an explicit null.
    let set = put_settings(&router, &bearer, serde_json::json!({"model": "claude-x"})).await;
    assert_eq!(set.status(), StatusCode::OK);
    let cleared = put_settings(&router, &bearer, serde_json::json!({"model": null})).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(cleared).await;
    assert!(
        settings["model"].is_null(),
        "explicit null resets the model"
    );

    // An empty body leaves the (now-cleared) value unchanged.
    let untouched = put_settings(&router, &bearer, serde_json::json!({})).await;
    let settings: serde_json::Value = json_body(untouched).await;
    assert!(settings["model"].is_null());
}

/// `has_api_key` from `GET /settings`.
async fn api_key_configured(router: &Router, bearer: &str) -> bool {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json_body::<serde_json::Value>(response).await["has_api_key"]
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn api_key_put_configures_it_and_delete_reverts() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Capture the env-dependent baseline so the test is deterministic wherever
    // it runs, then assert the transitions the API drives.
    let baseline = api_key_configured(&router, &bearer).await;

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": "sk-test"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    assert!(api_key_configured(&router, &bearer).await);

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    assert_eq!(api_key_configured(&router, &bearer).await, baseline);
}

#[tokio::test]
async fn put_empty_api_key_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"api_key": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn web_search_credential_routes_are_authenticated_and_never_return_keys() {
    let (router, token, secrets, _dir) = test_app_with_web_search_secrets().await;
    let bearer = format!("Bearer {token}");

    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/web-search/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let initial = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/web-search/credentials")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(initial).await,
        serde_json::json!({
            "credentials": [
                {"provider": "exa", "has_credential": false},
                {"provider": "tavily", "has_credential": false}
            ]
        })
    );

    let key = "exa-secret-that-must-not-cross-the-api-boundary";
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/web-search/credentials/exa")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"api_key": key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_body = to_bytes(put.into_body(), usize::MAX).await.unwrap();
    assert!(!std::str::from_utf8(&put_body).unwrap().contains(key));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&put_body).unwrap(),
        serde_json::json!({"provider": "exa", "has_credential": true})
    );
    assert_eq!(
        secrets
            .get_secret("web_search.exa.api_key")
            .await
            .unwrap()
            .as_deref(),
        Some(key)
    );
    assert_eq!(
        secrets
            .get_secret("web_search.tavily.api_key")
            .await
            .unwrap(),
        None
    );

    let deleted = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/web-search/credentials/exa")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(deleted).await,
        serde_json::json!({"provider": "exa", "has_credential": false})
    );
    assert_eq!(
        secrets.get_secret("web_search.exa.api_key").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn web_search_credential_write_validates_fixed_provider_and_key_bounds() {
    let (router, token, secrets, _dir) = test_app_with_web_search_secrets().await;
    let bearer = format!("Bearer {token}");

    for body in [
        serde_json::json!({"api_key": ""}),
        serde_json::json!({"api_key": " \n\t "}),
        serde_json::json!({"api_key": "x".repeat(8 * 1024 + 1)}),
        serde_json::json!({"api_key": "valid", "unexpected": true}),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/web-search/credentials/exa")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let unknown = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/web-search/credentials/arbitrary-secret-name")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": "valid"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(secrets.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn providers_list_and_put_roundtrip() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(list).await;
    let providers = body["providers"].as_array().unwrap();
    assert!(providers.iter().any(|p| p["kind"] == "anthropic"));
    assert!(providers.iter().any(|p| p["kind"] == "openai"));
    assert!(providers.iter().any(|p| p["kind"] == "openai_compatible"));

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "sk-openai"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(put).await;
    assert_eq!(info["kind"], "openai");
    assert_eq!(info["enabled"], true);
    assert_eq!(info["has_credential"], true);
    assert!(info.get("credential").is_none());

    // Credential never appears on the list either.
    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = json_body(list).await;
    let openai = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "openai")
        .unwrap();
    assert_eq!(openai["has_credential"], true);
    assert!(openai.get("credential").is_none());

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/providers/openai/credential")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = json_body(list).await;
    let openai = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "openai")
        .unwrap();
    assert_eq!(openai["has_credential"], false);
}

#[tokio::test]
async fn openai_compatible_requires_base_url_when_enabled() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai_compatible")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_provider_kind_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/not-a-provider")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn models_catalog_includes_enabled_credentialed_providers() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "sk-openai"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    let models = catalog["models"].as_array().unwrap();
    assert!(models.iter().any(|m| m["provider"] == "openai"));
}

#[tokio::test]
async fn resolver_builds_a_router_from_enabled_providers() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-test"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
        },
    )
    .await
    .unwrap();

    let resolver = resolver::KeyedResolver::new(store.clone(), secrets.clone());
    let resolved = resolver.resolve().await;
    // Composite router — selection happens on stream from req.model.
    assert_eq!(resolved.id().0, "router");

    // Same route set ⇒ the cached provider is reused.
    let again = resolver.resolve().await;
    assert!(Arc::ptr_eq(&resolved, &again));

    // Changing the key rebuilds it.
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-different"),
    )
    .await
    .unwrap();
    let rebuilt = resolver.resolve().await;
    assert!(!Arc::ptr_eq(&resolved, &rebuilt));
    assert_eq!(rebuilt.id().0, "router");

    // Disabling Anthropic with no other providers fails closed.
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: false,
            base_url: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(resolver.resolve().await.id().0, "unconfigured");
}

#[tokio::test]
async fn resolver_includes_openai_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-openai"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
        },
    )
    .await
    .unwrap();

    let routes = providers::collect_routes(&*store, &*secrets).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, openwave_router::RouteKind::Openai);

    let resolver = resolver::KeyedResolver::new(store, secrets);
    let provider = resolver.resolve().await;
    assert_eq!(provider.id().0, "router");

    // A curated openai model is selectable; an anthropic model is not
    // (no anthropic route, no openai_compatible fallback).
    let router = openwave_router::Router::build(routes);
    assert_eq!(
        router.select("gpt-4o"),
        Some(openwave_router::RouteKind::Openai)
    );
    assert_eq!(router.select("claude-opus-4-8"), None);
}

#[tokio::test]
async fn openai_compatible_route_is_free_form_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderCredential::api_key("sk-local"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("http://127.0.0.1:1234/v1".into()),
        },
    )
    .await
    .unwrap();

    let routes = providers::collect_routes(&*store, &*secrets).await;
    let router = openwave_router::Router::build(routes);
    assert_eq!(
        router.select("llama-3-local"),
        Some(openwave_router::RouteKind::OpenaiCompatible)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_model_is_used_for_the_turn() {
    let recorder = RecordingProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // Configure the model, then run a turn.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "claude-configured"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hi").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;

    assert!(
        recorder
            .models
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "claude-configured"),
        "the turn should run against the configured model"
    );
}

#[tokio::test]
async fn workspace_dir_is_not_an_accepted_product_field() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"workspace_dir": "/tmp/legacy"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}", ChatId::new()))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_message_runs_a_turn_and_journals_its_events() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
    assert!(events
        .iter()
        .any(|e| matches!(e.event, AgentEvent::TurnCompleted { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn foreground_sandbox_spawn_parks_executes_delivers_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let provider = Arc::new(SandboxRoundTripProvider::default());
    let mut tools = ToolRegistry::new();
    tools.register_foreground_sandbox_spawn();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let sandbox_worker = sandbox_agent_run_worker::SandboxAgentRunWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.agent_run_wake.clone(),
        state.turn_job_wake.clone(),
        state.agent_config.clone(),
        None,
        sandbox_agent_run_worker::SandboxAgentRunWorkerConfig::default(),
    );
    tokio::spawn(sandbox_worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "delegate this").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));

    let runs = store.list_agent_runs(chat.id).await.unwrap();
    let child = runs
        .iter()
        .find(|run| run.parent_id.is_some())
        .expect("foreground delegation should durably create one child");
    assert_eq!(child.status, AgentRunStatus::Completed);
    let parent = openwave_core::AgentRunId::foreground_for_chat(chat.id);
    let inbox = store.list_agent_run_inbox(parent).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].child_run_id, child.id);
    assert_eq!(inbox[0].result.text, "child result");
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Consumed);
    assert_eq!(inbox[0].claim_count, 1);
    assert!(inbox[0].consumed_lease_token.is_some());

    let turn = store.list_turn_runs(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Completed);
    assert_eq!(
        turn.claim_count, 2,
        "the resumed turn requires a fresh lease"
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0]
        .tools
        .iter()
        .any(|tool| tool.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL));
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == openwave_core::SANDBOX_WEB_SEARCH_TOOL),
        "sandbox receives only the fixed web-search tool surface"
    );
    assert!(
        requests[2].messages.iter().any(|message| {
            message.role == Role::System
                && message.content
                    == vec![ContentBlock::Text {
                        text:
                            "Sandbox agent completed. Its exact final result follows:\nchild result"
                                .into(),
                    }]
        }),
        "the resumed foreground request must receive the durable child result"
    );
    assert!(requests[2]
        .tools
        .iter()
        .any(|tool| tool.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_drains_a_turn_queued_before_startup() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "fake", "queued before startup")
            .await
            .unwrap(),
        openwave_core::AcceptTurnOutcome::Accepted(_)
    ));

    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    spawn_turn_worker(&state);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(events[0].event, AgentEvent::TurnStarted { turn_id: id } if id == turn_id));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_message_is_idempotent_by_turn_id_and_rejects_identity_reuse() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await,
        StatusCode::ACCEPTED
    );
    store
        .set_setting("model", &serde_json::json!("changed-after-accept"))
        .await
        .unwrap();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await,
        StatusCode::ACCEPTED,
        "an ambiguous retry must converge even after model resolution changes"
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "different").await,
        StatusCode::CONFLICT,
        "one turn id cannot name different request data"
    );
    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_message_retry_converges_across_a_model_setting_race() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_acceptance();
    let store: Arc<dyn Store> = injected;
    store
        .set_setting("model", &serde_json::json!("model-before"))
        .await
        .unwrap();
    let gate = Arc::new(Notify::new());
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let (router, token, store, _dir) = test_app_from_parts(
        Arc::new(GatedProvider { gate: gate.clone() }),
        retrieval,
        store,
        dir,
    );
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let first_router = router.clone();
    let first_bearer = bearer.clone();
    let first = tokio::spawn(async move {
        send_message_with_id(
            &first_router,
            &first_bearer,
            chat.id,
            turn_id,
            "same request",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first request reached acceptance");

    store
        .set_setting("model", &serde_json::json!("model-after"))
        .await
        .unwrap();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "same request").await,
        StatusCode::ACCEPTED
    );
    release.notify_one();
    assert_eq!(first.await.unwrap(), StatusCode::ACCEPTED);
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_recovers_ambiguous_claim_and_completion_with_exact_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.fail_after_next_claim_commit();
    injected.fail_after_next_completion_commit();
    injected.fail_next_terminal_recovery();
    let store: Arc<dyn Store> = injected.clone();
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let (router, token, store, _dir) =
        test_app_from_parts(Arc::new(FakeProvider), retrieval, store, dir);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "recover me").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnCompleted { .. }))
            .count(),
        1
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages.len(), 2, "input and exact terminal output only");
    tokio::time::timeout(Duration::from_secs(5), async {
        while injected.terminal_recovery_calls() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("transient exact-terminal recovery is retried");

    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "lane is free").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn scanner_won_failure_does_not_wedge_the_only_worker_lane() {
    struct FailOnceProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for FailOnceProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fail-once")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AgentError::Provider("injected first failure".into()));
            }
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

    let (router, token, store, _dir) = test_app_with_scanner_resolution_race(
        Arc::new(FailOnceProvider {
            calls: AtomicUsize::new(0),
        }),
        PauseTerminalStore::let_scan_win_next_failure_resolution,
    )
    .await;
    let bearer = format!("Bearer {token}");
    let failed_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, failed_chat.id, "fail first").await,
        StatusCode::ACCEPTED
    );
    let failed_events = wait_for_turn(&store, failed_chat.id).await;
    assert!(matches!(
        failed_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "lease_expired"
    ));

    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "use freed lane").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn scanner_won_cancellation_does_not_wedge_the_only_worker_lane() {
    struct UsageGatedProvider {
        entered: Arc<Notify>,
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for UsageGatedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("usage-gated")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let entered = self.entered.clone();
            let gate = self.gate.clone();
            Ok(stream::iter(vec![ProviderEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            })])
            .chain(stream::once(async move {
                entered.notify_one();
                gate.notified().await;
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                }
            }))
            .boxed())
        }
    }

    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with_scanner_resolution_race(
        Arc::new(UsageGatedProvider {
            entered: entered.clone(),
            gate: gate.clone(),
        }),
        PauseTerminalStore::let_scan_win_next_cancellation_ack,
    )
    .await;
    let bearer = format!("Bearer {token}");
    let cancelled_chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, cancelled_chat.id, turn_id, "cancel first",).await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider consumed nonzero usage before parking");
    assert_eq!(
        cancel_turn(&router, &bearer, cancelled_chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    let cancelled_events = wait_for_turn(&store, cancelled_chat.id).await;
    assert!(matches!(
        cancelled_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { usage }) if *usage == Usage::default()
    ));

    gate.notify_one();
    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "use freed lane").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_renews_a_near_expiry_ambiguous_claim_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_after_next_claim_commit();
    injected.fail_after_next_heartbeat_commit();
    let store: Arc<dyn Store> = injected;
    let gate = Arc::new(Notify::new());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(GatedProvider {
            gate: gate.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        None,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(600),
            heartbeat: Duration::from_millis(200),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            max_concurrency: 1,
        },
    );
    let token = state.token.clone();
    tokio::spawn(worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "renew before work").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("claim committed before its delayed response");
    tokio::time::sleep(Duration::from_millis(450)).await;
    release.notify_one();
    for _ in 0..100 {
        if store
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.event, AgentEvent::TurnStarted { .. }))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    gate.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_heartbeats_while_event_journaling_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_nonterminal_event();
    let store: Arc<dyn Store> = injected;
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        None,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(250),
            heartbeat: Duration::from_millis(50),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            max_concurrency: 1,
        },
    );
    let token = state.token.clone();
    tokio::spawn(worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "keep alive").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("worker reached the blocked event append");
    tokio::time::sleep(Duration::from_millis(400)).await;
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(
        matches!(
            events.last().map(|event| &event.event),
            Some(AgentEvent::TurnCompleted { .. })
        ),
        "unexpected journal: {events:?}; turns: {:?}",
        store.list_turn_runs(chat.id).await.unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_nul_agent_output_fails_without_wedging_the_worker() {
    struct NulProvider;

    #[async_trait]
    impl ModelProvider for NulProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("nul")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "bad\0output".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (router, token, store, _dir) = test_app_with(Arc::new(NulProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "produce invalid output").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "invalid_agent_output"
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn post_message_rejects_blank_content_as_bad_request() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"turn_id": TurnId::new(), "content": " \n "}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": uuid::Uuid::nil(),
                        "content": "valid content"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn message_to_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    assert_eq!(
        send_message(&router, &format!("Bearer {token}"), ChatId::new(), "hi").await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_turn_on_the_same_chat_is_refused() {
    // A gated provider keeps the first turn active (blocked on the gate) while
    // we submit a second one, which must be refused with 409.
    let gate = Arc::new(Notify::new());
    let (router, token, _store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // The database's live-turn constraint owns the slot before the 202, even if
    // the worker has not claimed the queued turn yet.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "one").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::CONFLICT
    );

    // Release the first turn so it can finish and free the slot.
    gate.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
async fn slot_frees_after_a_turn_completes() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;

    // The turn finished, so its slot is released and a follow-up is accepted.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_reports_conflict_after_completion_wins() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "finish first").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::CONFLICT
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_slot_stays_held_and_cancel_can_win_terminal_commit_race() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "one").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("turn reached blocked atomic terminal commit");

    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::CONFLICT,
        "durable slot must remain held until the terminal transition commits"
    );
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, turn_id, "late", false).await,
        StatusCode::ACCEPTED,
        "a live durable turn must accept steering even while completion is in flight"
    );
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED,
        "durable cancellation may win until completion commits"
    );

    release.notify_one();
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(openwave_core::Role::User, "one")],
        "cancellation rejects the pending steer without an orphan assistant candidate"
    );
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn steer_wins_a_completion_race_and_restarts_generation() {
    struct FinishTwice {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for FinishTwice {
        fn id(&self) -> ProviderId {
            ProviderId::new("finish-twice")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let text = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                "before steer"
            } else {
                "after steer"
            };
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: text.into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FinishTwice {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first generation reached its atomic completion");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "replace the answer",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let interrupted_at = events
        .iter()
        .position(|event| matches!(event.event, AgentEvent::StreamInterrupted))
        .expect("superseded output clears already-streamed deltas");
    let steered_at = events
        .iter()
        .position(|event| {
            matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "replace the answer"
            )
        })
        .expect("the next generation applies the durable steer");
    assert!(interrupted_at < steered_at);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));

    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "replace the answer",
            ),
            (
                messages[2].id,
                openwave_core::Role::Assistant,
                "after steer"
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn late_steers_share_the_turn_wide_model_step_budget() {
    struct CountedFinish {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for CountedFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("counted-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "candidate".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(CountedFinish {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("model output reached atomic completion");
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "too late for another call",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(openwave_core::Role::User, "go")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_fails_closed_with_no_provider_configured() {
    // The unconfigured provider errors without any network call; the turn must
    // end in TurnFailed, not hang or egress.
    let (router, token, store, _dir) =
        test_app_with(Arc::new(crate::provider::UnconfiguredProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::TurnFailed { .. }
    ));
}

/// Sensitive tool that records whether it ran.
struct SensitiveProbe {
    ran: Arc<std::sync::atomic::AtomicUsize>,
    name: &'static str,
}

#[async_trait]
impl Tool for SensitiveProbe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "sensitive probe".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _args: serde_json::Value,
    ) -> openwave_core::Result<ToolOutput> {
        self.ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput::text("probed"))
    }
}

/// Provider that asks for `probe` once, then finishes.
struct ProbeProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    tool_name: &'static str,
}

#[async_trait]
impl ModelProvider for ProbeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("probe")
    }
    async fn stream(
        &self,
        _req: ChatRequest,
    ) -> openwave_core::Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_probe".into(),
                    name: self.tool_name.into(),
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

#[tokio::test(flavor = "multi_thread")]
async fn approval_endpoint_unparks_a_sensitive_tool() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe {
        ran: ran.clone(),
        name: "search",
    })));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ProbeProvider {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tool_name: "search",
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "probe it").await,
        StatusCode::ACCEPTED
    );

    // Wait until the turn parks on ApprovalRequired.
    let call_id = {
        let mut found = None;
        for _ in 0..200 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(id) = events.iter().find_map(|e| match &e.event {
                AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                _ => None,
            }) {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("turn should park on ApprovalRequired")
    };
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Approve via the HTTP endpoint.
    let decide = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events
        .iter()
        .any(|e| matches!(e.event, AgentEvent::ApprovalDecided { approved: true, .. })));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::TurnCompleted { .. }
    ));

    // A second decide for the same call is 404 (already resolved).
    let again = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_endpoint_rejects_unpresentable_sensitive_tool_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_name = "third_party_sensitive";
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe {
        ran: ran.clone(),
        name: tool_name,
    })));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ProbeProvider {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tool_name,
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
        .0,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "run it").await,
        StatusCode::ACCEPTED
    );
    let call_id = {
        let mut found = None;
        for _ in 0..200 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(id) = events.iter().find_map(|event| match &event.event {
                AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                _ => None,
            }) {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("turn should park on ApprovalRequired")
    };

    let approve = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(approve).await;
    assert_eq!(info.kind, "approval_action_not_presentable");
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);

    let reject = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "reject"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reject.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        event.event,
        AgentEvent::ApprovalDecided {
            approved: false,
            ..
        }
    )));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn bind_yields_a_loopback_addr_and_token() {
    openwave_core::KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    assert!(server.local_addr().ip().is_loopback());
    assert!(!server.token().is_empty());
    assert!(server.store().list_chats().await.unwrap().is_empty());
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

#[tokio::test]
async fn self_host_profile_is_not_yet_supported() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        profile: Profile::SelfHost,
        data_dir: dir.path().to_path_buf(),
    };
    assert!(bind(config).await.is_err());
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

#[tokio::test(flavor = "multi_thread")]
async fn serve_answers_over_a_real_socket() {
    openwave_core::KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    let addr = server.local_addr();
    let token = server.token().to_string();
    // The listener is already bound, so connections queue immediately; drive
    // the accept loop in the background for the duration of the test.
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    let client = reqwest::Client::new();
    let health = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    let unauthed = client
        .get(format!("http://{addr}/chats"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

    let authed = client
        .get(format!("http://{addr}/chats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status(), reqwest::StatusCode::OK);
    assert_eq!(authed.json::<Vec<Chat>>().await.unwrap(), vec![]);
}

#[tokio::test(flavor = "multi_thread")]
async fn cors_preflight_allows_localhost_origin() {
    openwave_core::KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    let addr = server.local_addr();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    let client = reqwest::Client::new();
    let preflight = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/chats"))
        .header(reqwest::header::ORIGIN, "http://localhost:1420")
        .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(
            reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), reqwest::StatusCode::OK);
    let allow_origin = preflight
        .headers()
        .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok());
    assert_eq!(allow_origin, Some("http://localhost:1420"));
}

// --- WebSocket event stream ---

use std::net::SocketAddr;

use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

struct SensitiveEventProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SensitiveEventProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("provider-secret-id")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "provider-secret-call-id".into(),
                    name: "provider_secret_tool".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"/Users/private/file.txt","secret":"hunter2"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "safe assistant response".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Serve a router (with the given provider) over a real loopback socket.
async fn serve_app_with(
    provider: Arc<dyn ModelProvider>,
) -> (SocketAddr, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (router, token, store, dir) = test_app_with(provider).await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, token, store, dir)
}

async fn make_chat_http(client: &reqwest::Client, addr: SocketAddr, token: &str) -> Chat {
    client
        .post(format!("http://{addr}/chats"))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn send_message_http(client: &reqwest::Client, addr: SocketAddr, token: &str, chat: ChatId) {
    let response = client
        .post(format!("http://{addr}/chats/{chat}/messages"))
        .bearer_auth(token)
        .json(&serde_json::json!({"turn_id": TurnId::new(), "content": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

/// Connect to a chat's event socket (authenticated) and read frames until
/// `want` turns have ended (or a timeout), returning the decoded events in
/// arrival order.
async fn read_until_turns_end(
    addr: SocketAddr,
    token: &str,
    chat: ChatId,
    after: i64,
    want: usize,
) -> Vec<RendererSequencedEvent> {
    let mut request = format!("ws://{addr}/chats/{chat}/events?after={after}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _response) = connect_async(request).await.unwrap();

    let mut events = Vec::new();
    let mut completed = 0;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: RendererSequencedEvent = serde_json::from_str(text.as_str()).unwrap();
            if matches!(
                event.event,
                RendererAgentEvent::TurnCompleted | RendererAgentEvent::TurnFailed
            ) {
                completed += 1;
            }
            events.push(event);
            if completed >= want {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turns did not complete over the socket");
    events
}

/// Read one turn's worth of events over a fresh connection.
async fn read_until_turn_end(
    addr: SocketAddr,
    token: &str,
    chat: ChatId,
    after: i64,
) -> Vec<RendererSequencedEvent> {
    read_until_turns_end(addr, token, chat, after, 1).await
}

fn decode_ws_event(message: WsMessage) -> RendererSequencedEvent {
    let WsMessage::Text(text) = message else {
        panic!("expected a JSON text event frame");
    };
    serde_json::from_str(text.as_str()).unwrap()
}

async fn read_raw_until_terminal<S>(socket: &mut S) -> Vec<serde_json::Value>
where
    S: futures::Stream<
            Item = std::result::Result<WsMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let mut events = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            let terminal = matches!(
                event["event"]["type"].as_str(),
                Some("turn_completed" | "turn_failed" | "turn_cancelled")
            );
            events.push(event);
            if terminal {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not terminate over the event socket");
    events
}

fn assert_renderer_event_frames_are_redacted(events: &[serde_json::Value]) {
    let serialized = serde_json::to_string(events).unwrap();
    for forbidden in [
        "provider-secret-id",
        "provider-secret-call-id",
        "provider_secret_tool",
        "/Users/private",
        "file.txt",
        "hunter2",
        "fragment",
        "output",
        "content",
        "data",
        "summary",
        "usage",
        "stop_reason",
        "diagnostic",
        "lease",
        "checkpoint",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "event stream leaked {forbidden}"
        );
    }
    assert!(events.iter().any(|event| event["event"]["name"] == "other"));
    assert!(events.iter().any(|event| {
        event["event"]["type"] == "tool_call_completed" && event["event"]["status"] == "failed"
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_live_and_replay_frames_use_the_renderer_safe_projection() {
    let provider = Arc::new(SensitiveEventProvider {
        calls: AtomicUsize::new(0),
    });
    let (addr, token, store, _dir) = serve_app_with(provider).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    let mut request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut live_socket, _) = connect_async(request).await.unwrap();

    send_message_http(&client, addr, &token, chat.id).await;
    let live = read_raw_until_terminal(&mut live_socket).await;
    assert_renderer_event_frames_are_redacted(&live);

    wait_for_turn(&store, chat.id).await;
    let mut replay_request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    replay_request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut replay_socket, _) = connect_async(replay_request).await.unwrap();
    let replay = read_raw_until_terminal(&mut replay_socket).await;
    assert_renderer_event_frames_are_redacted(&replay);

    let live_sequences = live
        .iter()
        .map(|event| event["seq"].clone())
        .collect::<Vec<_>>();
    let replay_sequences = replay
        .iter()
        .map(|event| event["seq"].clone())
        .collect::<Vec<_>>();
    assert_eq!(replay_sequences, live_sequences);
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_a_journal_gap_before_accepting_a_later_live_event() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig::default(),
    );
    let token = state.token.clone();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    let first = AgentEvent::TextDelta { text: "one".into() };
    assert_eq!(store.append_event(chat.id, &first).await.unwrap(), 1);
    let mut request = format!("ws://{addr}/chats/{}/events", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let replayed_first = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("initial journal replay timed out")
        .expect("event socket closed")
        .unwrap();
    assert_eq!(decode_ws_event(replayed_first).seq, 1);

    let second = AgentEvent::TextDelta { text: "two".into() };
    let third = AgentEvent::TextDelta {
        text: "three".into(),
    };
    assert_eq!(store.append_event(chat.id, &second).await.unwrap(), 2);
    assert_eq!(store.append_event(chat.id, &third).await.unwrap(), 3);
    let _ = state.events.sender(chat.id).send(SequencedEvent {
        seq: 3,
        event: third,
    });
    let _ = state.events.sender(chat.id).send(SequencedEvent {
        seq: 2,
        event: second.clone(),
    });

    let mut recovered = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("gap recovery timed out")
            .expect("event socket closed")
            .unwrap();
        recovered.push(decode_ws_event(frame));
    }
    assert_eq!(
        recovered.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(
        recovered[0].event,
        RendererAgentEvent::TextDelta { text: "two".into() }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err(),
        "the late live seq 2 must be deduplicated after journal replay"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_a_finished_turn_from_the_journal() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    // Run the turn to completion, then connect — everything comes from replay.
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    let events = read_until_turn_end(addr, &token, chat.id, 0).await;
    assert_eq!(events.first().unwrap().seq, 1, "replay starts at seq 1");
    assert!(matches!(
        events[0].event,
        RendererAgentEvent::TurnStarted { .. }
    ));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, RendererAgentEvent::TextDelta { text } if text == "hi")));
    assert!(matches!(
        events.last().unwrap().event,
        RendererAgentEvent::TurnCompleted
    ));
    // Sequence numbers are strictly increasing.
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_streams_a_turn_started_after_connecting() {
    let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    // Connect first (journal empty), then trigger the turn — events arrive live.
    let reader = {
        let token = token.clone();
        tokio::spawn(async move { read_until_turn_end(addr, &token, chat.id, 0).await })
    };
    send_message_http(&client, addr, &token, chat.id).await;

    let events = reader.await.unwrap();
    assert!(matches!(
        events[0].event,
        RendererAgentEvent::TurnStarted { .. }
    ));
    assert!(matches!(
        events.last().unwrap().event,
        RendererAgentEvent::TurnCompleted
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_after_cursor_replays_only_newer_events() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Resume after seq 1: the first replayed event must be seq 2, and seq 1 is
    // not re-sent.
    let events = read_until_turn_end(addr, &token, chat.id, 1).await;
    assert_eq!(events.first().unwrap().seq, 2);
    assert!(events.iter().all(|e| e.seq > 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_one_turn_then_streams_the_next_live() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    // Turn 1 runs to completion and is journaled.
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Connect (replays turn 1) and keep reading; then run turn 2, whose events
    // arrive live on the same connection. Assert both turns come through in
    // one gap-free, duplicate-free, strictly-increasing stream.
    let reader = {
        let token = token.clone();
        tokio::spawn(async move { read_until_turns_end(addr, &token, chat.id, 0, 2).await })
    };
    // Let the reader connect, subscribe, and drain the replay before turn 2.
    tokio::time::sleep(Duration::from_millis(100)).await;
    send_message_http(&client, addr, &token, chat.id).await;

    let events = reader.await.unwrap();
    assert!(matches!(
        events[0].event,
        RendererAgentEvent::TurnStarted { .. }
    ));
    assert_eq!(events[0].seq, 1);
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.event, RendererAgentEvent::TurnCompleted))
            .count(),
        2,
        "both turns completed over one connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_bad_after_cursor_is_a_json_400() {
    let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    // A non-integer `after` fails extraction; it must answer the API-wide
    // `{ kind, message }` JSON, not axum's plain-text rejection.
    let response = client
        .get(format!("http://{addr}/chats/{}/events?after=abc", chat.id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = response.json().await.unwrap();
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_without_a_token_is_rejected() {
    let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let chat = ChatId::new();
    let request = format!("ws://{addr}/chats/{chat}/events")
        .into_client_request()
        .unwrap();
    // No Authorization header: the handshake must fail (auth runs before upgrade).
    assert!(connect_async(request).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_subprotocol_auth_succeeds() {
    use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Authenticate with Sec-WebSocket-Protocol only — no Authorization header.
    let mut request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}{token}")
            .parse()
            .unwrap(),
    );
    let (mut socket, response) = connect_async(request).await.unwrap();
    // Server must select the handshake subprotocol.
    let selected = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok());
    assert_eq!(selected, Some(WS_HANDSHAKE_SUBPROTOCOL));

    let mut saw_completed = false;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: RendererSequencedEvent = serde_json::from_str(text.as_str()).unwrap();
            if matches!(event.event, RendererAgentEvent::TurnCompleted) {
                saw_completed = true;
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over subprotocol-authed socket");
    assert!(saw_completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_subprotocol_wrong_token_is_rejected() {
    use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

    let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let chat = ChatId::new();
    let mut request = format!("ws://{addr}/chats/{chat}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}not-the-token")
            .parse()
            .unwrap(),
    );
    assert!(connect_async(request).await.is_err());
}
