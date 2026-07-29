//! Select the parse job eligible for explicit retry.

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

/// Select Parse only while retained bytes are awaiting canonical output.
pub(crate) fn retry_document_job_spec(
    document: &DocumentRecord,
    retrieval: &Retriever,
) -> Result<DocumentJobSpec> {
    if document.source_blob.is_none() || document.canonical_fingerprint.is_some() {
        return Err(openwave_retrieval::RetrievalError::msg(
            "document has no pending parse stage",
        ));
    }
    Ok(DocumentJobSpec {
        kind: DocumentJobKind::Parse,
        pipeline_fingerprint: retrieval.canonical_fingerprint_for(&document.media_type)?,
        max_attempts: MAX_PARSE_ATTEMPTS,
    })
}
