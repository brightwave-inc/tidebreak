//! Background repair of derived index rows from authoritative catalog content.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use openwave_core::{DocumentId, DocumentRecord, DocumentScope, Result, Store};
use openwave_retrieval::{Document, DocumentSource, Retriever};

use crate::state::DocumentWriteGuard;

/// Outcome of one best-effort catalog scan.
#[derive(Debug, Default)]
pub(crate) struct IndexRepairReport {
    pub scanned: usize,
    pub repaired: usize,
    pub skipped: usize,
    pub failures: Vec<IndexRepairFailure>,
}

/// A document left stale after a failed repair attempt.
#[derive(Debug)]
pub(crate) struct IndexRepairFailure {
    pub document_id: DocumentId,
    pub error: String,
}

#[derive(Debug, Clone)]
struct PendingWatermark {
    revision: i64,
    revision_token: uuid::Uuid,
    fingerprint: String,
}

/// Reconciliation state retained across background scans.
#[derive(Debug, Default)]
pub(crate) struct IndexRepairer {
    pending_watermarks: HashMap<DocumentId, PendingWatermark>,
}

/// Rebuild stale or pipeline-mismatched document rows from canonical source.
///
/// The initial catalog scan is the only fatal operation. Individual documents
/// are isolated: one provider/vector/store failure remains visible as a stale
/// watermark but does not prevent other records from being repaired.
#[cfg(test)]
pub(crate) async fn repair_document_index(
    store: Arc<dyn Store>,
    retrieval: Arc<Retriever>,
    writes: Arc<DocumentWriteGuard>,
) -> Result<IndexRepairReport> {
    IndexRepairer::default()
        .repair(store, retrieval, writes)
        .await
}

impl IndexRepairer {
    /// Run one reconciliation while retaining successful-but-unmarked index
    /// writes for cheap watermark-only retries on the next pass.
    pub(crate) async fn repair(
        &mut self,
        store: Arc<dyn Store>,
        retrieval: Arc<Retriever>,
        writes: Arc<DocumentWriteGuard>,
    ) -> Result<IndexRepairReport> {
        let document_ids = store.list_document_ids(DocumentScope::All).await?;
        let fingerprint = retrieval.index_fingerprint();
        let mut report = IndexRepairReport {
            scanned: document_ids.len(),
            ..IndexRepairReport::default()
        };
        let mut candidates = Vec::new();

        // First invalidate every stale or physically incomplete document. This pass
        // is intentionally separate from embedding so later candidates cannot keep
        // serving incompatible rows while earlier remote calls are in flight.
        for document_id in document_ids {
            let _write = writes.acquire(document_id).await;
            let current = match store.get_document(document_id).await {
                Ok(Some(document)) => document,
                Ok(None) => {
                    report.skipped += 1;
                    continue;
                }
                Err(error) => {
                    report.failures.push(IndexRepairFailure {
                        document_id,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            if let Some(pending) = self.pending_watermarks.get(&current.id).cloned() {
                if pending.revision == current.content_revision
                    && pending.revision_token == current.revision_token
                    && pending.fingerprint == fingerprint
                {
                    match store
                        .mark_document_indexed(
                            current.id,
                            pending.revision,
                            pending.revision_token,
                            &pending.fingerprint,
                            Utc::now(),
                        )
                        .await
                    {
                        Ok(true) => {
                            self.pending_watermarks.remove(&current.id);
                            report.repaired += 1;
                            continue;
                        }
                        Ok(false) => {
                            self.pending_watermarks.remove(&current.id);
                        }
                        Err(error) => {
                            report.failures.push(IndexRepairFailure {
                                document_id: current.id,
                                error: error.to_string(),
                            });
                            continue;
                        }
                    }
                } else {
                    self.pending_watermarks.remove(&current.id);
                }
            }
            let document = canonical_document(&current);
            let watermark_matches = current.indexed_revision == Some(current.content_revision)
                && current.index_fingerprint.as_deref() == Some(fingerprint.as_str());
            if watermark_matches {
                match retrieval.index_is_complete(&document).await {
                    Ok(true) => {
                        report.skipped += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        report.failures.push(IndexRepairFailure {
                            document_id: current.id,
                            error: error.to_string(),
                        });
                        continue;
                    }
                }
            }
            match store
                .clear_document_index(current.id, current.content_revision, current.revision_token)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    report.failures.push(IndexRepairFailure {
                        document_id: current.id,
                        error: "document changed before repair invalidation committed".into(),
                    });
                    continue;
                }
                Err(error) => {
                    report.failures.push(IndexRepairFailure {
                        document_id: current.id,
                        error: error.to_string(),
                    });
                    continue;
                }
            }
            if let Err(error) = retrieval.delete(current.id).await {
                report.failures.push(IndexRepairFailure {
                    document_id: current.id,
                    error: error.to_string(),
                });
                continue;
            }
            candidates.push(current.id);
        }

        // Then rebuild invalidated candidates. Re-fetching under the keyed lock lets
        // a live ingest/delete that won between passes supersede this snapshot.
        for document_id in candidates {
            let _write = writes.acquire(document_id).await;
            let current = match store.get_document(document_id).await {
                Ok(Some(document)) => document,
                Ok(None) => {
                    report.skipped += 1;
                    continue;
                }
                Err(error) => {
                    report.failures.push(IndexRepairFailure {
                        document_id,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let document = canonical_document(&current);
            if current.indexed_revision == Some(current.content_revision)
                && current.index_fingerprint.as_deref() == Some(fingerprint.as_str())
            {
                match retrieval.index_is_complete(&document).await {
                    Ok(true) => {
                        report.skipped += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        report.failures.push(IndexRepairFailure {
                            document_id: current.id,
                            error: error.to_string(),
                        });
                        continue;
                    }
                }
            }
            if let Err(error) = retrieval.index_document(&document).await {
                report.failures.push(IndexRepairFailure {
                    document_id: current.id,
                    error: error.to_string(),
                });
                continue;
            }

            self.pending_watermarks.insert(
                current.id,
                PendingWatermark {
                    revision: current.content_revision,
                    revision_token: current.revision_token,
                    fingerprint: fingerprint.clone(),
                },
            );

            match store
                .mark_document_indexed(
                    current.id,
                    current.content_revision,
                    current.revision_token,
                    &fingerprint,
                    Utc::now(),
                )
                .await
            {
                Ok(true) => {
                    self.pending_watermarks.remove(&current.id);
                    report.repaired += 1;
                }
                Ok(false) => {
                    self.pending_watermarks.remove(&current.id);
                    report.failures.push(IndexRepairFailure {
                        document_id: current.id,
                        error: "document changed before its repair watermark committed".into(),
                    });
                }
                Err(error) => report.failures.push(IndexRepairFailure {
                    document_id: current.id,
                    error: error.to_string(),
                }),
            }
        }

        Ok(report)
    }
}

fn canonical_document(record: &DocumentRecord) -> Document {
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
