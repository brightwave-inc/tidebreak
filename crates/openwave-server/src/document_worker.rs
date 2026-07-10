//! Durable document-index job execution.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use openwave_core::{
    AgentError, DocumentJob, DocumentJobId, DocumentJobKind, DocumentJobStatus, Result, Store,
};
use openwave_retrieval::{
    Document, DocumentSource, GenerationStageOutcome, RetrievalError, Retriever,
};
use tokio::sync::Notify;

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
}

#[derive(Clone)]
pub(crate) struct DocumentWorker {
    store: Arc<dyn Store>,
    retrieval: Arc<Retriever>,
    wake: Arc<Notify>,
    config: DocumentWorkerConfig,
}

enum Supervised<T> {
    Completed(T),
    LeaseLost,
}

impl DocumentWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        retrieval: Arc<Retriever>,
        wake: Arc<Notify>,
        config: DocumentWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.heartbeat < config.lease);
        Self {
            store,
            retrieval,
            wake,
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
        let now = Utc::now();
        let lease_expires_at = now + chrono_duration(self.config.lease)?;
        let Some(job) = self.store.claim_document_job(now, lease_expires_at).await? else {
            return Ok(WorkerOutcome::Idle);
        };
        self.process(job).await
    }

    async fn process(&self, job: DocumentJob) -> Result<WorkerOutcome> {
        if job.kind != DocumentJobKind::Index || job.status != DocumentJobStatus::Running {
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
    Document::with_id(
        record.id,
        source,
        record.media_type.clone(),
        record.canonical_text.clone(),
    )
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
    use openwave_core::{DbStore, DocumentId, DocumentProcessingStatus, DocumentUpsert};
    use openwave_retrieval::{
        Embedder, Embedding, HashEmbedder, InMemoryVectorStore, PlainTextParser, TextChunker,
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
            retrieval.clone(),
            Arc::new(Notify::new()),
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
            updated_at: Utc::now(),
        }
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
            retrieval.search("worker indexing", 5).await.unwrap().len(),
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
            retrieval.clone(),
            Arc::new(Notify::new()),
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
}
