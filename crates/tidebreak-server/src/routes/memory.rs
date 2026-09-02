//! Owner-scoped HTTP routes for durable memory records.

use axum::extract::Query;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tidebreak_core::{
    MemoryAuthor, MemoryEvidence, MemoryIngestRequest, MemoryLink, MemoryListFilter, MemoryOrigin,
    MemoryProvenance, MemoryRecord, MemoryRecordId, MemoryRecordUpdate, MemoryScope,
    MemorySearchRequest, MemoryStatus, MemoryStatusChange,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::scoped_memory::ScopedMemory;

/// Largest memory request body. A record body is capped at 2 KiB, and the
/// envelope adds provenance, links, and JSON overhead.
pub const MAX_MEMORY_BODY_BYTES: usize = 64 * 1024;

/// Body of `POST /memory/records`.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct CreateMemoryRecordBody {
    pub id: MemoryRecordId,
    pub kind: tidebreak_core::MemoryKind,
    pub status: MemoryStatus,
    pub title: String,
    pub body: String,
    pub author: MemoryAuthor,
    #[serde(default)]
    pub origin: MemoryOrigin,
    #[serde(default)]
    pub evidence: Vec<MemoryEvidence>,
    #[serde(default)]
    pub links: Vec<MemoryLink>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub observation_count: u32,
}

