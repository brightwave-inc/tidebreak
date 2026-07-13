//! Durable Parse and Index document-job execution.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use openwave_core::{
    AgentError, BlobStore, DocumentId, DocumentJob, DocumentJobId, DocumentJobKind,
    DocumentJobStatus, DocumentParseOutput, Result, Store,
};
use openwave_retrieval::{
    Document, DocumentSource, GenerationStageOutcome, RetrievalError, Retriever,
};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::state::DocumentWriteGuard;

/// Maximum attempts assigned to newly enqueued index jobs.
pub(crate) const MAX_INDEX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DocumentWorkerConfig {
    lease: Duration,
    heartbeat: Duration,
    retry_base: Duration,
    retry_cap: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
}

impl Default for DocumentWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            retry_base: Duration::from_secs(5),
            retry_cap: Duration::from_secs(5 * 60),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerOutcome {
    Idle,
    Completed(DocumentJobId),
    RetryScheduled(DocumentJobId),
    Failed(DocumentJobId),
    LeaseLost(DocumentJobId),
    Superseded(DocumentJobId),
    Retired(DocumentId),
}

#[derive(Clone)]
pub(crate) struct DocumentWorker {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    retrieval: Arc<Retriever>,
    wake: Arc<Notify>,
    document_writes: Arc<DocumentWriteGuard>,
    prefer_retirement: Arc<AtomicBool>,
    retirement_cursor: Arc<Mutex<Option<DocumentId>>>,
    config: DocumentWorkerConfig,
}

enum Supervised<T> {
    Completed(T),
    LeaseLost,
}

