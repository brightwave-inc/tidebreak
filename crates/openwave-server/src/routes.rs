//! Chat HTTP handlers.
//!
//! The conversation CRUD surface (create / list / get), posting a message to
//! start a turn, and the WebSocket stream of a chat's turn events.

use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use openwave_core::{
    Agent, ApprovalDecision, CallId, Chat, ChatId, DocumentJobId, DocumentListCursor,
    DocumentRecord, DocumentScope, DocumentSummaryRecord, DocumentUpsert, Project, ProjectId,
    SecretProvider, SequencedEvent, Store,
};
use openwave_retrieval::{
    Citation, DocumentId, DocumentSource, RetrievalError, SearchScope, SearchTool,
    MAX_SEARCH_RESULTS,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::document_worker::MAX_INDEX_ATTEMPTS;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;

/// The store-settings key for the selected model.
const MODEL_SETTING: &str = "model";

/// Runtime settings a client can read. The API key itself is never returned —
/// it lives in the `SecretProvider`, not the store — only whether one is set.
#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    /// The model turns run against, or `None` to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether a model API key is configured (never the key itself).
    pub has_api_key: bool,
}

/// Body of `PUT /settings`. Each field is a *double* option so an absent key is
/// distinguished from an explicit `null`: absent leaves the value unchanged,
/// `null` resets it to the server default, and a value sets it.
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`; `#[serde(default)]`
/// supplies `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// `GET /settings` — the current runtime settings.
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<Settings>, ServerError> {
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
    }))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are touched.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<SettingsUpdate>,
) -> Result<Json<Settings>, ServerError> {
    match body.model {
        // Absent: leave the model unchanged.
        None => {}
        // Explicit null: reset to the server default (stored as JSON null, which
        // `read_model` reads back as "unset").
        Some(None) => {
            state
                .store
                .set_setting(MODEL_SETTING, &serde_json::Value::Null)
                .await?;
        }
        // A value: reject empty (it would break every turn), else set it.
        Some(Some(model)) => {
            if model.is_empty() {
                return Err(ServerError::bad_request("model must not be empty"));
            }
            state
                .store
                .set_setting(MODEL_SETTING, &serde_json::json!(model))
                .await?;
        }
    }
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
    }))
}

/// The configured model, if any.
async fn read_model(store: &dyn Store) -> Result<Option<String>, ServerError> {
    Ok(store
        .get_setting(MODEL_SETTING)
        .await?
        .and_then(|value| value.as_str().map(str::to_owned)))
}

/// Whether any model provider credential is configured — stored or via the
/// env fallbacks the resolver also honors. Prefer `GET /providers` for
/// per-kind detail; this field is the legacy "is anything ready?" signal.
async fn has_api_key(secrets: &dyn SecretProvider) -> bool {
    for &kind in ProviderKind::ALL {
        if providers::has_credential(secrets, kind).await {
            return true;
        }
    }
    false
}

/// Body of `PUT /settings/api-key`.
///
/// Legacy shim: writes the Anthropic credential in the typed blob shape and
/// enables the Anthropic provider. Prefer `PUT /providers/anthropic`.
#[derive(Deserialize)]
pub struct ApiKey {
    /// The provider API key to store. Written to the `SecretProvider` (the OS
    /// keychain on desktop), never to the database, and never read back out.
    pub api_key: String,
}

// Redact the key so it can't leak through a `{:?}`.
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey").field("api_key", &"***").finish()
    }
}

