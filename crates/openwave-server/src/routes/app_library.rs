//! `/apps` — the Apps library's renderer surface: list, detail, delete.
//!
//! Everything here is a renderer-safe projection over the profile's app
//! records: names, counts, timestamps, and the grant verdict. Manifest
//! bindings, bundle bytes, and server definitions never cross this surface —
//! the consent sheet reads its own projection from `/apps/{id}/grant`, and
//! the bundle is only ever served through the single-use frame route.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;

use openwave_core::id::{AppId, AppRevisionId};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// How many library rows one listing returns. The library is a personal
/// shelf, not a search surface; a bound this size is unreachable in practice
/// and exists so a runaway profile cannot make the route unbounded.
const MAX_LISTED_APPS: u64 = 100;

/// The library listing: every live app, newest activity first.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppLibrary {
    pub apps: Vec<AppSummary>,
}

/// One library row.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppSummary {
    pub id: AppId,
    /// Display name, following the current revision's manifest.
    pub name: String,
    /// Number of retained revisions, always at least one.
    pub revision_count: u32,
    /// Creation time of the current revision.
    pub updated_at: DateTime<Utc>,
    /// Whether a live grant fully covers the app right now — the same
    /// verdict `GET /apps/{id}/grant` reports, so the library badge and the
    /// consent sheet can never disagree.
    pub granted: bool,
}

/// One app's detail: the summary fields plus its revision history.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppDetail {
    pub id: AppId,
    pub name: String,
    /// Creation time of the first revision.
    pub created_at: DateTime<Utc>,
    /// Creation time of the current revision.
    pub updated_at: DateTime<Utc>,
    /// Revision currently presented as the app's content.
    pub current_revision: AppRevisionId,
    /// Every retained revision, newest first.
    pub revisions: Vec<AppRevisionSummary>,
}

/// One revision row: identity and position only. The manifest and the
/// bundle's content address stay server-side.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppRevisionSummary {
    pub id: AppRevisionId,
    /// One-based position in the app's revision history.
    pub ordinal: u32,
    pub created_at: DateTime<Utc>,
}

/// `GET /apps` — the library listing.
pub async fn get_app_library(
    State(state): State<AppState>,
) -> Result<Json<AppLibrary>, ServerError> {
    let records = state.store.list_apps(MAX_LISTED_APPS).await?;
    // The current revisions first, so the fingerprint read knows which gateway
    // apps the listing actually needs — each one is a live catalog read, and
    // the listing must not fetch the whole entitled set to badge apps that
    // bind none of it. An app whose current revision cannot be read fails
    // closed below: it lists, but never as granted.
    let mut revisions = Vec::with_capacity(records.len());
    for record in &records {
        revisions.push(
            state
                .store
                .get_app_revision(record.current_revision)
                .await?,
        );
    }
    let needed = crate::connected_apps::gateway_apps_bound_by(
        revisions
            .iter()
            .flatten()
            .flat_map(|revision| &revision.manifest.bindings),
    );
    let current = crate::connected_apps::current_fingerprints(&state, &needed).await?;
    let mut apps = Vec::with_capacity(records.len());
    for (record, revision) in records.into_iter().zip(revisions) {
        // The same computation the grant surface performs, so the badge is
        // the invoke gate's verdict rather than a cached approximation.
        let granted = match revision {
            Some(revision) => {
                let grant = state.store.get_app_grant(record.id).await?;
                super::grant_state(&revision.manifest, grant.as_ref(), &current).granted
            }
            None => false,
        };
        apps.push(AppSummary {
            id: record.id,
            name: record.name,
            revision_count: record.revision_count,
            updated_at: record.updated_at,
            granted,
        });
    }
    Ok(Json(AppLibrary { apps }))
}

/// `GET /apps/{id}` — one live app's detail. A soft-deleted app answers as
/// missing, exactly as it does on every other renderer surface.
pub async fn get_app_detail(
    State(state): State<AppState>,
    Path(id): Path<AppId>,
) -> Result<Json<AppDetail>, ServerError> {
    let absent = || ServerError::not_found(format!("no app {id}"));
    let app = state.store.get_app(id).await?.ok_or_else(absent)?;
    if app.deleted_at.is_some() {
        return Err(absent());
    }
    let revisions = state
        .store
        .list_app_revisions(id)
        .await?
        .into_iter()
        .map(|revision| AppRevisionSummary {
            id: revision.id,
            ordinal: revision.ordinal,
            created_at: revision.created_at,
        })
        .collect();
    Ok(Json(AppDetail {
        id: app.id,
        name: app.name,
        created_at: app.created_at,
        updated_at: app.updated_at,
        current_revision: app.current_revision,
        revisions,
    }))
}

/// `DELETE /apps/{id}` — soft-delete one app.
///
/// Revisions and the grant are retained so the deletion stays recoverable;
/// what deletion removes is every open affordance — the library row, frame
/// minting, and invoke all refuse a deleted app server-side.
pub async fn delete_app(
    State(state): State<AppState>,
    Path(id): Path<AppId>,
) -> Result<StatusCode, ServerError> {
    if !state.store.delete_app(id, Utc::now()).await? {
        return Err(ServerError::not_found(format!("no app {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}