impl DocumentWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        retrieval: Arc<Retriever>,
        wake: Arc<Notify>,
        document_writes: Arc<DocumentWriteGuard>,
        config: DocumentWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.heartbeat < config.lease);
        Self {
            store,
            blobs,
            retrieval,
            wake,
            document_writes,
            prefer_retirement: Arc::new(AtomicBool::new(true)),
            retirement_cursor: Arc::new(Mutex::new(None)),
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut idle_delay = self.config.idle_min;
        loop {
            match self.run_once().await {
                Ok(WorkerOutcome::Idle) => {
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => idle_delay = self.config.idle_min,
                Err(error) => {
                    eprintln!("openwave: document worker iteration failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    pub(crate) async fn run_once(&self) -> Result<WorkerOutcome> {
        let retirement_first = self.prefer_retirement.fetch_xor(true, Ordering::Relaxed);
        if retirement_first {
            if let Some(outcome) = self.retire_one().await? {
                return Ok(outcome);
            }
        }
        let now = Utc::now();
        let lease_expires_at = now + chrono_duration(self.config.lease)?;
        if let Some(job) = self.store.claim_document_job(now, lease_expires_at).await? {
            return self.process(job).await;
        }
        if !retirement_first {
            if let Some(outcome) = self.retire_one().await? {
                return Ok(outcome);
            }
        }
        Ok(WorkerOutcome::Idle)
    }

    async fn retire_one(&self) -> Result<Option<WorkerOutcome>> {
        const BATCH_SIZE: u64 = 32;
        let after = *self.retirement_cursor.lock().unwrap();
        let mut retirements = self
            .store
            .list_pending_document_retirements(after, BATCH_SIZE)
            .await?;
        if retirements.is_empty() && after.is_some() {
            retirements = self
                .store
                .list_pending_document_retirements(None, BATCH_SIZE)
                .await?;
        }
        if retirements.is_empty() {
            *self.retirement_cursor.lock().unwrap() = None;
            return Ok(None);
        }

        let mut last_error = None;
        for (document_id, generation) in retirements {
            *self.retirement_cursor.lock().unwrap() = Some(document_id);
            match self.retire(document_id, generation).await {
                Ok(outcome) => return Ok(Some(outcome)),
                Err(error) => {
                    eprintln!(
                        "openwave: document {document_id} retirement attempt failed: {error}"
                    );
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.expect("a nonempty retirement batch produced an error"))
    }

    async fn retire(
        &self,
        document_id: DocumentId,
        generation: openwave_core::DocumentGeneration,
    ) -> Result<WorkerOutcome> {
        // Deletion and recreation take this same lock. Once held, an exact
        // operational tombstone cannot be replaced between vector activation
        // and the completion CAS below.
        let _document_write = self.document_writes.acquire(document_id).await;
        if self
            .store
            .get_pending_document_retirement(document_id)
            .await?
            != Some(generation)
        {
            return Ok(WorkerOutcome::Idle);
        }

        match self
            .retrieval
            .stage_document_tombstone(document_id, generation)
            .await
            .map_err(|error| AgentError::msg(error.to_string()))?
        {
            GenerationStageOutcome::Rejected { current } => {
                return Err(AgentError::msg(format!(
                    "vector generation {} fenced pending document {document_id} retirement {}",
                    current.content_revision, generation.content_revision
                )));
            }
            GenerationStageOutcome::Staged | GenerationStageOutcome::AlreadyPresent => {}
        }
        if !self
            .retrieval
            .activate_document_generation(document_id, generation)
            .await
            .map_err(|error| AgentError::msg(error.to_string()))?
        {
            return Err(AgentError::msg(format!(
                "exact tombstone generation {} for document {document_id} was not activatable",
                generation.content_revision
            )));
        }
        if !self
            .store
            .complete_document_retirement(document_id, generation)
            .await?
        {
            return Ok(WorkerOutcome::Idle);
        }
        Ok(WorkerOutcome::Retired(document_id))
    }

    async fn process(&self, job: DocumentJob) -> Result<WorkerOutcome> {
        if job.status != DocumentJobStatus::Running {
            return Err(AgentError::msg(format!(
                "claimed document job {} has an invalid execution state",
                job.id
            )));
        }
        let lease_token = job.lease_token.ok_or_else(|| {
            AgentError::msg(format!(
                "claimed document job {} has no lease token",
                job.id
            ))
        })?;
        let Some(source) = self.store.get_document(job.document_id).await? else {
            return Ok(WorkerOutcome::Superseded(job.id));
        };
        if source.generation() != job.generation() {
            return Ok(WorkerOutcome::Superseded(job.id));
        }
        match job.kind {
            DocumentJobKind::Parse => self.process_parse(job, lease_token, source).await,
            DocumentJobKind::Index => self.process_index(job, lease_token, source).await,
            _ => {
                self.record_failure(
                    &job,
                    lease_token,
                    false,
                    "unsupported_job_kind",
                    "document worker does not support this semantic job kind",
                )
                .await
            }
        }
    }

    async fn process_parse(
        &self,
        job: DocumentJob,
        lease_token: uuid::Uuid,
        source: openwave_core::DocumentRecord,
    ) -> Result<WorkerOutcome> {
        if source.canonical_fingerprint.is_some() {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "invalid_document_stage",
                    "parse job does not own a document with published canonical output",
                )
                .await;
        }
        let fingerprint = match self.retrieval.canonical_fingerprint_for(&source.media_type) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                return self
                    .record_failure(
                        &job,
                        lease_token,
                        false,
                        "parser_unavailable",
                        &error.to_string(),
                    )
                    .await;
            }
        };
        if job.pipeline_fingerprint != fingerprint {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "pipeline_changed",
                    "job parser fingerprint does not match the active parser",
                )
                .await;
        }
        let Some(descriptor) = source.source_blob.as_ref() else {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "source_blob_missing",
                    "parse job has no retained source descriptor",
                )
                .await;
        };
        let blob_id = descriptor.id;
        let bytes = match self
            .supervise(&job, lease_token, self.blobs.get(blob_id))
            .await
        {
            Supervised::LeaseLost => return Ok(WorkerOutcome::LeaseLost(job.id)),
            Supervised::Completed(Ok(Some(bytes))) => bytes,
            Supervised::Completed(Ok(None)) => {
                return self
                    .record_failure(
                        &job,
                        lease_token,
                        false,
                        "source_blob_missing",
                        "retained source blob does not exist",
                    )
                    .await;
            }
            Supervised::Completed(Err(error)) => {
                return self
                    .record_failure(
                        &job,
                        lease_token,
                        true,
                        "source_blob_read_failed",
                        &error.to_string(),
                    )
                    .await;
            }
        };
        if usize::try_from(descriptor.byte_len).ok() != Some(bytes.len()) {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "source_blob_length_mismatch",
                    "retained source byte length does not match its descriptor",
                )
                .await;
        }
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != descriptor.sha256 {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "source_blob_digest_mismatch",
                    "retained source digest does not match its descriptor",
                )
                .await;
        }
        let document_source = match source.source_uri.clone() {
            Some(uri) => DocumentSource::uri(uri),
            None => DocumentSource::Inline,
        };
        let parsed = match self
            .supervise(
                &job,
                lease_token,
                self.retrieval
                    .parse_document(document_source, &source.media_type, &bytes),
            )
            .await
        {
            Supervised::LeaseLost => return Ok(WorkerOutcome::LeaseLost(job.id)),
            Supervised::Completed(Ok(parsed)) => parsed,
            Supervised::Completed(Err(error)) => {
                let (retryable, code) = classify_retrieval_error(&error);
                return self
                    .record_failure(&job, lease_token, retryable, code, &error.to_string())
                    .await;
            }
        };
        let completed = self
            .store
            .complete_document_parse_job_and_enqueue_index(
                job.id,
                lease_token,
                Utc::now(),
                &DocumentParseOutput {
                    canonical_text: parsed.text,
                    source_regions: parsed.source_regions,
                },
                &self.retrieval.index_fingerprint(),
                MAX_INDEX_ATTEMPTS,
            )
            .await?;
        Ok(if completed.is_some() {
            WorkerOutcome::Completed(job.id)
        } else {
            WorkerOutcome::LeaseLost(job.id)
        })
    }

    async fn process_index(
        &self,
        job: DocumentJob,
        lease_token: uuid::Uuid,
        source: openwave_core::DocumentRecord,
    ) -> Result<WorkerOutcome> {
        if source.source_blob.is_some() && source.canonical_fingerprint.is_none() {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "invalid_document_stage",
                    "index job cannot consume canonical output that is still pending",
                )
                .await;
        }
        // A recreated source can have a newer live generation while an older
        // tombstone still has to retire the prior corpus publication. Retire
        // that exact watermark before attempting to stage the recreated job;
        // otherwise corpus validation correctly fences the new records behind
        // the still-active old scope.
        if let Some(retirement) = self
            .store
            .get_pending_document_retirement(job.document_id)
            .await?
        {
            self.retire(job.document_id, retirement).await?;
        }
        let fingerprint = self.retrieval.index_fingerprint();
        if job.pipeline_fingerprint != fingerprint {
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "pipeline_changed",
                    "job pipeline fingerprint does not match the active retrieval pipeline",
                )
                .await;
        }

        let document = canonical_document(&source);
        let staged = match self
            .supervise(
                &job,
                lease_token,
                self.retrieval
                    .stage_document_generation(&document, job.generation()),
            )
            .await
        {
            Supervised::LeaseLost => return Ok(WorkerOutcome::LeaseLost(job.id)),
            Supervised::Completed(Ok(staged)) => staged,
            Supervised::Completed(Err(error)) => {
                let (retryable, code) = classify_retrieval_error(&error);
                return self
                    .record_failure(&job, lease_token, retryable, code, &error.to_string())
                    .await;
            }
        };
        if let GenerationStageOutcome::Rejected { current } = staged.stage {
            if !self.prove_lease(&job, lease_token).await {
                return Ok(WorkerOutcome::LeaseLost(job.id));
            }
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "generation_fenced",
                    &format!(
                        "vector generation {} fenced requested revision {}",
                        current.content_revision, job.content_revision
                    ),
                )
                .await;
        }
        // Keep the final operational proof, vector activation, and Ready
        // publication in the same local lifecycle critical section as HTTP
        // replacement/deletion. Without this, a request can cancel the lease in
        // the narrow post-proof window and the stale stage can still activate.
        let _document_write = self.document_writes.acquire(job.document_id).await;
        if !self.prove_lease(&job, lease_token).await {
            return Ok(WorkerOutcome::LeaseLost(job.id));
        }

        let activated = match self
            .supervise(
                &job,
                lease_token,
                self.retrieval
                    .activate_document_generation(job.document_id, job.generation()),
            )
            .await
        {
            Supervised::LeaseLost => return Ok(WorkerOutcome::LeaseLost(job.id)),
            Supervised::Completed(Ok(activated)) => activated,
            Supervised::Completed(Err(error)) => {
                let (retryable, code) = classify_retrieval_error(&error);
                return self
                    .record_failure(&job, lease_token, retryable, code, &error.to_string())
                    .await;
            }
        };
        if !activated {
            if !self.prove_lease(&job, lease_token).await {
                return Ok(WorkerOutcome::LeaseLost(job.id));
            }
            return self
                .record_failure(
                    &job,
                    lease_token,
                    false,
                    "activation_fenced",
                    "exact staged vector generation was no longer activatable",
                )
                .await;
        }

        let completed = self
            .store
            .complete_document_index_job(job.id, lease_token, Utc::now())
            .await?;
        Ok(if completed {
            WorkerOutcome::Completed(job.id)
        } else {
            WorkerOutcome::LeaseLost(job.id)
        })
    }

    async fn supervise<T>(
        &self,
        job: &DocumentJob,
        lease_token: uuid::Uuid,
        future: impl Future<Output = T>,
    ) -> Supervised<T> {
        tokio::pin!(future);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                result = &mut future => return Supervised::Completed(result),
                _ = heartbeat.tick() => {
                    if !self.prove_lease(job, lease_token).await {
                        return Supervised::LeaseLost;
                    }
                }
            }
        }
    }

    async fn prove_lease(&self, job: &DocumentJob, lease_token: uuid::Uuid) -> bool {
        let now = Utc::now();
        let Ok(lease) = chrono_duration(self.config.lease) else {
            return false;
        };
        matches!(
            self.store
                .heartbeat_document_job(job.id, lease_token, now, now + lease)
                .await,
            Ok(true)
        )
    }

    async fn record_failure(
        &self,
        job: &DocumentJob,
        lease_token: uuid::Uuid,
        retryable: bool,
        code: &str,
        detail: &str,
    ) -> Result<WorkerOutcome> {
        let failed_at = Utc::now();
        let retry_at = if retryable && job.attempt_count < job.max_attempts {
            Some(failed_at + chrono_duration(self.retry_delay(job))?)
        } else {
            None
        };
        let detail = truncate_detail(detail);
        let status = self
            .store
            .record_document_job_failure(
                job.id,
                lease_token,
                failed_at,
                retry_at,
                code,
                Some(&detail),
            )
            .await?;
        Ok(match status {
            Some(DocumentJobStatus::RetryWait) => WorkerOutcome::RetryScheduled(job.id),
            Some(DocumentJobStatus::Failed) => WorkerOutcome::Failed(job.id),
            Some(_) | None => WorkerOutcome::LeaseLost(job.id),
        })
    }

    fn retry_delay(&self, job: &DocumentJob) -> Duration {
        let exponent = job.attempt_count.saturating_sub(1).clamp(0, 20) as u32;
        self.config
            .retry_base
            .saturating_mul(2_u32.saturating_pow(exponent))
            .min(self.config.retry_cap)
    }
}

