//! Reconcile operational document state with the current retrieval generation.

use std::sync::Arc;
use std::time::Duration;

use openwave_core::{
    DocumentId, DocumentIndexJobReason, DocumentJobStatus, DocumentProcessingStatus, DocumentScope,
    EnsureDocumentIndexJobOutcome, Result, Store,
};
use openwave_retrieval::{Document, DocumentGenerationState, DocumentSource, Retriever};
use tokio::sync::Notify;

use crate::document_worker::MAX_INDEX_ATTEMPTS;
use crate::state::DocumentWriteGuard;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DocumentAuditorConfig {
    interval: Duration,
    failure_delay: Duration,
}

impl Default for DocumentAuditorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(6 * 60 * 60),
            failure_delay: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DocumentAuditReport {
    pub scanned: usize,
    pub enqueued: usize,
    pub skipped: usize,
    pub failed_jobs: usize,
    pub failures: Vec<DocumentAuditFailure>,
}

#[derive(Debug)]
pub(crate) struct DocumentAuditFailure {
    pub document_id: DocumentId,
    pub error: String,
}

#[derive(Clone)]
pub(crate) struct DocumentAuditor {
    store: Arc<dyn Store>,
    retrieval: Arc<Retriever>,
    document_writes: Arc<DocumentWriteGuard>,
    wake: Arc<Notify>,
    config: DocumentAuditorConfig,
}

