//! Reconcile raw sources whose parse jobs still need to run.

use std::sync::Arc;
use std::time::Duration;

use openwave_core::{DocumentId, DocumentScope, EnsureDocumentParseJobOutcome, Result, Store};
use openwave_retrieval::Retriever;
use tokio::sync::Notify;

use crate::document_stage::MAX_PARSE_ATTEMPTS;
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
                    let retry_soon = report.enqueued > 0 || report.failed_jobs > 0;
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
        let mut report = DocumentAuditReport {
            scanned: document_ids.len(),
            ..DocumentAuditReport::default()
        };

        for document_id in document_ids {
            if let Err(error) = self.audit_document(document_id, &mut report).await {
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
        report: &mut DocumentAuditReport,
    ) -> Result<()> {
        let _document_write = self.document_writes.acquire(document_id).await;
        let Some(document) = self.store.get_document(document_id).await? else {
            report.skipped += 1;
            return Ok(());
        };
        if document.source_blob.is_none() || document.canonical_fingerprint.is_some() {
            report.skipped += 1;
            return Ok(());
        }
        let parser_fingerprint = match self
            .retrieval
            .canonical_fingerprint_for(&document.media_type)
        {
            Ok(fingerprint) => fingerprint,
            Err(_) => {
                report.skipped += 1;
                return Ok(());
            }
        };
        match self
            .store
            .ensure_document_parse_job(
                document_id,
                document.generation(),
                &parser_fingerprint,
                MAX_PARSE_ATTEMPTS,
            )
            .await?
        {
            EnsureDocumentParseJobOutcome::Enqueued(_) => report.enqueued += 1,
            EnsureDocumentParseJobOutcome::Failed(_) => report.failed_jobs += 1,
            EnsureDocumentParseJobOutcome::Existing(_)
            | EnsureDocumentParseJobOutcome::SourceUnavailable
            | EnsureDocumentParseJobOutcome::MissingDocument
            | EnsureDocumentParseJobOutcome::GenerationChanged(_)
            | EnsureDocumentParseJobOutcome::CanonicalCurrent => report.skipped += 1,
        }
        Ok(())
    }
}
