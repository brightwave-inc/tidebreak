//! Storing an extracted web page as a conversation document.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tidebreak_core::{DocumentId, SessionId};

use crate::web_search::WebExtractResponse;

/// Where one stored extraction landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredExtractedPage {
    pub document_id: DocumentId,
}

/// Why one extracted page could not be kept as a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the extracted page could not be stored as a source")]
pub struct ExtractedPageSinkError;

/// A durable home for extracted pages.
#[async_trait]
pub trait ExtractedPageSink: Send + Sync {
    async fn store_page(
        &self,
        chat_id: SessionId,
        page: &WebExtractResponse,
        fetched_at: DateTime<Utc>,
    ) -> Result<StoredExtractedPage, ExtractedPageSinkError>;
}

/// The model-facing result of one extraction that became a source.
pub(crate) fn extracted_page_result(page: &WebExtractResponse, document_id: DocumentId) -> String {
    format!(
        "{}Document ID: {document_id}\n\
         This page is now a source in this conversation. Cite it with \
         `:cit[your phrasing]{{doc={document_id}}}` or name its URL in prose.\n\n{}\n",
        extraction_header(page),
        page.content
    )
}

/// The same result when the page could not be kept as a source.
pub(crate) fn uncitable_page_result(page: &WebExtractResponse) -> String {
    format!(
        "{}This page was not kept as a source in this conversation, so attribute it \
         by naming its URL.\n\n{}\n",
        extraction_header(page),
        page.content
    )
}

fn extraction_header(page: &WebExtractResponse) -> String {
    let title = if page.title.is_empty() {
        "Untitled page"
    } else {
        page.title.as_str()
    };
    let words = if page.truncated {
        format!("{} words, shortened to fit", page.word_count)
    } else {
        format!("{} words", page.word_count)
    };
    format!(
        "Fetched page: {title}\nURL: {}\nExtracted by: {}\nLength: {words}\n",
        page.url, page.extraction_method
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_search::ExtractionMethod;

    #[test]
    fn stored_page_result_teaches_a_direct_document_citation() {
        let page = WebExtractResponse::new(
            ExtractionMethod::Native,
            "https://example.com/article",
            "Ownership Explained",
            "Useful content",
            2,
            false,
        )
        .unwrap();
        let document_id = DocumentId::new();
        let result = extracted_page_result(&page, document_id);
        assert!(result.contains(&format!("doc={document_id}")));
        assert!(result.contains("https://example.com/article"));
        assert!(result.contains("Useful content"));
    }
}
