//! Document ingestion, catalog, lifecycle, and search HTTP handlers.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use unicode_general_category::{get_general_category, GeneralCategory};

use openwave_core::{
    DocumentJobId, DocumentListCursor, DocumentRecord, DocumentScope, DocumentSourceBlob,
    DocumentSourceUpsert, DocumentSummaryRecord, ProjectId,
};
use openwave_retrieval::{
    Citation, DocumentId, RetrievalError, SearchScope, SearchTool, MAX_SEARCH_RESULTS,
};

use crate::document_stage::{retry_document_job_spec, MAX_PARSE_ATTEMPTS};
use crate::error::ServerError;
use crate::extract::{Json, Path, Query, RawBytes};
use crate::state::AppState;

/// Body of `POST /documents`.
#[derive(Debug, Deserialize)]
pub struct IngestDocument {
    /// The document's text/content, as a UTF-8 string.
    pub content: String,
    /// Optional source URI, recorded for provenance and used to make re-ingesting
    /// the same location idempotent. Omitted for inline content.
    #[serde(default)]
    pub uri: Option<String>,
    /// Media (MIME) type; defaults to `text/plain` when omitted.
    #[serde(default)]
    pub media_type: Option<String>,
}

/// Query parameters for raw-byte document ingestion.
#[derive(Debug, Default, Deserialize)]
pub struct RawDocumentQuery {
    /// Optional source URI used for provenance and idempotent document identity.
    pub uri: Option<String>,
    /// Optional human-facing title. This is metadata only and never used as a
    /// source locator.
    pub title: Option<String>,
}

/// Result of `POST /documents`.
#[derive(Debug, Serialize)]
pub struct IngestResult {
    /// The ingested document's id (derived from the URI when one is given).
    pub document_id: DocumentId,
    /// Durable semantic job for this exact source generation.
    pub job_id: DocumentJobId,
    /// Monotonic authoritative source revision accepted by the store.
    pub content_revision: i64,
    /// Current asynchronous processing state.
    pub processing_status: openwave_core::DocumentProcessingStatus,
}

/// Catalog metadata returned by document listings.
#[derive(Debug, Serialize)]
pub struct DocumentSummary {
    /// Stable identifier shared with citations and delete/get routes.
    pub document_id: DocumentId,
    /// Owning project, or `None` for the unscoped corpus.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for inline content.
    pub uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Current authoritative source revision.
    pub content_revision: i64,
    /// Processing lifecycle of the current source revision.
    pub processing_status: openwave_core::DocumentProcessingStatus,
    /// Source revision currently represented in the index.
    pub indexed_revision: Option<i64>,
    /// Parser/chunker/embedder identity for the current indexed revision.
    pub index_fingerprint: Option<String>,
    /// When this document was first created.
    pub created_at: chrono::DateTime<Utc>,
    /// When its authoritative source last changed.
    pub updated_at: chrono::DateTime<Utc>,
    /// When the current index watermark was recorded.
    pub indexed_at: Option<chrono::DateTime<Utc>>,
}

impl From<DocumentSummaryRecord> for DocumentSummary {
    fn from(document: DocumentSummaryRecord) -> Self {
        Self {
            document_id: document.id,
            project_id: document.project_id,
            uri: document.source_uri,
            media_type: document.media_type,
            title: document.title,
            content_revision: document.content_revision,
            processing_status: document.processing_status,
            indexed_revision: document.indexed_revision,
            index_fingerprint: document.index_fingerprint,
            created_at: document.created_at,
            updated_at: document.updated_at,
            indexed_at: document.indexed_at,
        }
    }
}

impl From<&DocumentRecord> for DocumentSummary {
    fn from(document: &DocumentRecord) -> Self {
        Self {
            document_id: document.id,
            project_id: document.project_id,
            uri: document.source_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            content_revision: document.content_revision,
            processing_status: document.processing_status,
            indexed_revision: document.indexed_revision,
            index_fingerprint: document.index_fingerprint.clone(),
            created_at: document.created_at,
            updated_at: document.updated_at,
            indexed_at: document.indexed_at,
        }
    }
}

/// Query parameters for bounded document catalog pagination.
#[derive(Debug, Default, Deserialize)]
pub struct DocumentListQuery {
    /// Maximum number of documents to return (defaults to 50, maximum 200).
    pub limit: Option<u64>,
    /// Opaque cursor returned by the previous page.
    pub cursor: Option<String>,
}