/// `PUT /settings/api-key` — store the Anthropic API key. `204 No Content`.
pub async fn put_api_key(
    State(state): State<AppState>,
    Json(body): Json<ApiKey>,
) -> Result<StatusCode, ServerError> {
    if body.api_key.is_empty() {
        return Err(ServerError::bad_request("api_key must not be empty"));
    }
    // Write the typed credential and enable Anthropic so the new providers
    // surface and the legacy shim stay equivalent.
    providers::write_credential(
        &*state.secrets,
        ProviderKind::Anthropic,
        &ProviderCredential::api_key(&body.api_key),
    )
    .await?;
    let mut config = providers::read_config(&*state.store, ProviderKind::Anthropic).await?;
    config.enabled = true;
    providers::write_config(&*state.store, ProviderKind::Anthropic, &config).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /settings/api-key` — remove the stored Anthropic API key. `204`.
///
/// Clears only the stored key. If the daemon was launched with an
/// `ANTHROPIC_API_KEY` in its environment, that fallback still applies — so
/// `has_api_key` may stay `true` and turns keep resolving a provider after a
/// delete. The environment is a deploy-time default the API doesn't override.
pub async fn delete_api_key(State(state): State<AppState>) -> Result<StatusCode, ServerError> {
    providers::delete_credential(&*state.secrets, ProviderKind::Anthropic).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Response for `GET /providers`.
#[derive(Debug, Serialize)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

/// `GET /providers` — every known provider kind and its current config.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersList>, ServerError> {
    Ok(Json(ProvidersList {
        providers: providers::list_providers(&*state.store, &*state.secrets).await?,
    }))
}

/// `PUT /providers/{kind}` — update a provider's config and/or credential.
pub async fn put_provider(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(body): Json<ProviderUpdate>,
) -> Result<Json<ProviderInfo>, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    let info = providers::update_provider(&*state.store, &*state.secrets, kind, body).await?;
    Ok(Json(info))
}

/// `DELETE /providers/{kind}/credential` — remove the stored credential. `204`.
pub async fn delete_provider_credential(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<StatusCode, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    providers::delete_credential(&*state.secrets, kind).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A selectable model in the catalog.
#[derive(Debug, Serialize)]
pub struct ModelInfo {
    /// The identifier passed to the provider and stored as `chat.model`.
    pub id: String,
    /// The provider that serves the model.
    pub provider: String,
}

/// Response for `GET /models`.
#[derive(Debug, Serialize)]
pub struct ModelCatalog {
    /// The models a client can select from.
    pub models: Vec<ModelInfo>,
}

/// `GET /models` — the catalog a chat's model selector chooses from.
///
/// Models of enabled, credentialed providers. Falls back to Anthropic's curated
/// list when nothing is configured yet so the selector isn't empty on first run.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<ModelCatalog>, ServerError> {
    let models = providers::catalog_models(&*state.store, &*state.secrets)
        .await?
        .into_iter()
        .map(|(kind, id)| ModelInfo {
            id: id.to_string(),
            provider: kind.as_str().to_string(),
        })
        .collect();
    Ok(Json(ModelCatalog { models }))
}

/// Body of `POST /projects`.
#[derive(Debug, Deserialize)]
pub struct CreateProject {
    /// Absolute path to the project's workspace/corpus root.
    pub workspace_dir: PathBuf,
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    if !body.workspace_dir.is_absolute() {
        return Err(ServerError::bad_request(format!(
            "workspace_dir must be an absolute path, got {:?}",
            body.workspace_dir
        )));
    }
    let project = Project {
        id: ProjectId::new(),
        title: body.title,
        workspace_dir: body.workspace_dir,
        created_at: Utc::now(),
    };
    state.store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /projects` — list projects, most-recently-created first.
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, ServerError> {
    Ok(Json(state.store.list_projects().await?))
}

/// `GET /projects/{id}` — fetch one project, or `404`.
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
) -> Result<Json<Project>, ServerError> {
    state
        .store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

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

/// Result of `POST /documents`.
#[derive(Debug, Serialize)]
pub struct IngestResult {
    /// The ingested document's id (derived from the URI when one is given).
    pub document_id: DocumentId,
    /// Durable job that will index this exact source generation.
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

/// `POST /documents` — atomically persist canonical source content and enqueue an
/// exact-generation index job. Parsing remains synchronous validation; embedding
/// and vector publication run durably in the background.
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
    let source = match body.uri.as_deref().map(str::trim) {
        // Trim before deriving the document id: a padded URI must resolve to the
        // same document as its unpadded form, or idempotent re-ingest breaks.
        Some(uri) if !uri.is_empty() => DocumentSource::uri(uri),
        _ => DocumentSource::Inline,
    };
    let media_type = body.media_type.as_deref().unwrap_or("text/plain");
    // Pipeline components promise a stable fingerprint for their lifetime. Take
    // one snapshot and use it for the exact parse/index operation below.
    let index_fingerprint = state.retrieval.index_fingerprint();
    let mut document = state
        .retrieval
        .parse_document(source, media_type, body.content.as_bytes())
        .await
        .map_err(retrieval_error)?;
    let source_uri = match &document.source {
        DocumentSource::Uri { uri } => Some(uri.clone()),
        DocumentSource::Inline => None,
        _ => None,
    };
    if let Some(project_id) = project_id {
        document.id = source_uri
            .as_deref()
            .map(|uri| DocumentId::derive_for_project(project_id, uri))
            .unwrap_or(document.id);
        document.project_id = Some(project_id);
    }
    let _document_write = state.document_writes.acquire(document.id).await;
    let (revision, job) = state
        .store
        .upsert_document_and_enqueue_index(
            &DocumentUpsert {
                id: document.id,
                project_id,
                source_uri,
                media_type: document.media_type.clone(),
                title: None,
                canonical_text: document.text.clone(),
                source_regions: document.source_regions.clone(),
                updated_at: Utc::now(),
            },
            &index_fingerprint,
            MAX_INDEX_ATTEMPTS,
        )
        .await?;
    state.document_job_wake.notify_one();
    Ok((
        StatusCode::ACCEPTED,
        Json(IngestResult {
            document_id: document.id,
            job_id: job.id,
            content_revision: revision.content_revision,
            processing_status: revision.processing_status,
        }),
    ))
}

/// `POST /documents/{id}/retry` — explicitly revive the current exact failed
/// index job without mutating canonical source content or its generation.
pub async fn retry_document(
    State(state): State<AppState>,
    Path(id): Path<DocumentId>,
) -> Result<impl IntoResponse, ServerError> {
    retry_document_in_scope(&state, id, None).await
}

/// `POST /projects/{project_id}/documents/{document_id}/retry` — revive an owned
/// document's current exact failed index job.
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
    let fingerprint = state.retrieval.index_fingerprint();
    let Some(job) = state
        .store
        .retry_document_job(
            id,
            document.generation(),
            openwave_core::DocumentJobKind::Index,
            &fingerprint,
            MAX_INDEX_ATTEMPTS,
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

/// Body of `POST /chats`.
#[derive(Debug, Deserialize)]
pub struct CreateChat {
    /// Absolute path to the workspace directory the agent operates in.
    pub workspace_dir: PathBuf,
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional project to file this chat under; omitted for a loose chat.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Optional model for this chat; omitted to use the configured default.
    #[serde(default)]
    pub model: Option<String>,
}

/// `POST /chats` — create a chat and return it (`201 Created`).
pub async fn create_chat(
    State(state): State<AppState>,
    Json(body): Json<CreateChat>,
) -> Result<impl IntoResponse, ServerError> {
    // The workspace path must be absolute: a relative one is resolved against
    // the server process's CWD only later (when a tool canonicalizes it), so the
    // same chat would map to different directories across restarts or launch
    // dirs. Reject it here rather than persist an ambiguous path.
    if !body.workspace_dir.is_absolute() {
        return Err(ServerError::bad_request(format!(
            "workspace_dir must be an absolute path, got {:?}",
            body.workspace_dir
        )));
    }
    // Membership is validated here (the store has no DB-level foreign key): a
    // chat can't reference a project that doesn't exist.
    if let Some(project_id) = body.project_id {
        if state.store.get_project(project_id).await?.is_none() {
            return Err(ServerError::bad_request(format!(
                "project {project_id} not found"
            )));
        }
    }
    if body.model.as_deref().is_some_and(str::is_empty) {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: body.title,
        model: body.model,
        workspace_dir: body.workspace_dir,
        created_at: Utc::now(),
    };
    state.store.create_chat(&chat).await?;
    Ok((StatusCode::CREATED, Json(chat)))
}

/// Body of `PATCH /chats/{id}`. A double option (like `PUT /settings`): absent
/// leaves the model unchanged, `null` clears it (fall back to the default), and a
/// value sets it.
#[derive(Debug, Deserialize)]
pub struct ChatUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
}

/// `PATCH /chats/{id}` — update a chat's model selection; returns the chat. This
/// is what a chat UI's model selector writes to.
pub async fn patch_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    let mut chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    if let Some(model) = body.model {
        if model.as_deref().is_some_and(str::is_empty) {
            return Err(ServerError::bad_request("model must not be empty"));
        }
        state.store.set_chat_model(id, model.clone()).await?;
        chat.model = model;
    }
    Ok(Json(chat))
}