fn canonical_document(record: &openwave_core::DocumentRecord) -> Document {
    let source = match record.source_uri.clone() {
        Some(uri) => DocumentSource::uri(uri),
        None => DocumentSource::Inline,
    };
    let document = match record.project_id {
        Some(project_id) => Document::with_id_scoped(
            record.id,
            project_id,
            source,
            record.media_type.clone(),
            record.canonical_text.clone(),
        ),
        None => Document::with_id(
            record.id,
            source,
            record.media_type.clone(),
            record.canonical_text.clone(),
        ),
    };
    document.with_source_regions(record.source_regions.clone())
}

fn classify_retrieval_error(error: &RetrievalError) -> (bool, &'static str) {
    match error {
        RetrievalError::Parse(_) => (false, "parse_failed"),
        RetrievalError::DimensionMismatch { .. } => (false, "dimension_mismatch"),
        RetrievalError::VectorStore(message) if message.contains("conflicting revision tokens") => {
            (false, "generation_conflict")
        }
        RetrievalError::Embed(_) => (true, "embedding_failed"),
        RetrievalError::VectorStore(_) => (true, "vector_store_failed"),
        _ => (true, "index_failed"),
    }
}

fn truncate_detail(detail: &str) -> String {
    detail
        .chars()
        .take(openwave_core::DocumentJob::MAX_ERROR_DETAIL_LEN)
        .collect()
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid document-worker duration: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use openwave_core::{
        ByteSpan, DbStore, DocumentId, DocumentProcessingStatus, DocumentSourceBlob,
        DocumentSourceUpsert, DocumentUpsert, Project, ProjectId, SourceLocation, SourceRegion,
    };
    use openwave_retrieval::{
        Embedder, Embedding, HashEmbedder, InMemoryVectorStore, PlainTextParser, ScoredChunk,
        SearchScope, TextChunker, VectorRecord, VectorStore,
    };

    use super::*;

    struct SwitchEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    struct GatedEmbedder {
        inner: HashEmbedder,
        entered: Notify,
        release: Notify,
    }

    struct GatedActivationVectorStore {
        inner: InMemoryVectorStore,
        gate_next: AtomicBool,
        fail_tombstone_for: Mutex<Option<DocumentId>>,
        entered: Notify,
        release: Notify,
    }

    impl GatedActivationVectorStore {
        fn new(dimensions: usize) -> Self {
            Self {
                inner: InMemoryVectorStore::new(dimensions),
                gate_next: AtomicBool::new(false),
                fail_tombstone_for: Mutex::new(None),
                entered: Notify::new(),
                release: Notify::new(),
            }
        }
    }

    #[async_trait]
    impl VectorStore for GatedActivationVectorStore {
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
            document_id: DocumentId,
            records: Vec<VectorRecord>,
        ) -> openwave_retrieval::Result<()> {
            self.inner.replace_document(document_id, records).await
        }

        async fn stage_document_generation(
            &self,
            document_id: DocumentId,
            generation: openwave_core::DocumentGeneration,
            records: Vec<VectorRecord>,
        ) -> openwave_retrieval::Result<GenerationStageOutcome> {
            if records.is_empty()
                && self
                    .fail_tombstone_for
                    .lock()
                    .unwrap()
                    .is_some_and(|failed| failed == document_id)
            {
                return Err(RetrievalError::vector_store(
                    "injected permanent tombstone failure",
                ));
            }
            self.inner
                .stage_document_generation(document_id, generation, records)
                .await
        }

        async fn activate_document_generation(
            &self,
            document_id: DocumentId,
            generation: openwave_core::DocumentGeneration,
        ) -> openwave_retrieval::Result<bool> {
            if self.gate_next.swap(false, Ordering::SeqCst) {
                self.entered.notify_waiters();
                self.release.notified().await;
            }
            self.inner
                .activate_document_generation(document_id, generation)
                .await
        }

        async fn active_document_generation(
            &self,
            document_id: DocumentId,
        ) -> openwave_retrieval::Result<Option<openwave_core::DocumentGeneration>> {
            self.inner.active_document_generation(document_id).await
        }

        async fn newest_document_generation(
            &self,
            document_id: DocumentId,
        ) -> openwave_retrieval::Result<Option<openwave_retrieval::DocumentGenerationState>>
        {
            self.inner.newest_document_generation(document_id).await
        }

        async fn document_len(
            &self,
            document_id: DocumentId,
        ) -> openwave_retrieval::Result<Option<usize>> {
            self.inner.document_len(document_id).await
        }

        async fn len(&self) -> openwave_retrieval::Result<usize> {
            self.inner.len().await
        }
    }

    #[async_trait]
    impl Embedder for SwitchEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn fingerprint(&self) -> String {
            "worker-test-pipeline-v1".into()
        }

        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> openwave_retrieval::Result<Vec<Embedding>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(RetrievalError::embed("injected transient failure"));
            }
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> openwave_retrieval::Result<Embedding> {
            self.inner.embed_query(text).await
        }
    }

    #[async_trait]
    impl Embedder for GatedEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn fingerprint(&self) -> String {
            "worker-gated-pipeline-v1".into()
        }

        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> openwave_retrieval::Result<Vec<Embedding>> {
            self.entered.notify_waiters();
            self.release.notified().await;
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> openwave_retrieval::Result<Embedding> {
            self.inner.embed_query(text).await
        }
    }

    async fn harness() -> (
        tempfile::TempDir,
        Arc<dyn Store>,
        Arc<Retriever>,
        Arc<SwitchEmbedder>,
        DocumentWorker,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("worker.db").display()
            ))
            .await
            .unwrap(),
        );
        let embedder = Arc::new(SwitchEmbedder {
            inner: HashEmbedder::new(32),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            embedder.clone(),
            Arc::new(InMemoryVectorStore::new(32)),
        ));
        let worker = DocumentWorker::new(
            store.clone(),
            Arc::new(openwave_core::FsBlobStore::new(dir.path().join("blobs"))),
            retrieval.clone(),
            Arc::new(Notify::new()),
            Arc::new(DocumentWriteGuard::default()),
            test_config(),
        );
        (dir, store, retrieval, embedder, worker)
    }

    fn test_config() -> DocumentWorkerConfig {
        DocumentWorkerConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(50),
            retry_base: Duration::from_millis(1),
            retry_cap: Duration::from_millis(10),
            idle_min: Duration::from_millis(1),
            idle_cap: Duration::from_millis(5),
            failure_delay: Duration::from_millis(1),
        }
    }

    fn source(id: DocumentId, text: &str) -> DocumentUpsert {
        DocumentUpsert {
            id,
            project_id: None,
            source_uri: Some(format!("file:///{id}.txt")),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: text.into(),
            source_regions: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn parse_job_rejects_source_bytes_that_do_not_match_the_descriptor() {
        let (_dir, store, retrieval, _embedder, worker) = harness().await;
        let raw = b"tampered source bytes".to_vec();
        let source_blob = DocumentSourceBlob::from_digest([0x77; 32], raw.len() as u64);
        let blob_id = source_blob.id;
        worker.blobs.put(blob_id, raw.clone()).await.unwrap();
        let source = DocumentSourceUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///tampered.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            source_blob,
            updated_at: Utc::now(),
        };
        let (_, parse_job) = store
            .accept_document_source_and_enqueue_parse(
                &source,
                &retrieval
                    .canonical_fingerprint_for(&source.media_type)
                    .unwrap(),
                3,
            )
            .await
            .unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Failed(parse_job.id)
        );
        let failed = store.get_document_job(parse_job.id).await.unwrap().unwrap();
        assert_eq!(failed.status, DocumentJobStatus::Failed);
        assert_eq!(
            failed.last_error_code.as_deref(),
            Some("source_blob_digest_mismatch")
        );
        let document = store.get_document(source.id).await.unwrap().unwrap();
        assert_eq!(document.processing_status, DocumentProcessingStatus::Failed);
        assert_eq!(document.canonical_fingerprint, None);
    }

    #[tokio::test]
    async fn run_once_stages_activates_and_completes_the_exact_job() {
        let (_dir, store, retrieval, embedder, worker) = harness().await;
        let id = DocumentId::new();
        let (document, job) = store
            .upsert_document_and_enqueue_index(
                &source(id, "durable worker indexing"),
                &retrieval.index_fingerprint(),
                3,
            )
            .await
            .unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(job.id)
        );
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        let completed = store.get_document_job(job.id).await.unwrap().unwrap();
        assert_eq!(completed.status, DocumentJobStatus::Succeeded);
        let ready = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(ready.processing_status, DocumentProcessingStatus::Ready);
        assert_eq!(ready.indexed_revision, Some(document.content_revision));
        assert_eq!(
            retrieval
                .store()
                .active_document_generation(id)
                .await
                .unwrap(),
            Some(document.generation())
        );
        assert_eq!(
            retrieval
                .search(
                    openwave_retrieval::SearchScope::Unscoped,
                    "worker indexing",
                    5,
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn worker_carries_catalog_page_regions_into_citations() {
        let (_dir, store, retrieval, _embedder, worker) = harness().await;
        let id = DocumentId::new();
        let text = "durable page provenance";
        let mut input = source(id, text);
        input.media_type = "application/pdf".into();
        input.source_regions = vec![SourceRegion {
            span: ByteSpan::new(0, text.len()),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(3).unwrap(),
            },
        }];
        let (_, job) = store
            .upsert_document_and_enqueue_index(&input, &retrieval.index_fingerprint(), 3)
            .await
            .unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(job.id)
        );
        let citations = retrieval
            .search(SearchScope::Unscoped, "durable page provenance", 1)
            .await
            .unwrap();
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].source_regions, input.source_regions);
        assert_eq!(citations[0].snippet, text);
    }

    #[tokio::test]
    async fn recreation_in_a_new_corpus_retires_before_publishing() {
        let (_dir, store, retrieval, _embedder, worker) = harness().await;
        let id = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        for project_id in [project_a, project_b] {
            store
                .create_project(&Project {
                    id: project_id,
                    title: None,
                    workspace_dir: std::path::PathBuf::from(format!("/{project_id}")),
                    created_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let mut first = source(id, "first project corpus");
        first.project_id = Some(project_a);
        let (first_record, first_job) = store
            .upsert_document_and_enqueue_index(&first, &retrieval.index_fingerprint(), 3)
            .await
            .unwrap();
        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(first_job.id)
        );

        let tombstone = store.delete_document(id).await.unwrap();
        let mut recreated = source(id, "second project corpus");
        recreated.project_id = Some(project_b);
        let (recreated_record, recreated_job) = store
            .upsert_document_and_enqueue_index(&recreated, &retrieval.index_fingerprint(), 3)
            .await
            .unwrap();
        assert_eq!(
            tombstone.content_revision,
            first_record.content_revision + 1
        );
        assert_eq!(
            recreated_record.content_revision,
            tombstone.content_revision + 1
        );
        assert_eq!(
            store.get_pending_document_retirement(id).await.unwrap(),
            Some(tombstone)
        );

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(recreated_job.id)
        );
        assert_eq!(
            store.get_pending_document_retirement(id).await.unwrap(),
            None
        );
        assert_eq!(
            retrieval
                .store()
                .active_document_generation(id)
                .await
                .unwrap(),
            Some(recreated_record.generation())
        );
        assert_eq!(
            retrieval
                .search(
                    openwave_retrieval::SearchScope::Project(project_a),
                    "project corpus",
                    5,
                )
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            retrieval
                .search(
                    openwave_retrieval::SearchScope::Project(project_b),
                    "project corpus",
                    5,
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn transient_embedding_failure_retries_then_completes() {
        let (_dir, store, retrieval, embedder, worker) = harness().await;
        let id = DocumentId::new();
        let (_, job) = store
            .upsert_document_and_enqueue_index(
                &source(id, "retry the embedding"),
                &retrieval.index_fingerprint(),
                3,
            )
            .await
            .unwrap();
        embedder.fail.store(true, Ordering::SeqCst);
        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::RetryScheduled(job.id)
        );
        assert_eq!(
            store
                .get_document_job(job.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::RetryWait
        );

        embedder.fail.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(3)).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(job.id)
        );
        let completed = store.get_document_job(job.id).await.unwrap().unwrap();
        assert_eq!(completed.status, DocumentJobStatus::Succeeded);
        assert_eq!(completed.attempt_count, 2);
    }

    #[tokio::test]
    async fn expired_staged_job_recovers_without_reembedding() {
        let (_dir, store, retrieval, embedder, worker) = harness().await;
        let id = DocumentId::new();
        let (source, job) = store
            .upsert_document_and_enqueue_index(
                &source(id, "recover the durable stage"),
                &retrieval.index_fingerprint(),
                2,
            )
            .await
            .unwrap();
        let old_now = Utc::now();
        let claimed = store
            .claim_document_job(old_now, old_now + chrono::Duration::milliseconds(1))
            .await
            .unwrap()
            .unwrap();
        let document = canonical_document(&source);
        retrieval
            .stage_document_generation(&document, claimed.generation())
            .await
            .unwrap();
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        embedder.fail.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(3)).await;

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Completed(job.id)
        );
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .get_document_job(job.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn pipeline_mismatch_fails_without_vector_publication() {
        let (_dir, store, _retrieval, embedder, worker) = harness().await;
        let id = DocumentId::new();
        let (_, job) = store
            .upsert_document_and_enqueue_index(&source(id, "old pipeline"), "old-pipeline", 3)
            .await
            .unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Failed(job.id)
        );
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .get_document_job(job.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Failed
        );
    }

    #[tokio::test]
    async fn superseding_source_cancels_blocked_work_at_the_next_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("heartbeat.db").display()
            ))
            .await
            .unwrap(),
        );
        let embedder = Arc::new(GatedEmbedder {
            inner: HashEmbedder::new(32),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            embedder.clone(),
            Arc::new(InMemoryVectorStore::new(32)),
        ));
        let worker = DocumentWorker::new(
            store.clone(),
            Arc::new(openwave_core::FsBlobStore::new(dir.path().join("blobs"))),
            retrieval.clone(),
            Arc::new(Notify::new()),
            Arc::new(DocumentWriteGuard::default()),
            test_config(),
        );
        let id = DocumentId::new();
        let (_, first) = store
            .upsert_document_and_enqueue_index(
                &source(id, "first blocked version"),
                &retrieval.index_fingerprint(),
                3,
            )
            .await
            .unwrap();

        let run = tokio::spawn({
            let worker = worker.clone();
            async move { worker.run_once().await.unwrap() }
        });
        tokio::time::timeout(Duration::from_secs(1), embedder.entered.notified())
            .await
            .expect("worker did not enter embedding");
        let (_, second) = store
            .upsert_document_and_enqueue_index(
                &source(id, "newer source wins"),
                &retrieval.index_fingerprint(),
                3,
            )
            .await
            .unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), run)
                .await
                .expect("worker did not abandon the cancelled lease")
                .unwrap(),
            WorkerOutcome::LeaseLost(first.id)
        );
        assert_eq!(
            store
                .get_document_job(first.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Cancelled
        );
        assert_eq!(
            store
                .get_document_job(second.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Queued
        );
        assert_eq!(
            retrieval
                .store()
                .newest_document_generation(id)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn source_replacement_cannot_cancel_in_the_activation_window() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("activation-window.db").display()
            ))
            .await
            .unwrap(),
        );
        let vectors = Arc::new(GatedActivationVectorStore::new(32));
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(32)),
            vectors.clone(),
        ));
        let document_writes = Arc::new(DocumentWriteGuard::default());
        let worker = DocumentWorker::new(
            store.clone(),
            Arc::new(openwave_core::FsBlobStore::new(dir.path().join("blobs"))),
            retrieval.clone(),
            Arc::new(Notify::new()),
            document_writes.clone(),
            test_config(),
        );
        let id = DocumentId::new();
        let (first_record, _) = store
            .upsert_document_and_enqueue_index(
                &source(id, "first version"),
                &retrieval.index_fingerprint(),
                3,
            )
            .await
            .unwrap();
        vectors.gate_next.store(true, Ordering::SeqCst);

        let run = tokio::spawn({
            let worker = worker.clone();
            async move { worker.run_once().await.unwrap() }
        });
        tokio::time::timeout(Duration::from_secs(1), vectors.entered.notified())
            .await
            .expect("worker did not reach gated activation");

        let replacement = tokio::spawn({
            let store = store.clone();
            let retrieval = retrieval.clone();
            let document_writes = document_writes.clone();
            async move {
                let _write = document_writes.acquire(id).await;
                store
                    .upsert_document_and_enqueue_index(
                        &source(id, "second version"),
                        &retrieval.index_fingerprint(),
                        3,
                    )
                    .await
                    .unwrap()
            }
        });
        tokio::task::yield_now().await;
        assert!(
            !replacement.is_finished(),
            "replacement must not cancel a job after its final proof"
        );

        vectors.release.notify_one();
        assert!(matches!(run.await.unwrap(), WorkerOutcome::Completed(_)));
        let (second_record, second_job) = replacement.await.unwrap();
        assert_eq!(second_record.content_revision, 2);
        assert_eq!(second_job.status, DocumentJobStatus::Queued);
        assert_eq!(
            vectors.active_document_generation(id).await.unwrap(),
            Some(first_record.generation())
        );
    }

    #[tokio::test]
    async fn poisoned_retirement_does_not_starve_later_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("retirement-fairness.db").display()
            ))
            .await
            .unwrap(),
        );
        let vectors = Arc::new(GatedActivationVectorStore::new(32));
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(32)),
            vectors.clone(),
        ));
        let worker = DocumentWorker::new(
            store.clone(),
            Arc::new(openwave_core::FsBlobStore::new(dir.path().join("blobs"))),
            retrieval,
            Arc::new(Notify::new()),
            Arc::new(DocumentWriteGuard::default()),
            test_config(),
        );
        let poison = DocumentId(uuid::Uuid::from_u128(1));
        let healthy = DocumentId(uuid::Uuid::from_u128(2));
        store.delete_document(poison).await.unwrap();
        store.delete_document(healthy).await.unwrap();
        *vectors.fail_tombstone_for.lock().unwrap() = Some(poison);

        assert_eq!(
            worker.run_once().await.unwrap(),
            WorkerOutcome::Retired(healthy)
        );
        let pending = store
            .list_pending_document_retirements(None, 10)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, poison);
    }
}