/// One bounded page of catalog metadata.
#[derive(Debug, Serialize)]
pub struct DocumentListPage {
    /// Documents in newest-first order.
    pub documents: Vec<DocumentSummary>,
    /// Cursor for the next page, or `None` when this is the final page.
    pub next_cursor: Option<String>,
}

const DEFAULT_DOCUMENT_PAGE_SIZE: u64 = 50;
const MAX_DOCUMENT_PAGE_SIZE: u64 = 200;

fn encode_document_cursor(cursor: DocumentListCursor) -> String {
    format!(
        "{}:{:09}:{}",
        cursor.created_at.timestamp(),
        cursor.created_at.timestamp_subsec_nanos(),
        cursor.id
    )
}

fn decode_document_cursor(raw: &str) -> Result<DocumentListCursor, ServerError> {
    let mut parts = raw.splitn(3, ':');
    let seconds = parts.next().and_then(|part| part.parse::<i64>().ok());
    let nanos = parts.next().and_then(|part| part.parse::<u32>().ok());
    let id = parts.next();
    let created_at = seconds
        .zip(nanos)
        .and_then(|(seconds, nanos)| chrono::DateTime::<Utc>::from_timestamp(seconds, nanos))
        .ok_or_else(|| ServerError::bad_request("invalid document cursor"))?;
    let id = id
        .ok_or_else(|| ServerError::bad_request("invalid document cursor"))?
        .parse()
        .map_err(|_| ServerError::bad_request("invalid document cursor"))?;
    Ok(DocumentListCursor { created_at, id })
}

/// Full document response, including the canonical text used for reindexing.
#[derive(Debug, Serialize)]
pub struct DocumentDetail {
    /// Catalog metadata.
    #[serde(flatten)]
    pub summary: DocumentSummary,
    /// Parsed text-of-record. Citation spans index into this string.
    pub content: String,
}

impl From<DocumentRecord> for DocumentDetail {
    fn from(document: DocumentRecord) -> Self {
        Self {
            summary: DocumentSummary::from(&document),
            content: document.canonical_text,
        }
    }
}

/// Map a retrieval failure to an HTTP error: a parse problem is the caller's
/// (unsupported media type / bad content), everything else is server-side.
fn retrieval_error(err: RetrievalError) -> ServerError {
    match err {
        RetrievalError::Parse(_) => ServerError::bad_request(err.to_string()),
        _ => ServerError::internal(err.to_string()),
    }
}

/// `POST /documents` — durably retain source bytes and enqueue exact-generation
/// asynchronous parsing.
pub async fn ingest_document(
    State(state): State<AppState>,
    Json(body): Json<IngestDocument>,
) -> Result<impl IntoResponse, ServerError> {
    ingest_document_in_scope(&state, None, body).await
}

/// `POST /projects/{project_id}/documents` — enqueue a document in one project corpus.
pub async fn ingest_project_document(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Json(body): Json<IngestDocument>,
) -> Result<impl IntoResponse, ServerError> {
    require_project(&state, project_id).await?;
    ingest_document_in_scope(&state, Some(project_id), body).await
}

async fn ingest_document_in_scope(
    state: &AppState,
    project_id: Option<ProjectId>,
    body: IngestDocument,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    if body.content.trim().is_empty() {
        return Err(ServerError::bad_request("content must not be empty"));
    }
    publish_document_source(
        state,
        project_id,
        body.uri,
        None,
        body.media_type.unwrap_or_else(|| "text/plain".to_owned()),
        body.content.into_bytes(),
    )
    .await
}

/// `POST /documents/raw` — retain the exact request body under its required
/// `Content-Type` and enqueue asynchronous parsing.
pub async fn ingest_raw_document(
    State(state): State<AppState>,
    Query(query): Query<RawDocumentQuery>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    ingest_raw_document_in_scope(&state, None, query, &headers, bytes.to_vec()).await
}

/// `POST /projects/{project_id}/documents/raw` — retain exact bytes in one
/// project corpus and enqueue asynchronous parsing.
pub async fn ingest_raw_project_document(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<RawDocumentQuery>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    require_project(&state, project_id).await?;
    ingest_raw_document_in_scope(&state, Some(project_id), query, &headers, bytes.to_vec()).await
}