/// `GET /chats` — list chats, most-recently-created first.
pub async fn list_chats(State(state): State<AppState>) -> Result<Json<Vec<Chat>>, ServerError> {
    Ok(Json(state.store.list_chats().await?))
}

/// `GET /chats/{id}` — fetch one chat, or `404`.
pub async fn get_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<Chat>, ServerError> {
    state
        .store
        .get_chat(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))
}

/// Body of `POST /chats/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct PostMessage {
    /// The user's input for this turn.
    pub content: String,
}

/// `POST /chats/{id}/messages` — submit a message and start a turn.
///
/// Returns `202 Accepted` immediately; the turn runs in the background and its
/// events are journaled as they emit (a client watches them over the event
/// stream). `404` if the chat doesn't exist, `409` if a turn is already running
/// for it (one turn per chat at a time).
pub async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<PostMessage>,
) -> Result<StatusCode, ServerError> {
    let chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    // Claim the chat's single turn slot up front; a concurrent turn is refused.
    let active = state.active_turns.try_acquire(id).ok_or_else(|| {
        ServerError::conflict(format!("chat {id} already has a turn in progress"))
    })?;

    // Model resolution order: the chat's own selection wins, then the global
    // default setting (PUT /settings), then the boot default in agent_config.
    // Short-circuit: only read the setting when the chat has no model of its own,
    // so a chat with a model doesn't pay (or fail on) the settings lookup.
    let mut agent_config = state.agent_config.clone();
    let model = match chat.model.clone() {
        Some(model) => Some(model),
        None => read_model(&*state.store).await?,
    };
    if let Some(model) = model {
        agent_config.model = model;
    }
    // Resolve the provider from currently-configured providers, so a key set via
    // PUT /providers/{kind} (or the legacy /settings/api-key) takes effect on
    // this turn. The composite router selects the adapter from the model name.
    let provider = state.resolver.resolve().await;
    let agent = Agent::new(
        provider,
        state.tools.clone(),
        state.store.clone(),
        agent_config,
    )
    .with_approvals(state.approvals.clone())
    // Watch the slot's token so `POST /chats/{id}/cancel` can stop this turn.
    .with_cancel(active.cancel_token())
    // Drain the slot's inbox so `POST /chats/{id}/steer` can inject mid-turn.
    .with_steer(active.steer_inbox());
    let store = state.store.clone();
    let events = state.events.clone();
    tokio::spawn(async move {
        // The hub holds the slot until the turn and its journal writes finish.
        crate::hub::drive_and_journal(agent, chat, body.content, store, events, active).await;
    });

    Ok(StatusCode::ACCEPTED)
}

