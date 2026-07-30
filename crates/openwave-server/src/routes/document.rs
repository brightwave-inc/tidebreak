//! Document ingestion, catalog, and lifecycle HTTP handlers.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use unicode_general_category::{get_general_category, GeneralCategory};

use openwave_core::{
    AgentError, ChatId, DocumentId, DocumentListCursor, DocumentRecord, DocumentScope,
    DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, ProjectId, SourceReadiness,
};

use crate::document_decode::decode_document;
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

/// Query parameters for a raw source whose request body is streamed into blob
/// storage. The native bridge computes the descriptor while it securely reads
/// the selected file; the server verifies it while writing.
#[derive(Debug, Deserialize)]
pub struct StreamedRawDocumentQuery {
    /// Optional human-facing title. This is metadata only.
    pub title: Option<String>,
    /// Exact SHA-256 digest of the body, encoded as 64 lowercase or uppercase
    /// hexadecimal characters.
    pub sha256: String,
    /// Exact body length in bytes.
    pub byte_len: u64,
}

/// Result of `POST /documents`.
#[derive(Debug, Serialize)]
pub struct IngestResult {
    /// The ingested document's id (derived from the URI when one is given).
    pub document_id: DocumentId,
    /// Whether the stored source has canonical text.
    pub readiness: SourceReadiness,
}

/// Result of streamed native-file ingestion.
#[derive(Debug, Serialize)]
pub struct StreamedIngestResult {
    /// The content-derived id in this conversation.
    pub document_id: DocumentId,
    /// Whether the stored source has canonical text.
    pub readiness: SourceReadiness,
    /// Whether this request found an existing source with the same immutable
    /// content rather than creating a second catalog record.
    pub already_present: bool,
}

/// Catalog metadata returned by document listings.
#[derive(Debug, Serialize)]
pub struct DocumentSummary {
    /// Stable identifier shared with citations and delete/get routes.
    pub document_id: DocumentId,
    /// Owning conversation for conversation-scoped sources.
    pub chat_id: Option<ChatId>,
    /// Owning project, or `None` for a legacy unscoped source.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for inline content.
    pub uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Exact retained source byte length, when original bytes are available.
    pub source_byte_len: Option<u64>,
    /// Whether the canonical text can be read as a source.
    pub readable: bool,
    /// When this document was first created.
    pub created_at: chrono::DateTime<Utc>,
    /// When its authoritative source last changed.
    pub updated_at: chrono::DateTime<Utc>,
}

impl From<DocumentSummaryRecord> for DocumentSummary {
    fn from(document: DocumentSummaryRecord) -> Self {
        Self {
            document_id: document.id,
            chat_id: document.chat_id,
            project_id: document.project_id,
            uri: document.source_uri,
            media_type: document.media_type,
            title: document.title,
            source_byte_len: document.source_byte_len,
            readable: document.readable,
            created_at: document.created_at,
            updated_at: document.updated_at,
        }
    }
}