async fn ingest_raw_document_in_scope(
    state: &AppState,
    project_id: Option<ProjectId>,
    query: RawDocumentQuery,
    headers: &HeaderMap,
    source_bytes: Vec<u8>,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    if source_bytes.is_empty() {
        return Err(ServerError::bad_request("content must not be empty"));
    }
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .ok_or_else(|| ServerError::bad_request("Content-Type header is required"))?
        .to_str()
        .map_err(|_| ServerError::bad_request("Content-Type header is not valid text"))?
        .trim();
    if media_type.is_empty() {
        return Err(ServerError::bad_request(
            "Content-Type header must not be empty",
        ));
    }
    publish_document_source(
        state,
        project_id,
        query.uri,
        query.title,
        media_type.to_owned(),
        source_bytes,
    )
    .await
}

async fn publish_document_source(
    state: &AppState,
    project_id: Option<ProjectId>,
    source_uri: Option<String>,
    title: Option<String>,
    media_type: String,
    source_bytes: Vec<u8>,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    let source_uri = match source_uri.as_deref().map(str::trim) {
        // Trim before deriving the document id: a padded URI must resolve to the
        // same document as its unpadded form, or idempotent re-ingest breaks.
        Some(uri) if !uri.is_empty() => Some(uri.to_owned()),
        _ => None,
    };
    let title = normalize_document_title(title.as_deref())?;
    let parser_fingerprint = state
        .retrieval
        .canonical_fingerprint_for(&media_type)
        .map_err(retrieval_error)?;
    let document_id = match (project_id, source_uri.as_deref()) {
        (Some(project_id), Some(uri)) => DocumentId::derive_for_project(project_id, uri),
        (None, Some(uri)) => DocumentId::derive(uri),
        (_, None) => DocumentId::new(),
    };
    let source_blob = DocumentSourceBlob::from_bytes(&source_bytes);
    // Serialize the entire source publication boundary for one document. If
    // the first accepted request blocks on blob I/O, a later request cannot
    // commit its descriptor and then be overwritten by the older source.
    let _document_write = state.document_writes.acquire(document_id).await;
    // Publication intentionally precedes the catalog transaction so an
    // accepted descriptor can never reference missing bytes. A later catalog
    // failure may leave an unreferenced content-addressed blob; it must be
    // reclaimed by a grace-period sweep, not eagerly deleted, because another
    // document may already share the same blob id.
    let _blob_write = state.blob_writes.acquire(source_blob.id).await?;
    state.blobs.put(source_blob.id, source_bytes).await?;

    let (revision, job) = state
        .store
        .accept_document_source_and_enqueue_parse(
            &DocumentSourceUpsert {
                id: document_id,
                project_id,
                source_uri,
                media_type,
                title,
                source_blob,
                updated_at: Utc::now(),
            },
            &parser_fingerprint,
            MAX_PARSE_ATTEMPTS,
        )
        .await?;
    state.document_job_wake.notify_one();
    state.blob_retirement_wake.notify_one();
    Ok((
        StatusCode::ACCEPTED,
        Json(IngestResult {
            document_id,
            job_id: job.id,
            content_revision: revision.content_revision,
            processing_status: revision.processing_status,
        }),
    ))
}