impl DocumentAuditor {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        retrieval: Arc<Retriever>,
        document_writes: Arc<DocumentWriteGuard>,
        wake: Arc<Notify>,
        config: DocumentAuditorConfig,
    ) -> Self {
        Self {
            store,
            retrieval,
            document_writes,
            wake,
            config,
        }
    }

    pub(crate) async fn run(self) {
        loop {
            let delay = match self.audit_once().await {
                Ok(report) => {
                    let retry_soon = !report.failures.is_empty();
                    if report.enqueued > 0 || report.failed_jobs > 0 || !report.failures.is_empty()
                    {
                        eprintln!(
                            "openwave: document audit scanned {}, enqueued {}, skipped {}, failed jobs {}, errors {}",
                            report.scanned,
                            report.enqueued,
                            report.skipped,
                            report.failed_jobs,
                            report.failures.len()
                        );
                    }
                    for failure in report.failures {
                        eprintln!(
                            "openwave: document {} audit failed: {}",
                            failure.document_id, failure.error
                        );
                    }
                    if retry_soon {
                        self.config.failure_delay
                    } else {
                        self.config.interval
                    }
                }
                Err(error) => {
                    eprintln!("openwave: document audit scan failed: {error}");
                    self.config.failure_delay
                }
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub(crate) async fn audit_once(&self) -> Result<DocumentAuditReport> {
        let document_ids = self.store.list_document_ids(DocumentScope::All).await?;
        let fingerprint = self.retrieval.index_fingerprint();
        let mut report = DocumentAuditReport {
            scanned: document_ids.len(),
            ..DocumentAuditReport::default()
        };

        for document_id in document_ids {
            if let Err(error) = self
                .audit_document(document_id, &fingerprint, &mut report)
                .await
            {
                report.failures.push(DocumentAuditFailure {
                    document_id,
                    error: error.to_string(),
                });
            }
        }
        if report.enqueued > 0 {
            self.wake.notify_one();
        }
        Ok(report)
    }

    async fn audit_document(
        &self,
        document_id: DocumentId,
        fingerprint: &str,
        report: &mut DocumentAuditReport,
    ) -> Result<()> {
        let _document_write = self.document_writes.acquire(document_id).await;
        let Some(document) = self.store.get_document(document_id).await? else {
            report.skipped += 1;
            return Ok(());
        };
        let jobs = self.store.list_document_jobs(document_id).await?;
        let current_jobs: Vec<_> = jobs
            .iter()
            .filter(|job| job.generation() == document.generation())
            .collect();
        let desired_job = current_jobs
            .iter()
            .copied()
            .find(|job| job.pipeline_fingerprint == fingerprint);

        if desired_job.is_some_and(|job| {
            matches!(
                job.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            )
        }) {
            report.skipped += 1;
            return Ok(());
        }
        if desired_job.is_some_and(|job| job.status == DocumentJobStatus::Failed) {
            report.failed_jobs += 1;
            return Ok(());
        }

        let known_pipeline_changed = document
            .index_fingerprint
            .as_deref()
            .is_some_and(|indexed| indexed != fingerprint)
            || (!current_jobs.is_empty()
                && current_jobs
                    .iter()
                    .all(|job| job.pipeline_fingerprint != fingerprint));
        let reason = if known_pipeline_changed {
            DocumentIndexJobReason::PipelineChanged
        } else if document.processing_status == DocumentProcessingStatus::Ready
            && document.indexed_revision == Some(document.content_revision)
            && document.index_fingerprint.as_deref() == Some(fingerprint)
        {
            match self
                .retrieval
                .store()
                .newest_document_generation(document_id)
                .await
                .map_err(|error| openwave_core::AgentError::msg(error.to_string()))?
            {
                Some(DocumentGenerationState::Active(generation))
                    if generation == document.generation() =>
                {
                    if self
                        .retrieval
                        .index_is_complete(&canonical_document(&document))
                        .await
                        .map_err(|error| openwave_core::AgentError::msg(error.to_string()))?
                    {
                        report.skipped += 1;
                        return Ok(());
                    }
                    DocumentIndexJobReason::DerivedStateIncomplete
                }
                Some(state)
                    if state.generation().content_revision > document.content_revision
                        || (state.generation().content_revision == document.content_revision
                            && state.generation().revision_token != document.revision_token) =>
                {
                    return Err(openwave_core::AgentError::msg(format!(
                        "vector generation {:?} is not covered by operational generation {:?}",
                        state.generation(),
                        document.generation()
                    )));
                }
                Some(DocumentGenerationState::Staged(generation))
                    if generation == document.generation() =>
                {
                    DocumentIndexJobReason::DerivedStateIncomplete
                }
                Some(DocumentGenerationState::Active(_))
                | Some(DocumentGenerationState::Staged(_))
                | None => DocumentIndexJobReason::DerivedStateMissing,
            }
        } else {
            DocumentIndexJobReason::DerivedStateMissing
        };

        match self
            .store
            .ensure_document_index_job(
                document_id,
                document.generation(),
                fingerprint,
                MAX_INDEX_ATTEMPTS,
                reason,
            )
            .await?
        {
            EnsureDocumentIndexJobOutcome::Enqueued(_) => report.enqueued += 1,
            EnsureDocumentIndexJobOutcome::Failed(_) => report.failed_jobs += 1,
            EnsureDocumentIndexJobOutcome::Existing(_)
            | EnsureDocumentIndexJobOutcome::Parsing(_)
            | EnsureDocumentIndexJobOutcome::MissingDocument
            | EnsureDocumentIndexJobOutcome::GenerationChanged(_) => report.skipped += 1,
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use openwave_core::{
        ByteSpan, DbStore, DocumentJobStatus, DocumentProcessingStatus, DocumentUpsert,
        SourceLocation, SourceRegion,
    };
    use openwave_retrieval::{
        Document, DocumentSource, HashEmbedder, InMemoryVectorStore, PlainTextParser, TextChunker,
    };

    use super::*;

    async fn harness() -> (tempfile::TempDir, Arc<dyn Store>, Arc<Retriever>) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("auditor.db").display()
            ))
            .await
            .unwrap(),
        );
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(32)),
            Arc::new(InMemoryVectorStore::new(32)),
        ));
        (dir, store, retrieval)
    }

    fn source(id: DocumentId, text: &str) -> DocumentUpsert {
        DocumentUpsert {
            id,
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: text.into(),
            source_regions: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    async fn complete_operational_job(
        store: &Arc<dyn Store>,
        source: &DocumentUpsert,
        fingerprint: &str,
    ) -> (openwave_core::DocumentRecord, openwave_core::DocumentJob) {
        let (record, job) = store
            .upsert_document_and_enqueue_index(source, fingerprint, 3)
            .await
            .unwrap();
        let now = Utc::now();
        let claimed = store
            .claim_document_job(now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, job.id);
        assert!(store
            .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), Utc::now())
            .await
            .unwrap());
        (record, job)
    }

    fn auditor(store: Arc<dyn Store>, retrieval: Arc<Retriever>) -> DocumentAuditor {
        DocumentAuditor::new(
            store,
            retrieval,
            Arc::new(DocumentWriteGuard::default()),
            Arc::new(Notify::new()),
            DocumentAuditorConfig {
                interval: Duration::from_secs(60),
                failure_delay: Duration::from_secs(1),
            },
        )
    }

    #[tokio::test]
    async fn canonical_document_preserves_catalog_source_regions() {
        let (_dir, store, _retrieval) = harness().await;
        let text = "page provenance";
        let mut input = source(DocumentId::new(), text);
        input.source_regions = vec![SourceRegion {
            span: ByteSpan::new(0, text.len()),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(4).unwrap(),
            },
        }];
        let record = store.upsert_document(&input).await.unwrap();

        assert_eq!(
            canonical_document(&record).source_regions,
            input.source_regions
        );
    }

    #[tokio::test]
    async fn missing_derived_generation_requeues_the_exact_succeeded_job() {
        let (_dir, store, retrieval) = harness().await;
        let id = DocumentId::new();
        let (record, job) = complete_operational_job(
            &store,
            &source(id, "missing derived rows"),
            &retrieval.index_fingerprint(),
        )
        .await;

        let report = auditor(store.clone(), retrieval)
            .audit_once()
            .await
            .unwrap();
        assert_eq!(report.enqueued, 1);
        let current = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(current.generation(), record.generation());
        assert_eq!(current.processing_status, DocumentProcessingStatus::Queued);
        let repaired = store.get_document_job(job.id).await.unwrap().unwrap();
        assert_eq!(repaired.status, DocumentJobStatus::Queued);
        assert_eq!(repaired.attempt_count, 0);
    }

    #[tokio::test]
    async fn pipeline_change_advances_once_and_enqueues_the_desired_job() {
        let (_dir, store, retrieval) = harness().await;
        let id = DocumentId::new();
        let (old, _) =
            complete_operational_job(&store, &source(id, "same canonical source"), "old-pipeline")
                .await;
        let auditor = auditor(store.clone(), retrieval.clone());

        let first = auditor.audit_once().await.unwrap();
        assert_eq!(first.enqueued, 1);
        let advanced = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(advanced.content_revision, old.content_revision + 1);
        assert_eq!(advanced.canonical_text, old.canonical_text);
        assert_eq!(advanced.processing_status, DocumentProcessingStatus::Queued);
        let jobs = store.list_document_jobs(id).await.unwrap();
        assert!(jobs.iter().any(|job| {
            job.generation() == advanced.generation()
                && job.pipeline_fingerprint == retrieval.index_fingerprint()
                && job.status == DocumentJobStatus::Queued
        }));

        let second = auditor.audit_once().await.unwrap();
        assert_eq!(second.enqueued, 0);
        assert_eq!(
            store.get_document(id).await.unwrap().unwrap().generation(),
            advanced.generation()
        );
    }

    #[tokio::test]
    async fn exact_active_ready_generation_is_left_untouched() {
        let (_dir, store, retrieval) = harness().await;
        let id = DocumentId::new();
        let source = source(id, "healthy active generation");
        let (record, _) =
            complete_operational_job(&store, &source, &retrieval.index_fingerprint()).await;
        let document = Document::with_id(
            id,
            DocumentSource::Inline,
            source.media_type,
            source.canonical_text,
        );
        retrieval
            .stage_document_generation(&document, record.generation())
            .await
            .unwrap();
        assert!(retrieval
            .activate_document_generation(id, record.generation())
            .await
            .unwrap());

        let report = auditor(store.clone(), retrieval)
            .audit_once()
            .await
            .unwrap();
        assert_eq!(report.enqueued, 0);
        assert!(report.failures.is_empty());
        assert_eq!(
            store.get_document(id).await.unwrap().unwrap().generation(),
            record.generation()
        );
    }

    #[tokio::test]
    async fn incomplete_exact_active_generation_advances_before_rebuild() {
        let (_dir, store, retrieval) = harness().await;
        let id = DocumentId::new();
        let (record, _) = complete_operational_job(
            &store,
            &source(id, "this live document expects chunks"),
            &retrieval.index_fingerprint(),
        )
        .await;
        retrieval
            .stage_document_tombstone(id, record.generation())
            .await
            .unwrap();
        assert!(retrieval
            .activate_document_generation(id, record.generation())
            .await
            .unwrap());

        let report = auditor(store.clone(), retrieval)
            .audit_once()
            .await
            .unwrap();
        assert_eq!(report.enqueued, 1);
        let advanced = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(advanced.content_revision, record.content_revision + 1);
        assert_eq!(advanced.processing_status, DocumentProcessingStatus::Queued);
    }

    #[tokio::test]
    async fn exact_staged_ready_generation_advances_instead_of_publishing_unknown_chunks() {
        let (_dir, store, retrieval) = harness().await;
        let id = DocumentId::new();
        let (record, _) = complete_operational_job(
            &store,
            &source(id, "staged state is not proof of complete publication"),
            &retrieval.index_fingerprint(),
        )
        .await;
        retrieval
            .stage_document_tombstone(id, record.generation())
            .await
            .unwrap();

        let report = auditor(store.clone(), retrieval)
            .audit_once()
            .await
            .unwrap();
        assert_eq!(report.enqueued, 1);
        let advanced = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(advanced.content_revision, record.content_revision + 1);
        assert_eq!(advanced.processing_status, DocumentProcessingStatus::Queued);
    }
}
