//! `/apps/{id}/grant` — the renderer-facing surface of the app-grant consent
//! object: read grant state, record consent, revoke.
//!
//! The renderer never supplies grant content. Consent (`POST`) is a bare
//! affirmative — the server computes the grant from the app's *current*
//! manifest and the server definitions *current* at that moment, so a stale
//! sheet can never grant tools the manifest no longer pins or pin a
//! definition that has since changed. State (`GET`) is renderer-safe
//! metadata: server and tool *names* with coverage/staleness booleans, never
//! definitions and never environment values.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;

use openwave_core::id::{AppId, ConnectedAppId};
use openwave_core::local_app::{
    mounted_tool_under, AppGrant, AppGrantBinding, AppManifest, AppRecord, AppRevision,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::mcp_config::McpAppFingerprint;
use crate::state::AppState;

/// Renderer-safe grant state for one app: the consent sheet's whole input.
///
/// `bindings` follows the app's **current** revision's manifest — ids and
/// names only. The definitions behind the connected apps, and any environment
/// or token values they select, are deliberately absent from this projection.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantState {
    /// Whether a live grant fully covers the current manifest with every
    /// bound definition unchanged since consent — the "no sheet needed"
    /// verdict. When `false`, (re-)consent is required before every pinned
    /// tool is invokable.
    pub granted: bool,
    /// The current manifest's bindings, one entry per bound connected app.
    pub bindings: Vec<AppGrantBindingState>,
}

/// One current-manifest binding, projected for the consent sheet.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantBindingState {
    /// Connected app the manifest binds, by record id.
    pub app: ConnectedAppId,
    /// The connected app's display name, absent when no record with that id
    /// is configured — the sheet says so instead of showing a raw id alone.
    pub name: Option<String>,
    /// Full mounted tool names the current manifest pins under this app.
    pub tools: Vec<String>,
    /// Whether the live grant covers every listed tool under this connected
    /// app and the app's current definition still matches the granted
    /// fingerprint.
    pub granted: bool,
    /// Whether a grant names this connected app but its definition changed
    /// (or the record disappeared) since consent — the "reconfigured since
    /// you agreed" affordance, distinct from a binding that was simply never
    /// granted.
    pub definition_changed: bool,
}