/// Body of `POST /chats/{id}/steer`.
#[derive(Debug, Deserialize)]
pub struct SteerBody {
    /// User text to inject into the running turn.
    pub content: String,
    /// When true, preempt the provider stream immediately; otherwise the message
    /// waits for the next step boundary.
    #[serde(default)]
    pub interrupt: bool,
}

/// `POST /chats/{id}/steer` — inject a message into the turn currently running.
///
/// `202 Accepted` once the message is queued (and interrupt signalled, if
/// requested). The turn continues after injecting — watch the event stream for
/// `UserSteered`. `404` if the chat doesn't exist, `409` if no turn is running,
/// `400` if `content` is empty.
pub async fn post_steer(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    if body.content.trim().is_empty() {
        return Err(ServerError::bad_request("steer content must not be empty"));
    }
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    if state.active_turns.steer(id, body.content, body.interrupt) {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ServerError::conflict(format!(
            "chat {id} has no turn in progress"
        )))
    }
}

/// `POST /chats/{id}/cancel` — stop the turn currently running for a chat.
///
/// `202 Accepted` once the running turn has been signalled to stop; it winds down
/// asynchronously and emits `TurnCancelled` as its terminal event (watch the
/// event stream for it). `404` if the chat doesn't exist, `409` if no turn is
/// accepting cancel (idle, or the agent has finished and only the journal is
/// still draining). Idempotent while the agent is still running — a repeat
/// cancel simply re-trips the already-tripped token.
pub async fn post_cancel(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    // Distinguish "unknown chat" (404) from "known chat, nothing running" (409).
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    if state.active_turns.cancel(id) {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ServerError::conflict(format!(
            "chat {id} has no turn in progress"
        )))
    }
}