impl CreateMemoryRecordBody {
    fn into_record(self, scope: MemoryScope) -> MemoryRecord {
        let now = Utc::now();
        MemoryRecord {
            id: self.id,
            scope,
            kind: self.kind,
            status: self.status,
            title: self.title,
            body: self.body,
            provenance: MemoryProvenance {
                author: self.author,
                origin: self.origin,
                evidence: self.evidence,
            },
            links: self.links,
            expires_at: self.expires_at,
            superseded_by: None,
            observation_count: self.observation_count,
            revision: 1,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Body of `PATCH /memory/records/{id}`.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct UpdateMemoryRecordBody {
    pub expected_revision: i64,
    pub kind: tidebreak_core::MemoryKind,
    pub title: String,
    pub body: String,
    pub author: MemoryAuthor,
    #[serde(default)]
    pub origin: MemoryOrigin,
    #[serde(default)]
    pub evidence: Vec<MemoryEvidence>,
    #[serde(default)]
    pub links: Vec<MemoryLink>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub observation_count: u32,
}

impl UpdateMemoryRecordBody {
    fn into_update(self, id: MemoryRecordId) -> MemoryRecordUpdate {
        MemoryRecordUpdate {
            id,
            expected_revision: self.expected_revision,
            kind: self.kind,
            title: self.title,
            body: self.body,
            provenance: MemoryProvenance {
                author: self.author,
                origin: self.origin,
                evidence: self.evidence,
            },
            links: self.links,
            expires_at: self.expires_at,
            observation_count: self.observation_count,
        }
    }
}

/// Body of `PUT /memory/records/{id}/status`.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct MemoryStatusBody {
    pub expected_revision: i64,
    pub status: MemoryStatus,
}

/// `GET /memory/records` filters.
#[derive(Debug, Default, Deserialize)]
pub struct MemoryListQuery {
    #[serde(flatten)]
    pub filter: MemoryListFilter,
}

/// `GET /memory/search` filters.
///
/// Query strings cannot carry a nested struct, so the scope is two flat
/// fields: `repo_id` selects one repository scope, `scope_kind=personal`
/// selects the personal scope, and neither searches every scope the owner
/// has.
#[derive(Debug, Deserialize)]
pub struct MemorySearchQuery {
    pub query: String,
    #[serde(default)]
    pub scope_kind: Option<MemorySearchScopeKind>,
    #[serde(default)]
    pub repo_id: Option<tidebreak_core::RepoId>,
    #[serde(default)]
    pub statuses: Vec<MemoryStatus>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The scope kinds a search can name without a repository id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchScopeKind {
    Personal,
    Repo,
}

impl MemorySearchQuery {
    /// The exact scope to search, or `None` for every owner scope.
    fn scope(&self) -> Result<Option<MemoryScope>, ServerError> {
        match (self.scope_kind, self.repo_id) {
            (_, Some(repo_id)) => Ok(Some(MemoryScope::Repo { repo_id })),
            (Some(MemorySearchScopeKind::Personal), None) => Ok(Some(MemoryScope::Personal)),
            (Some(MemorySearchScopeKind::Repo), None) => Err(ServerError::bad_request(
                "repo_id is required to search a repository scope",
            )),
            (None, None) => Ok(None),
        }
    }
}

/// Body of `POST /memory/ingest`.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct MemoryIngestBody {
    pub scope: MemoryScope,
    #[serde(default)]
    pub origin: MemoryOrigin,
    pub content: String,
}

/// Scope parsed from a route segment or query field.
#[derive(Debug, Default, Deserialize)]
pub struct MemoryScopeQuery {
    #[serde(default)]
    pub repo_id: Option<tidebreak_core::RepoId>,
}

impl MemoryScopeQuery {
    fn scope(&self) -> Result<MemoryScope, ServerError> {
        match self.repo_id {
            None => Ok(MemoryScope::Personal),
            Some(repo_id) => Ok(MemoryScope::Repo { repo_id }),
        }
    }
}

/// `GET /memory/capabilities` — every operation the backend reports.
pub async fn capabilities(memory: ScopedMemory) -> Json<tidebreak_core::MemoryCaps> {
    Json(memory.caps())
}

/// `GET /memory/records` — list the caller's records.
pub async fn list_records(
    memory: ScopedMemory,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<Vec<MemoryRecord>>, ServerError> {
    Ok(Json(memory.list(query.filter).await?))
}

/// `POST /memory/records` — store one caller-supplied record.
pub async fn create_record(
    memory: ScopedMemory,
    Query(scope): Query<MemoryScopeQuery>,
    Json(body): Json<CreateMemoryRecordBody>,
) -> Result<(StatusCode, Json<MemoryRecord>), ServerError> {
    let receipt = memory.put(body.into_record(scope.scope()?)).await?;
    Ok((StatusCode::CREATED, Json(receipt.record)))
}

/// `GET /memory/records/{id}` — load one caller-owned record.
pub async fn get_record(
    memory: ScopedMemory,
    Path(id): Path<MemoryRecordId>,
) -> Result<Json<MemoryRecord>, ServerError> {
    let record = memory
        .get(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("memory record {id} not found")))?;
    Ok(Json(record))
}

/// `PATCH /memory/records/{id}` — replace one editable record envelope.
pub async fn update_record(
    memory: ScopedMemory,
    Path(id): Path<MemoryRecordId>,
    Json(body): Json<UpdateMemoryRecordBody>,
) -> Result<Json<MemoryRecord>, ServerError> {
    let receipt = memory.update(body.into_update(id)).await?;
    Ok(Json(receipt.record))
}

/// `PUT /memory/records/{id}/status` — move one record through its lifecycle.
pub async fn set_record_status(
    memory: ScopedMemory,
    Path(id): Path<MemoryRecordId>,
    Json(body): Json<MemoryStatusBody>,
) -> Result<Json<MemoryRecord>, ServerError> {
    let receipt = memory
        .set_status(MemoryStatusChange {
            id,
            expected_revision: body.expected_revision,
            status: body.status,
        })
        .await?;
    Ok(Json(receipt.record))
}

/// `DELETE /memory/records/{id}` — hard-delete one record and its revisions.
pub async fn delete_record(
    memory: ScopedMemory,
    Path(id): Path<MemoryRecordId>,
) -> Result<StatusCode, ServerError> {
    if memory.delete(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found(format!(
            "memory record {id} not found"
        )))
    }
}

/// `GET /memory/search` — search full titles and bodies.
pub async fn search(
    memory: ScopedMemory,
    Query(query): Query<MemorySearchQuery>,
) -> Result<Json<Vec<tidebreak_core::MemorySearchHit>>, ServerError> {
    Ok(Json(
        memory
            .search(MemorySearchRequest {
                scope: query.scope()?,
                query: query.query,
                statuses: query.statuses,
                limit: query.limit.unwrap_or(20),
            })
            .await?,
    ))
}

/// `GET /memory/digest` — render one scope's active-record digest.
pub async fn digest(
    memory: ScopedMemory,
    Query(scope): Query<MemoryScopeQuery>,
) -> Result<Json<tidebreak_core::MemoryDigest>, ServerError> {
    Ok(Json(memory.assemble_context(scope.scope()?).await?))
}

/// `GET /memory/records/{id}/revisions` — return an owned record's history.
pub async fn revisions(
    memory: ScopedMemory,
    Path(id): Path<MemoryRecordId>,
) -> Result<Json<Vec<tidebreak_core::MemoryRevision>>, ServerError> {
    Ok(Json(memory.revision_history(id).await?))
}

/// `POST /memory/ingest` — ask an extraction-capable backend to derive records.
pub async fn ingest(
    memory: ScopedMemory,
    Json(body): Json<MemoryIngestBody>,
) -> Result<Json<tidebreak_core::MemoryIngestReceipt>, ServerError> {
    Ok(Json(
        memory
            .ingest(MemoryIngestRequest {
                scope: body.scope,
                origin: body.origin,
                content: body.content,
            })
            .await?,
    ))
}