fn is_safe_document_title_char(character: char) -> bool {
    !matches!(
        get_general_category(character),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

fn normalize_document_title(title: Option<&str>) -> Result<Option<String>, ServerError> {
    let Some(raw_title) = title else {
        return Ok(None);
    };
    if raw_title
        .chars()
        .any(|character| !is_safe_document_title_char(character))
    {
        return Err(ServerError::bad_request(
            "document title contains unsupported control characters",
        ));
    }
    let title = raw_title.trim();
    if title.is_empty() {
        return Ok(None);
    }
    if title.chars().count() > 255 {
        return Err(ServerError::bad_request(
            "document title must be at most 255 characters",
        ));
    }
    Ok(Some(title.to_owned()))
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn renderer_titles_reject_all_visual_control_categories() {
        for unsafe_character in [
            '\u{0000}', '\u{200d}', '\u{206a}', '\u{206f}', '\u{2028}', '\u{2029}',
        ] {
            assert!(
                normalize_document_title(Some(&format!("report{unsafe_character}.md"))).is_err()
            );
        }
        let unicode_title = format!("{}.md", "😀".repeat(252));
        assert_eq!(unicode_title.chars().count(), 255);
        let normalized = normalize_document_title(Some(&unicode_title));
        assert!(normalized.is_ok());
        assert_eq!(normalized.ok().flatten(), Some(unicode_title));
    }
}

/// `POST /documents/{id}/retry` — explicitly revive the current exact failed
/// semantic job without mutating source content or its generation.
pub async fn retry_document(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
) -> Result<impl IntoResponse, ServerError> {
    retry_document_in_scope(&state, id, None).await
}

/// `POST /projects/{project_id}/documents/{document_id}/retry` — revive an owned
/// document's current exact failed semantic job.
pub async fn retry_project_document(
    State(state): State<AppState>,
    Path((project_id, document_id)): Path<(ProjectId, DocumentId)>,
) -> Result<impl IntoResponse, ServerError> {
    require_project(&state, project_id).await?;
    retry_document_in_scope(&state, document_id, Some(project_id)).await
}

async fn retry_document_in_scope(
    state: &AppState,
    id: DocumentId,
    project_id: Option<ProjectId>,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    let _document_write = state.document_writes.acquire(id).await;
    let Some(document) = state
        .store
        .get_document(id)
        .await?
        .filter(|document| document.project_id == project_id)
    else {
        return Err(ServerError::not_found(format!("document {id} not found")));
    };
    let desired = retry_document_job_spec(&document, &state.retrieval).map_err(retrieval_error)?;
    let Some(job) = state
        .store
        .retry_document_job(
            id,
            document.generation(),
            desired.kind,
            &desired.pipeline_fingerprint,
            desired.max_attempts,
        )
        .await?
    else {
        return Err(ServerError::conflict(format!(
            "document {id} has no retryable failed job for the active pipeline"
        )));
    };
    let record = state
        .store
        .get_document(id)
        .await?
        .filter(|record| record.project_id == project_id && record.generation() == job.generation())
        .ok_or_else(|| {
            ServerError::internal(format!(
                "retried job {} no longer matches document {id}",
                job.id
            ))
        })?;
    state.document_job_wake.notify_one();
    Ok((
        StatusCode::ACCEPTED,
        Json(IngestResult {
            document_id: id,
            job_id: job.id,
            content_revision: record.content_revision,
            processing_status: record.processing_status,
        }),
    ))
}

/// `GET /documents` — list the explicitly unscoped corpus without returning each
/// document's potentially large canonical text. Project-scoped listing lands with
/// corpus scoping; this endpoint never widens to every project's documents.
pub async fn list_documents(
    State(state): State<AppState>,
    Query(query): Query<DocumentListQuery>,
) -> Result<Json<DocumentListPage>, ServerError> {
    list_documents_in_scope(&state, DocumentScope::Unscoped, query).await
}

/// `GET /projects/{project_id}/documents` — list one project's document corpus.
pub async fn list_project_documents(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<DocumentListQuery>,
) -> Result<Json<DocumentListPage>, ServerError> {
    require_project(&state, project_id).await?;
    list_documents_in_scope(&state, DocumentScope::Project(project_id), query).await
}

async fn list_documents_in_scope(
    state: &AppState,
    scope: DocumentScope,
    query: DocumentListQuery,
) -> Result<Json<DocumentListPage>, ServerError> {
    let limit = query.limit.unwrap_or(DEFAULT_DOCUMENT_PAGE_SIZE);
    if limit == 0 || limit > MAX_DOCUMENT_PAGE_SIZE {
        return Err(ServerError::bad_request(format!(
            "document limit must be between 1 and {MAX_DOCUMENT_PAGE_SIZE}"
        )));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(decode_document_cursor)
        .transpose()?;
    let mut records = state
        .store
        .list_document_summaries(scope, cursor, limit + 1)
        .await?;
    let has_more = records.len() > limit as usize;
    records.truncate(limit as usize);
    let next_cursor = has_more.then(|| {
        let last = records
            .last()
            .expect("a page with another row has at least one returned row");
        encode_document_cursor(DocumentListCursor {
            created_at: last.created_at,
            id: last.id,
        })
    });
    Ok(Json(DocumentListPage {
        documents: records.into_iter().map(DocumentSummary::from).collect(),
        next_cursor,
    }))
}

/// `GET /documents/{id}` — fetch canonical source and catalog metadata, or `404`.
pub async fn get_document(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
) -> Result<Json<DocumentDetail>, ServerError> {
    state
        .store
        .get_document(id)
        .await?
        .filter(|document| document.project_id.is_none())
        .map(DocumentDetail::from)
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("document {id} not found")))
}

/// `GET /projects/{project_id}/documents/{document_id}` — fetch an owned document.
pub async fn get_project_document(
    State(state): State<AppState>,
    Path((project_id, document_id)): Path<(ProjectId, DocumentId)>,
) -> Result<Json<DocumentDetail>, ServerError> {
    require_project(&state, project_id).await?;
    state
        .store
        .get_document(document_id)
        .await?
        .filter(|document| document.project_id == Some(project_id))
        .map(DocumentDetail::from)
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("document {document_id} not found")))
}