/// Body of `POST /chats/{id}/approvals/{call_id}`.
#[derive(Debug, Deserialize)]
pub struct ApprovalBody {
    /// `approve` or `reject`.
    pub decision: ApprovalChoice,
    /// Optional reject reason (ignored on approve).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Wire form of an approval decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Approve,
    Reject,
}

/// `POST /chats/{id}/approvals/{call_id}` — decide a parked Sensitive tool call.
///
/// `204` on success. `404` if the chat or call isn't pending. The turn stays
/// holding its slot until it finishes after the decision.
pub async fn post_approval(
    State(state): State<AppState>,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ApprovalBody>,
) -> Result<StatusCode, ServerError> {
    // Confirm the chat exists so a typo'd id doesn't look like "not pending".
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let decision = match body.decision {
        ApprovalChoice::Approve => ApprovalDecision::Approve,
        ApprovalChoice::Reject => ApprovalDecision::Reject {
            reason: body
                .reason
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "user denied approval".into()),
        },
    };
    match state.approvals.resolve(chat_id, call_id, decision) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::approvals::DecideError::NotPending) => Err(ServerError::not_found(format!(
            "no pending approval for call {call_id}"
        ))),
        Err(crate::approvals::DecideError::WrongChat) => Err(ServerError::not_found(format!(
            "no pending approval for call {call_id}"
        ))),
    }
}

/// Query for `GET /chats/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Resume after this journal sequence number; `0` (the default) replays from
    /// the start.
    #[serde(default)]
    pub after: i64,
}

/// `GET /chats/{id}/events` (WebSocket) — stream a chat's turn events.
///
/// On connect the client gets **snapshot → replay → live**: journaled events with
/// `seq > after` are replayed, then live events stream as they occur. Subscribing
/// to the live tail *before* replaying, and dropping any live event whose `seq`
/// was already replayed, means nothing is missed or duplicated across the handoff.
/// `404` if the chat doesn't exist.
///
/// Auth is checked by the bearer middleware. Browser clients authenticate via
/// `Sec-WebSocket-Protocol` (`openwave-token.<token>`). When the client offered
/// `openwave-v1`, this handler selects it in the upgrade response so the
/// browser accepts the handshake (RFC 6455).
pub async fn chat_events(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after)))
}

/// Serve one client's event stream for `chat`: replay from the journal, then live.
async fn stream_events(mut socket: WebSocket, state: AppState, chat: ChatId, after: i64) {
    // Subscribe before replaying, so an event emitted during replay is buffered on
    // the live channel rather than lost in the gap between the two.
    let mut live = state.events.subscribe(chat);

    // Replay everything the client hasn't seen yet from the durable journal.
    let mut last_seq = after;
    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            // Watch the socket so a client disconnect ends the task promptly.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            live_event = live.recv() => match live_event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue; // already covered by replay
                    }
                    last_seq = event.seq;
                    if send_event(&mut socket, &event).await.is_err() {
                        break;
                    }
                }
                // Fell behind the live buffer. Rather than drop the client, catch
                // up from the journal (durable truth) and resume live — the seq
                // dedup above absorbs any overlap. A long/fast turn can outrun the
                // 256-slot buffer, so this keeps an ordinary client connected.
                Err(RecvError::Lagged(_)) => {
                    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Send journaled events with `seq > *last_seq` to the socket, advancing
/// `*last_seq`. `Err(())` means the connection should end (send or store failure).
async fn replay_after(
    socket: &mut WebSocket,
    store: &dyn Store,
    chat: ChatId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = store.list_events(chat, *last_seq).await.map_err(|_| ())?;
    for event in events {
        *last_seq = event.seq;
        send_event(socket, &event).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Send one event as a JSON text frame. An event that fails to serialize is
/// skipped rather than sent as an empty frame (which a client couldn't decode).
async fn send_event(socket: &mut WebSocket, event: &SequencedEvent) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(event) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