impl From<&DocumentRecord> for DocumentSummary {
    fn from(document: &DocumentRecord) -> Self {
        Self {
            document_id: document.id,
            chat_id: document.chat_id,
            project_id: document.project_id,
            uri: document.source_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            source_byte_len: document.source_blob.as_ref().map(|source| source.byte_len),
            readable: document.is_readable(),
            created_at: document.created_at,
            updated_at: document.updated_at,
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

/// Full document response, including its canonical text.
#[derive(Debug, Serialize)]
pub struct DocumentDetail {
    /// Catalog metadata.
    #[serde(flatten)]
    pub summary: DocumentSummary,
    /// Parsed text-of-record returned by source reads.
    pub content: String,
}

/// One source as the renderer may see it.
///
/// Deliberately narrower than [`DocumentDetail`], which flattens the full
/// catalog summary. That summary carries `uri` — for a conversation source
/// either absent or an opaque `connected-folder:{root_id}/{path}` reference, but
/// for an unscoped source a real filesystem path. None of it is the renderer's,
/// and a projection is a guarantee where "it happens to be empty today" is not.
///
/// Adding a field here puts it on an untrusted surface. Nothing derived from a
/// host path belongs in one.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct ChatDocumentDetail {
    /// Stable identifier shared with citations and the file-content route.
    pub document_id: DocumentId,
    /// Media type of the canonical content, which selects the viewer.
    pub media_type: String,
    pub title: Option<String>,
    /// Whether the canonical text is readable as a source.
    pub readable: bool,
    /// Whether this source retained the bytes it was made from, and so whether
    /// the file-content route has anything to serve.
    ///
    /// Not every source has an original. A fetched web page is stored as the
    /// readable text extraction produced; the markup it came from is not kept,
    /// and offering to open "the original" would be offering a view that can
    /// only fail. This is a fact about what exists, not a permission — the route
    /// enforces ownership regardless of what a client believes.
    pub has_original_bytes: bool,
    pub updated_at: chrono::DateTime<Utc>,
    /// Parsed text-of-record returned by source reads.
    pub content: String,
}

impl From<DocumentRecord> for ChatDocumentDetail {
    fn from(document: DocumentRecord) -> Self {
        let has_original_bytes = document.source_blob.is_some();
        let summary = DocumentSummary::from(&document);
        Self {
            document_id: summary.document_id,
            media_type: summary.media_type,
            title: summary.title,
            readable: summary.readable,
            has_original_bytes,
            updated_at: summary.updated_at,
            content: document.canonical_text,
        }
    }
}

impl From<DocumentRecord> for DocumentDetail {
    fn from(document: DocumentRecord) -> Self {
        Self {
            summary: DocumentSummary::from(&document),
            content: document.canonical_text,
        }
    }
}

/// `POST /documents` — decode and durably retain source bytes.
pub async fn ingest_document(
    State(state): State<AppState>,
    Json(body): Json<IngestDocument>,
) -> Result<impl IntoResponse, ServerError> {
    ingest_document_in_scope(&state, None, None, body).await
}

/// `POST /projects/{project_id}/documents` — ingest a document in one project corpus.
pub async fn ingest_project_document(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Json(body): Json<IngestDocument>,
) -> Result<impl IntoResponse, ServerError> {
    require_project(&state, project_id).await?;
    ingest_document_in_scope(&state, None, Some(project_id), body).await
}

/// `POST /chats/{chat_id}/documents` — ingest a source owned by one conversation.
pub async fn ingest_chat_document(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Json(body): Json<IngestDocument>,
) -> Result<impl IntoResponse, ServerError> {
    require_chat(&state, chat_id).await?;
    ingest_document_in_scope(&state, Some(chat_id), None, body).await
}

async fn ingest_document_in_scope(
    state: &AppState,
    chat_id: Option<ChatId>,
    project_id: Option<ProjectId>,
    body: IngestDocument,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    if body.content.trim().is_empty() {
        return Err(ServerError::bad_request("content must not be empty"));
    }
    publish_document_source(
        state,
        chat_id,
        project_id,
        body.uri,
        None,
        body.media_type.unwrap_or_else(|| "text/plain".to_owned()),
        body.content.into_bytes(),
    )
    .await
}

/// `POST /documents/raw` — retain the exact request body under its required
/// `Content-Type` and decode it synchronously.
pub async fn ingest_raw_document(
    State(state): State<AppState>,
    Query(query): Query<RawDocumentQuery>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    ingest_raw_document_in_scope(&state, None, None, query, &headers, bytes.to_vec()).await
}

/// `POST /projects/{project_id}/documents/raw` — retain exact bytes in one
/// project corpus and decode it synchronously.
pub async fn ingest_raw_project_document(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<RawDocumentQuery>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    require_project(&state, project_id).await?;
    ingest_raw_document_in_scope(
        &state,
        None,
        Some(project_id),
        query,
        &headers,
        bytes.to_vec(),
    )
    .await
}

/// `POST /chats/{chat_id}/documents/raw` — retain exact source bytes for one
/// conversation and decode it synchronously.
pub async fn ingest_raw_chat_document(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<RawDocumentQuery>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    require_chat(&state, chat_id).await?;
    ingest_raw_document_in_scope(&state, Some(chat_id), None, query, &headers, bytes.to_vec()).await
}

/// `POST /chats/{chat_id}/documents/raw-stream` — stream one native-selected
/// file into the content-addressed blob store and use its digest as its stable
/// conversation-local identity.
pub async fn ingest_streamed_raw_chat_document(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<StreamedRawDocumentQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ServerError> {
    require_chat(&state, chat_id).await?;
    let source_blob = streamed_source_blob(&query)?;
    let media_type = raw_document_media_type(&headers)?;
    let title = normalize_document_title(query.title.as_deref())?;
    let document_id = DocumentId::derive_for_chat_content(chat_id, source_blob.sha256);

    if let Some(existing) = state
        .store
        .get_document(document_id)
        .await?
        .filter(|document| document.chat_id == Some(chat_id) && document.project_id.is_none())
    {
        if existing.source_blob.as_ref() == Some(&source_blob) {
            return Ok((
                StatusCode::OK,
                Json(StreamedIngestResult {
                    document_id,
                    readiness: SourceReadiness::of(existing.is_readable()),
                    already_present: true,
                }),
            ));
        }
    }

    let stream = body
        .into_data_stream()
        .map(|chunk| {
            chunk
                .map(|chunk| chunk.to_vec())
                .map_err(|error| AgentError::Store(format!("read streamed document body: {error}")))
        })
        .boxed();
    let _blob_write = state.blob_writes.acquire(source_blob.id).await?;
    state.blobs.put_stream(source_blob.clone(), stream).await?;
    let source_bytes = state.blobs.get(source_blob.id).await?.ok_or_else(|| {
        ServerError::internal("streamed document disappeared before synchronous decoding")
    })?;
    let canonical_text = decode_document(&media_type, &source_bytes);
    let document = state
        .store
        .accept_document_source(&DocumentSourceUpsert {
            id: document_id,
            chat_id: Some(chat_id),
            project_id: None,
            source_uri: None,
            media_type,
            title,
            source_blob,
            canonical_text,
            updated_at: Utc::now(),
        })
        .await?;
    state.blob_retirement_wake.notify_one();
    Ok((
        StatusCode::CREATED,
        Json(StreamedIngestResult {
            document_id,
            readiness: SourceReadiness::of(document.is_readable()),
            already_present: false,
        }),
    ))
}

async fn ingest_raw_document_in_scope(
    state: &AppState,
    chat_id: Option<ChatId>,
    project_id: Option<ProjectId>,
    query: RawDocumentQuery,
    headers: &HeaderMap,
    source_bytes: Vec<u8>,
) -> Result<(StatusCode, Json<IngestResult>), ServerError> {
    if source_bytes.is_empty() {
        return Err(ServerError::bad_request("content must not be empty"));
    }
    let media_type = raw_document_media_type(headers)?;
    publish_document_source(
        state,
        chat_id,
        project_id,
        query.uri,
        query.title,
        media_type,
        source_bytes,
    )
    .await
}

async fn publish_document_source(
    state: &AppState,
    chat_id: Option<ChatId>,
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
    let document_id = match (chat_id, project_id, source_uri.as_deref()) {
        (Some(chat_id), None, Some(uri)) => DocumentId::derive_for_chat(chat_id, uri),
        (None, Some(project_id), Some(uri)) => DocumentId::derive_for_project(project_id, uri),
        (None, None, Some(uri)) => DocumentId::derive(uri),
        (_, _, None) => DocumentId::new(),
        (Some(_), Some(_), Some(_)) => {
            return Err(ServerError::internal(
                "document cannot belong to both a conversation and a project",
            ));
        }
    };
    let source_blob = DocumentSourceBlob::from_bytes(&source_bytes);
    let canonical_text = decode_document(&media_type, &source_bytes);

    // Publication intentionally precedes the catalog transaction so an
    // accepted descriptor can never reference missing bytes. A later catalog
    // failure may leave an unreferenced content-addressed blob; it must be
    // reclaimed by a grace-period sweep, not eagerly deleted, because another
    // document may already share the same blob id.
    let _blob_write = state.blob_writes.acquire(source_blob.id).await?;
    state.blobs.put(source_blob.id, source_bytes).await?;
    let document = state
        .store
        .accept_document_source(&DocumentSourceUpsert {
            id: document_id,
            chat_id,
            project_id,
            source_uri,
            media_type,
            title,
            source_blob,
            canonical_text,
            updated_at: Utc::now(),
        })
        .await?;
    state.blob_retirement_wake.notify_one();
    Ok((
        StatusCode::CREATED,
        Json(IngestResult {
            document_id,
            readiness: SourceReadiness::of(document.is_readable()),
        }),
    ))
}

fn raw_document_media_type(headers: &HeaderMap) -> Result<String, ServerError> {
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
    Ok(media_type.to_owned())
}

fn streamed_source_blob(
    query: &StreamedRawDocumentQuery,
) -> Result<DocumentSourceBlob, ServerError> {
    if query.byte_len == 0 {
        return Err(ServerError::bad_request("content must not be empty"));
    }
    let sha256 = decode_sha256(&query.sha256).ok_or_else(|| {
        ServerError::bad_request("sha256 must be a 64-character hexadecimal digest")
    })?;
    Ok(DocumentSourceBlob::from_digest(sha256, query.byte_len))
}

fn decode_sha256(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 || !raw.is_ascii() {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
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

/// `GET /chats/{chat_id}/documents` — list only sources owned by one conversation.
pub async fn list_chat_documents(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<DocumentListQuery>,
) -> Result<Json<DocumentListPage>, ServerError> {
    require_chat(&state, chat_id).await?;
    list_documents_in_scope(&state, DocumentScope::Chat(chat_id), query).await
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
        .filter(|document| document.chat_id.is_none() && document.project_id.is_none())
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
        .filter(|document| document.chat_id.is_none() && document.project_id == Some(project_id))
        .map(DocumentDetail::from)
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("document {document_id} not found")))
}

/// `GET /chats/{chat_id}/documents/{document_id}` — fetch a source only when
/// the path conversation owns it.
pub async fn get_chat_document(
    State(state): State<AppState>,
    Path((chat_id, document_id)): Path<(ChatId, DocumentId)>,
) -> Result<Json<ChatDocumentDetail>, ServerError> {
    require_chat(&state, chat_id).await?;
    state
        .store
        .get_document(document_id)
        .await?
        .filter(|document| document.chat_id == Some(chat_id) && document.project_id.is_none())
        .map(ChatDocumentDetail::from)
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("document {document_id} not found")))
}

/// `GET /documents/{id}/file-content` — serve the original bytes for one
/// explicitly unscoped document.
pub async fn get_document_file_content(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    serve_document_file_content(&state, id, None, None, method, &headers).await
}

/// `GET /projects/{project_id}/documents/{document_id}/file-content` — serve
/// original bytes only when the path project owns the document.
pub async fn get_project_document_file_content(
    State(state): State<AppState>,
    Path((project_id, document_id)): Path<(ProjectId, DocumentId)>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_project(&state, project_id).await?;
    serve_document_file_content(
        &state,
        document_id,
        None,
        Some(project_id),
        method,
        &headers,
    )
    .await
}

/// `GET /chats/{chat_id}/documents/{document_id}/file-content` — serve original
/// bytes only when the path conversation owns the document.
pub async fn get_chat_document_file_content(
    State(state): State<AppState>,
    Path((chat_id, document_id)): Path<(ChatId, DocumentId)>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_chat(&state, chat_id).await?;
    serve_document_file_content(&state, document_id, Some(chat_id), None, method, &headers).await
}

async fn serve_document_file_content(
    state: &AppState,
    document_id: DocumentId,
    chat_id: Option<ChatId>,
    project_id: Option<ProjectId>,
    method: Method,
    headers: &HeaderMap,
) -> Result<Response, ServerError> {
    let Some(document) = state
        .store
        .get_document(document_id)
        .await?
        .filter(|document| document.chat_id == chat_id && document.project_id == project_id)
    else {
        return Err(ServerError::not_found(format!(
            "document {document_id} not found"
        )));
    };
    let Some(source_blob) = document.source_blob else {
        return Err(ServerError::not_found(format!(
            "original bytes for document {document_id} not found"
        )));
    };
    let metadata = state.blobs.metadata(source_blob.id).await?.ok_or_else(|| {
        ServerError::internal(format!(
            "original bytes for document {document_id} are missing from blob storage"
        ))
    })?;
    let actual_len = metadata.byte_len;
    if actual_len != source_blob.byte_len {
        return Err(ServerError::internal(format!(
            "original byte length for document {document_id} does not match its descriptor"
        )));
    }
    let content_type = HeaderValue::from_str(&document.media_type).map_err(|_| {
        ServerError::internal(format!(
            "document {document_id} has an invalid stored media type"
        ))
    })?;

    let requested_range = match requested_byte_range(headers, actual_len) {
        Ok(range) => range,
        Err(()) => return Ok(range_not_satisfiable_response(actual_len)),
    };
    let (status, content_range, range, body_len) = match requested_range {
        Some(range) => {
            let end_exclusive = range.end_inclusive + 1;
            (
                StatusCode::PARTIAL_CONTENT,
                Some(format!(
                    "bytes {}-{}/{}",
                    range.start, range.end_inclusive, actual_len
                )),
                range.start..end_exclusive,
                end_exclusive - range.start,
            )
        }
        None => (StatusCode::OK, None, 0..actual_len, actual_len),
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let stream = state
            .blobs
            .read_range(source_blob.id, range)
            .await?
            .ok_or_else(|| {
                ServerError::internal(format!(
                    "original bytes for document {document_id} are missing from blob storage"
                ))
            })?;
        Body::from_stream(stream)
    };

    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body_len.to_string());
    if let Some(content_range) = content_range {
        response = response.header(header::CONTENT_RANGE, content_range);
    }
    response.body(body).map_err(|error| {
        ServerError::internal(format!("failed to build document response: {error}"))
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end_inclusive: u64,
}

fn requested_byte_range(headers: &HeaderMap, full_len: u64) -> Result<Option<ByteRange>, ()> {
    // This route does not expose an HTTP validator in this slice. Treat
    // conditional ranges as a request for the complete representation so a
    // stale validator cannot select bytes from an outdated representation.
    if headers.contains_key(header::IF_RANGE) {
        return Ok(None);
    }
    let mut values = headers.get_all(header::RANGE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?.trim();
    let (unit, range) = value.split_once('=').ok_or(())?;
    if !unit.trim().eq_ignore_ascii_case("bytes") || range.contains(',') {
        return Err(());
    }
    let (start, end) = range.trim().split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix_len = end.parse::<u64>().map_err(|_| ())?;
        if suffix_len == 0 || full_len == 0 {
            return Err(());
        }
        return Ok(Some(ByteRange {
            start: full_len.saturating_sub(suffix_len),
            end_inclusive: full_len - 1,
        }));
    }

    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= full_len {
        return Err(());
    }
    let end_inclusive = if end.is_empty() {
        full_len - 1
    } else {
        let requested_end = end.parse::<u64>().map_err(|_| ())?;
        if requested_end < start {
            return Err(());
        }
        requested_end.min(full_len - 1)
    };
    Ok(Some(ByteRange {
        start,
        end_inclusive,
    }))
}

fn range_not_satisfiable_response(full_len: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes */{full_len}"))
        .header(header::CONTENT_LENGTH, "0")
        .body(Body::empty())
        .expect("fixed range response headers are valid")
}

#[cfg(test)]
mod byte_range_tests {
    use super::*;

    fn parse(value: &str, full_len: u64) -> Result<Option<ByteRange>, ()> {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_str(value).unwrap());
        requested_byte_range(&headers, full_len)
    }

    #[test]
    fn parses_bounded_open_ended_and_suffix_ranges() {
        assert_eq!(
            parse("bytes=2-5", 10),
            Ok(Some(ByteRange {
                start: 2,
                end_inclusive: 5,
            }))
        );
        assert_eq!(
            parse("bytes=7-", 10),
            Ok(Some(ByteRange {
                start: 7,
                end_inclusive: 9,
            }))
        );
        assert_eq!(
            parse("bytes=-3", 10),
            Ok(Some(ByteRange {
                start: 7,
                end_inclusive: 9,
            }))
        );
        assert_eq!(
            parse("bytes=8-99", 10),
            Ok(Some(ByteRange {
                start: 8,
                end_inclusive: 9,
            }))
        );
        assert_eq!(
            parse("bytes=-99", 10),
            Ok(Some(ByteRange {
                start: 0,
                end_inclusive: 9,
            }))
        );
    }

    #[test]
    fn rejects_invalid_unsatisfiable_and_multi_ranges() {
        for value in [
            "bytes=10-",
            "bytes=5-2",
            "bytes=-0",
            "bytes=",
            "bytes=one-two",
            "items=0-1",
            "bytes=0-1,3-4",
        ] {
            assert_eq!(parse(value, 10), Err(()), "{value}");
        }
        assert_eq!(parse("bytes=0-", 0), Err(()));
    }

    #[test]
    fn ignores_range_when_if_range_cannot_be_validated() {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-5"));
        headers.insert(header::IF_RANGE, HeaderValue::from_static("\"old-etag\""));
        assert_eq!(requested_byte_range(&headers, 10), Ok(None));
    }
}

/// `DELETE /documents/{id}` — delete one authoritative source.
pub async fn delete_document(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
) -> Result<StatusCode, ServerError> {
    if state
        .store
        .get_document(id)
        .await?
        .is_some_and(|document| document.chat_id.is_some() || document.project_id.is_some())
    {
        return Err(ServerError::not_found(format!("document {id} not found")));
    }
    state.store.delete_document(id).await?;
    state.blob_retirement_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /projects/{project_id}/documents/{document_id}` — retire an owned document.
pub async fn delete_project_document(
    State(state): State<AppState>,
    Path((project_id, document_id)): Path<(ProjectId, DocumentId)>,
) -> Result<StatusCode, ServerError> {
    require_project(&state, project_id).await?;
    if state
        .store
        .get_document(document_id)
        .await?
        .is_none_or(|document| {
            document.chat_id.is_some() || document.project_id != Some(project_id)
        })
    {
        return Err(ServerError::not_found(format!(
            "document {document_id} not found"
        )));
    }
    state.store.delete_document(document_id).await?;
    state.blob_retirement_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /chats/{chat_id}/documents/{document_id}` — retire one source owned
/// by a conversation without exposing another conversation's source identity.
pub async fn delete_chat_document(
    State(state): State<AppState>,
    Path((chat_id, document_id)): Path<(ChatId, DocumentId)>,
) -> Result<StatusCode, ServerError> {
    require_chat(&state, chat_id).await?;
    if state
        .store
        .get_document(document_id)
        .await?
        .is_none_or(|document| document.chat_id != Some(chat_id) || document.project_id.is_some())
    {
        return Err(ServerError::not_found(format!(
            "document {document_id} not found"
        )));
    }
    state.store.delete_document(document_id).await?;
    state.blob_retirement_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

async fn require_project(state: &AppState, project_id: ProjectId) -> Result<(), ServerError> {
    if state.store.get_project(project_id).await?.is_none() {
        return Err(ServerError::not_found(format!(
            "project {project_id} not found"
        )));
    }
    Ok(())
}

async fn require_chat(state: &AppState, chat_id: ChatId) -> Result<(), ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    Ok(())
}
