//! Durable parse document-job execution.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use openwave_core::{
    AgentError, BlobStore, DocumentJob, DocumentJobId, DocumentJobStatus, DocumentParseOutput,
    Result, Store,
};
use openwave_retrieval::{RetrievalError, Retriever};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DocumentWorkerConfig {
    lease: Duration,
    heartbeat: Duration,
    retry_base: Duration,
    retry_cap: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
    max_concurrency: usize,
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
            max_concurrency: 4,
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
    blobs: Arc<dyn BlobStore>,
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
        blobs: Arc<dyn BlobStore>,
        retrieval: Arc<Retriever>,
        wake: Arc<Notify>,
        config: DocumentWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.heartbeat < config.lease);
        assert!(config.max_concurrency > 0);
        Self {
            store,
            blobs,
            retrieval,
            wake,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_lane());
        }
        while let Some(result) = lanes.join_next().await {
            if let Err(error) = result {
                eprintln!("openwave: document worker lane stopped: {error}");
                tokio::time::sleep(self.config.failure_delay).await;
            }
            lanes.spawn(self.clone().run_lane());
        }
    }

    async fn run_lane(self) {
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
        self.wake.notify_one();
        self.process(job).await
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
        self.process_parse(job, lease_token, source).await
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
        let parsed = match self
            .supervise(
                &job,
                lease_token,
                self.retrieval.parse_document(&source.media_type, &bytes),
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
            .complete_document_parse_job(
                job.id,
                lease_token,
                Utc::now(),
                &DocumentParseOutput {
                    canonical_text: parsed.text,
                    source_regions: parsed.source_regions,
                },
            )
            .await?;
        Ok(if completed.is_some() {
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

fn classify_retrieval_error(error: &RetrievalError) -> (bool, &'static str) {
    match error {
        RetrievalError::Parse(_) => (false, "parse_failed"),
        _ => (true, "parse_failed"),
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
