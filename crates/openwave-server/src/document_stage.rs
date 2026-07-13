//! Select the semantic pipeline stage currently owning one document generation.

use openwave_core::{DocumentJobKind, DocumentRecord};
use openwave_retrieval::{Result, Retriever};

/// Maximum attempts assigned to newly queued or explicitly retried parse jobs.
pub(crate) const MAX_PARSE_ATTEMPTS: i32 = 3;

/// The exact semantic job configuration eligible for explicit retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentJobSpec {
    pub kind: DocumentJobKind,
    pub pipeline_fingerprint: String,
    pub max_attempts: i32,
}

/// Select Parse only while retained bytes are still awaiting canonical output;
/// otherwise select Index. Parser upgrades require a separate atomic reparse
/// transition rather than reviving a failed job in the current generation.
pub(crate) fn retry_document_job_spec(
    document: &DocumentRecord,
    retrieval: &Retriever,
) -> Result<DocumentJobSpec> {
    if document.source_blob.is_some() && document.canonical_fingerprint.is_none() {
        let fingerprint = retrieval.canonical_fingerprint_for(&document.media_type)?;
        return Ok(DocumentJobSpec {
            kind: DocumentJobKind::Parse,
            pipeline_fingerprint: fingerprint,
            max_attempts: MAX_PARSE_ATTEMPTS,
        });
    }

    Ok(DocumentJobSpec {
        kind: DocumentJobKind::Index,
        pipeline_fingerprint: retrieval.index_fingerprint(),
        max_attempts: crate::document_worker::MAX_INDEX_ATTEMPTS,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use openwave_core::{DocumentId, DocumentProcessingStatus, DocumentSourceBlob};
    use openwave_retrieval::{HashEmbedder, InMemoryVectorStore, PlainTextParser, TextChunker};

    use super::*;

    fn retrieval() -> Retriever {
        Retriever::new(
            Box::new(PlainTextParser),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        )
    }

    fn record(
        source_blob: Option<DocumentSourceBlob>,
        canonical_fingerprint: Option<&str>,
    ) -> DocumentRecord {
        let now = chrono::Utc::now();
        DocumentRecord {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            source_blob,
            canonical_text: "canonical".into(),
            canonical_fingerprint: canonical_fingerprint.map(str::to_string),
            source_regions: Vec::new(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: DocumentProcessingStatus::Failed,
            indexed_revision: None,
            index_fingerprint: None,
            created_at: now,
            updated_at: now,
            indexed_at: None,
        }
    }

    fn blob() -> DocumentSourceBlob {
        DocumentSourceBlob {
            id: uuid::Uuid::new_v4(),
            sha256: [0x66; 32],
            byte_len: 128,
        }
    }

    #[test]
    fn retained_source_selects_parse_only_while_canonical_output_is_pending() {
        let retrieval = retrieval();
        let desired = retry_document_job_spec(&record(Some(blob()), None), &retrieval).unwrap();
        assert_eq!(desired.kind, DocumentJobKind::Parse);
        assert_eq!(desired.pipeline_fingerprint, "plain-text-lossy-v1");
        assert_eq!(desired.max_attempts, MAX_PARSE_ATTEMPTS);

        for canonical_fingerprint in ["plain-text-lossy-v1", "older-parser"] {
            let desired = retry_document_job_spec(
                &record(Some(blob()), Some(canonical_fingerprint)),
                &retrieval,
            )
            .unwrap();
            assert_eq!(desired.kind, DocumentJobKind::Index);
            assert_eq!(
                desired.max_attempts,
                crate::document_worker::MAX_INDEX_ATTEMPTS
            );
        }
    }

    #[test]
    fn canonical_only_document_selects_index_without_parser_provenance() {
        let desired = retry_document_job_spec(&record(None, None), &retrieval()).unwrap();
        assert_eq!(desired.kind, DocumentJobKind::Index);
    }

    #[test]
    fn retained_source_requires_a_parser_for_its_media_type() {
        let mut document = record(Some(blob()), None);
        document.media_type = "application/pdf".into();
        assert!(retry_document_job_spec(&document, &retrieval()).is_err());
    }
}