/// `GET /apps/{id}/grant` — the app's current grant state.
pub async fn get_app_grant_state(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<Json<AppGrantState>, ServerError> {
    let (app, revision) = current_live_app(&state, app_id).await?;
    let grant = state.store.get_app_grant(app.id).await?;
    let current = state.mcp.app_fingerprints().await;
    Ok(Json(grant_state(
        &revision.manifest,
        grant.as_ref(),
        &current,
    )))
}

/// `POST /apps/{id}/grant` — record explicit user consent.
///
/// The request carries no body: consent is only ever "yes to what the server
/// shows right now". The grant is computed here from the current revision's
/// manifest and the current definition fingerprints, then replaces any
/// previous grant wholesale.
pub async fn post_app_grant(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<Json<AppGrantState>, ServerError> {
    let (app, revision) = current_live_app(&state, app_id).await?;
    let current = state.mcp.app_fingerprints().await;
    let mut bindings = Vec::with_capacity(revision.manifest.bindings.len());
    for binding in &revision.manifest.bindings {
        // A binding whose connected app is not configured cannot be pinned to
        // a definition, so there is nothing coherent to consent to.
        let Some(app_fingerprint) = current.get(&binding.app) else {
            return Err(ServerError::conflict(format!(
                "connected app {} is not configured, so this app cannot be granted",
                binding.app
            )));
        };
        // Nor is there anything coherent when a pinned name is not under the
        // app's current namespace (the record was renamed after authoring):
        // the grant would name tools that cannot exist under this app.
        if let Some(tool) = binding
            .tools
            .iter()
            .find(|tool| mounted_tool_under(&app_fingerprint.name, tool).is_none())
        {
            return Err(ServerError::conflict(format!(
                "tool {tool:?} is not mounted under connected app {:?}, so this \
                 app cannot be granted",
                app_fingerprint.name
            )));
        }
        bindings.push(AppGrantBinding {
            app: binding.app,
            tools: binding.tools.clone(),
            fingerprint: app_fingerprint.fingerprint,
        });
    }
    let grant = AppGrant {
        app_id: app.id,
        bindings,
        created_at: Utc::now(),
    };
    state.store.put_app_grant(&grant).await?;
    Ok(Json(grant_state(
        &revision.manifest,
        Some(&grant),
        &current,
    )))
}

/// Whether one granted binding still pins the definition its connected app
/// carries right now. A missing record is a mismatch, never a match.
fn fingerprint_current(
    binding: &AppGrantBinding,
    current: &std::collections::BTreeMap<ConnectedAppId, McpAppFingerprint>,
) -> bool {
    current
        .get(&binding.app)
        .is_some_and(|app| app.fingerprint == binding.fingerprint)
}

/// `DELETE /apps/{id}/grant` — revoke consent.
///
/// Idempotent, and deliberately available for a soft-deleted app too:
/// revocation is fail-safe and must never be blocked by library state. The
/// next invoke refuses with `consent_required`.
pub async fn delete_app_grant(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<StatusCode, ServerError> {
    if state.store.get_app(app_id).await?.is_none() {
        return Err(ServerError::not_found(format!("no app {app_id}")));
    }
    state.store.delete_app_grant(app_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a live app and its current revision, or 404.
///
/// A soft-deleted app answers as missing on the consent surface, exactly as
/// it does on invoke.
async fn current_live_app(
    state: &AppState,
    app_id: AppId,
) -> Result<(AppRecord, AppRevision), ServerError> {
    let absent = || ServerError::not_found(format!("no app {app_id}"));
    let app = state.store.get_app(app_id).await?.ok_or_else(absent)?;
    if app.deleted_at.is_some() {
        return Err(absent());
    }
    let revision = state
        .store
        .get_app_revision(app.current_revision)
        .await?
        .ok_or_else(absent)?;
    Ok((app, revision))
}

/// Project grant state against the current manifest and current definitions.
///
/// The same three checks the invoke gate applies, evaluated for the whole
/// manifest: `granted` is true exactly when every pinned tool would pass the
/// gate right now. Shared with the library listing so its granted badge is
/// this verdict rather than a reimplementation of it.
pub(crate) fn grant_state(
    manifest: &AppManifest,
    grant: Option<&AppGrant>,
    current: &std::collections::BTreeMap<ConnectedAppId, McpAppFingerprint>,
) -> AppGrantState {
    let bindings: Vec<AppGrantBindingState> = manifest
        .bindings
        .iter()
        .map(|binding| {
            let granted_binding = grant.and_then(|grant| {
                grant
                    .bindings
                    .iter()
                    .find(|candidate| candidate.app == binding.app)
            });
            let covered = granted_binding.is_some_and(|granted| {
                binding
                    .tools
                    .iter()
                    .all(|tool| granted.tools.iter().any(|held| held == tool))
            });
            let definition_changed =
                granted_binding.is_some_and(|granted| !fingerprint_current(granted, current));
            AppGrantBindingState {
                app: binding.app,
                name: current.get(&binding.app).map(|app| app.name.clone()),
                tools: binding.tools.clone(),
                granted: covered
                    && granted_binding.is_some_and(|granted| fingerprint_current(granted, current)),
                definition_changed,
            }
        })
        .collect();
    // The invoke gate also pins connected apps the grant names beyond the
    // current manifest, so the overall verdict does too.
    let granted = grant.is_some_and(|grant| {
        bindings.iter().all(|binding| binding.granted)
            && grant
                .bindings
                .iter()
                .all(|binding| fingerprint_current(binding, current))
    });
    AppGrantState { granted, bindings }
}