/// `DELETE /documents/{id}` — atomically delete authoritative source/jobs and
/// persist a pending empty tombstone. The durable worker publishes it.
pub async fn delete_document(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
) -> Result<StatusCode, ServerError> {
    let _document_write = state.document_writes.acquire(id).await;
    if state
        .store
        .get_document(id)
        .await?
        .is_some_and(|document| document.project_id.is_some())
    {
        return Err(ServerError::not_found(format!("document {id} not found")));
    }
    state.store.delete_document(id).await?;
    state.document_job_wake.notify_one();
    state.blob_retirement_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /projects/{project_id}/documents/{document_id}` — retire an owned document.
pub async fn delete_project_document(
    State(state): State<AppState>,
    Path((project_id, document_id)): Path<(ProjectId, DocumentId)>,
) -> Result<StatusCode, ServerError> {
    require_project(&state, project_id).await?;
    let _document_write = state.document_writes.acquire(document_id).await;
    if state
        .store
        .get_document(document_id)
        .await?
        .is_none_or(|document| document.project_id != Some(project_id))
    {
        return Err(ServerError::not_found(format!(
            "document {document_id} not found"
        )));
    }
    state.store.delete_document(document_id).await?;
    state.document_job_wake.notify_one();
    state.blob_retirement_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// Body of `POST /search`.
#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    /// The natural-language query.
    pub query: String,
    /// How many passages to return (optional; clamped to `[1, MAX_SEARCH_RESULTS]`).
    #[serde(default)]
    pub k: Option<usize>,
}

/// Result of `POST /search`.
#[derive(Debug, Serialize)]
pub struct SearchResults {
    /// Ranked citations, most relevant first.
    pub citations: Vec<Citation>,
}

/// `POST /search` — search the shared index and return grounded citations. This
/// is the direct HTTP counterpart to the agent's `search` tool; both read the
/// same index. `k` defaults to [`SearchTool::DEFAULT_K`] and is clamped, never
/// rejected.
pub async fn search_documents(
    State(state): State<AppState>,
    Json(body): Json<SearchRequest>,
) -> Result<Json<SearchResults>, ServerError> {
    search_documents_in_scope(&state, SearchScope::Unscoped, body).await
}

/// `POST /projects/{project_id}/search` — search exactly one project corpus.
pub async fn search_project_documents(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Json(body): Json<SearchRequest>,
) -> Result<Json<SearchResults>, ServerError> {
    require_project(&state, project_id).await?;
    search_documents_in_scope(&state, SearchScope::Project(project_id), body).await
}

async fn search_documents_in_scope(
    state: &AppState,
    scope: SearchScope,
    body: SearchRequest,
) -> Result<Json<SearchResults>, ServerError> {
    let query = body.query.trim();
    if query.is_empty() {
        return Err(ServerError::bad_request("query must not be empty"));
    }
    let k = body
        .k
        .unwrap_or(SearchTool::DEFAULT_K)
        .clamp(1, MAX_SEARCH_RESULTS);
    let citations = state
        .retrieval
        .search(scope, query, k)
        .await
        .map_err(retrieval_error)?;
    Ok(Json(SearchResults { citations }))
}

async fn require_project(state: &AppState, project_id: ProjectId) -> Result<(), ServerError> {
    if state.store.get_project(project_id).await?.is_none() {
        return Err(ServerError::not_found(format!(
            "project {project_id} not found"
        )));
    }
    Ok(())
}
