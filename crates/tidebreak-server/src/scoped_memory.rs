//! The request-facing memory backend, bound to one authenticated principal.
//!
//! Route handlers do not touch [`AppState::store`]. They extract a
//! [`ScopedMemory`], and every memory operation carries the requesting
//! principal's [`OwnerId`]. The inner handle never escapes this type, so route
//! code cannot express a query that crosses owners.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use tidebreak_core::{
    MemoryBackend, MemoryCaps, MemoryDigest, MemoryIngestReceipt, MemoryIngestRequest,
    MemoryListFilter, MemoryRecord, MemoryRecordId, MemoryRecordUpdate, MemoryRevision,
    MemorySearchHit, MemorySearchRequest, MemoryStatusChange, MemoryWriteReceipt, OwnerId,
};

use crate::error::ServerError;
use crate::principal::AuthContext;
use crate::state::AppState;

/// Memory as one authenticated principal may see it.
///
/// The field stays private and no method returns the inner backend handle.
#[derive(Clone)]
pub struct ScopedMemory {
    backend: Arc<dyn MemoryBackend>,
    owner: OwnerId,
}

impl ScopedMemory {
    /// Bind the process's memory backend to the request's principal.
    fn new(state: &AppState, auth: &AuthContext) -> Result<Self, ServerError> {
        Ok(Self {
            backend: memory_backend(state)?,
            owner: auth.principal.owner_id(),
        })
    }

    /// Every capability the memory backend reports.
    pub fn caps(&self) -> MemoryCaps {
        self.backend.caps()
    }

    /// Ask an extraction-capable backend to derive owned records.
    pub async fn ingest(
        &self,
        request: MemoryIngestRequest,
    ) -> Result<MemoryIngestReceipt, ServerError> {
        self.backend
            .ingest(&self.owner, request)
            .await
            .map_err(ServerError::from)
    }

    /// Store one caller-supplied record for the requesting principal.
    pub async fn put(&self, record: MemoryRecord) -> Result<MemoryWriteReceipt, ServerError> {
        self.backend
            .put(&self.owner, record)
            .await
            .map_err(ServerError::from)
    }

    /// Load one record owned by the requesting principal.
    pub async fn get(&self, id: MemoryRecordId) -> Result<Option<MemoryRecord>, ServerError> {
        self.backend
            .get(&self.owner, id)
            .await
            .map_err(ServerError::from)
    }

    /// List records owned by the requesting principal.
    pub async fn list(&self, filter: MemoryListFilter) -> Result<Vec<MemoryRecord>, ServerError> {
        self.backend
            .list(&self.owner, filter)
            .await
            .map_err(ServerError::from)
    }

    /// Replace one record owned by the requesting principal.
    pub async fn update(
        &self,
        update: MemoryRecordUpdate,
    ) -> Result<MemoryWriteReceipt, ServerError> {
        self.backend
            .update(&self.owner, update)
            .await
            .map_err(ServerError::from)
    }

    /// Move one owned record through its lifecycle.
    pub async fn set_status(
        &self,
        change: MemoryStatusChange,
    ) -> Result<MemoryWriteReceipt, ServerError> {
        self.backend
            .set_status(&self.owner, change)
            .await
            .map_err(ServerError::from)
    }

    /// Hard-delete one owned record.
    pub async fn delete(&self, id: MemoryRecordId) -> Result<bool, ServerError> {
        self.backend
            .delete(&self.owner, id)
            .await
            .map_err(ServerError::from)
    }

    /// Search owned records.
    pub async fn search(
        &self,
        request: MemorySearchRequest,
    ) -> Result<Vec<MemorySearchHit>, ServerError> {
        self.backend
            .search(&self.owner, request)
            .await
            .map_err(ServerError::from)
    }

    /// Render the owned scope's active-record digest.
    pub async fn assemble_context(
        &self,
        scope: tidebreak_core::MemoryScope,
    ) -> Result<MemoryDigest, ServerError> {
        self.backend
            .assemble_context(&self.owner, scope)
            .await
            .map_err(ServerError::from)
    }

    /// Return an owned record's immutable history.
    pub async fn revision_history(
        &self,
        id: MemoryRecordId,
    ) -> Result<Vec<MemoryRevision>, ServerError> {
        self.backend
            .revision_history(&self.owner, id)
            .await
            .map_err(ServerError::from)
    }
}

impl FromRequestParts<AppState> for ScopedMemory {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth = parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or_else(|| ServerError::unauthorized("memory routes require a principal"))?;
        Self::new(state, &auth)
    }
}

/// Build the memory backend behind [`AppState`].
fn memory_backend(state: &AppState) -> Result<Arc<dyn MemoryBackend>, ServerError> {
    let store = state
        .memory
        .clone()
        .ok_or_else(|| ServerError::internal("memory storage is not configured on this server"))?;
    Ok(store)
}
