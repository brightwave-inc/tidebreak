//! `/apps/{id}/grant` — the renderer-facing surface of the app-grant consent
//! object: read grant state, record consent, revoke.
//!
//! The renderer never supplies grant content. Consent (`POST`) is a bare
//! affirmative — the server computes the grant from the app's *current*
//! manifest and the connected-app definitions *current* at that moment, so a
//! stale sheet can never grant capabilities the manifest no longer pins or
//! pin a definition that has since changed. State (`GET`) is renderer-safe
//! metadata: connected-app, tool, and operation *names* with
//! coverage/staleness booleans, never definitions and never environment or
//! credential values.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;

use openwave_core::id::{AppId, ConnectedAppId};
use openwave_core::local_app::{
    AppBinding, AppGrant, AppGrantBinding, AppManifest, AppOperationsGrantBinding, AppRecord,
    AppRevision,
};

use crate::connected_apps::{current_app_fingerprints, current_rest_definitions, AppFingerprint};
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// Renderer-safe grant state for one app: the consent sheet's whole input.
///
/// `bindings` follows the app's **current** revision's manifest — ids and
/// names only. The definitions behind the connected apps, and any environment
/// or credential values they select, are deliberately absent from this
/// projection.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantState {
    /// Whether a live grant fully covers the current manifest with every
    /// bound definition unchanged since consent — the "no sheet needed"
    /// verdict. When `false`, (re-)consent is required before every pinned
    /// capability is invokable.
    pub granted: bool,
    /// The current manifest's bindings, one entry per bound connected app.
    pub bindings: Vec<AppGrantBindingState>,
}

/// One current-manifest binding, projected for the consent sheet.
///
/// Exactly one of `tools` and `operation_ids` is present, matching the
/// binding's vocabulary: mounted MCP tools for an `mcp_server` binding,
/// declared operations for a `rest_api` binding.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantBindingState {
    /// Connected app the manifest binds, by record id.
    pub app: ConnectedAppId,
    /// The connected app's display name, absent when no record with that id
    /// is configured — the sheet says so instead of showing a raw id alone.
    pub name: Option<String>,
    /// Full mounted tool names the current manifest pins under this app, for
    /// an `mcp_server` binding.
    pub tools: Option<Vec<String>>,
    /// Catalog `operationId`s the current manifest pins under this app, for a
    /// `rest_api` binding.
    pub operation_ids: Option<Vec<String>>,
    /// Whether the live grant covers every listed capability under this
    /// connected app and the app's current definition still matches the
    /// granted fingerprint.
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
    let current = current_app_fingerprints(&state).await?;
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
    let current = current_app_fingerprints(&state).await?;
    let rest_definitions = current_rest_definitions(&state).await?;
    let mut bindings = Vec::with_capacity(revision.manifest.bindings.len());
    for binding in &revision.manifest.bindings {
        // A binding whose connected app is not configured cannot be pinned to
        // a definition, so there is nothing coherent to consent to. An
        // unparseable rest_api definition reads the same way, by design.
        let Some(app_fingerprint) = current.get(&binding.app()) else {
            return Err(ServerError::conflict(format!(
                "connected app {} is not configured, so this app cannot be granted",
                binding.app()
            )));
        };
        match binding {
            // Mounted-tool bindings are retired: MCP was the only bindable
            // kind when local apps shipped, and the REST vocabulary has since
            // replaced it as the app-facing surface (#1332). A manifest still
            // pinning tools cannot be granted — the failure direction is
            // "revise the app", never "grant the legacy surface".
            AppBinding::Tools(_) => {
                return Err(ServerError::conflict(format!(
                    "connected app {:?} is bound by mounted MCP tools, which local \
                     apps no longer support; publish a revision binding a rest_api \
                     connected app's operations instead",
                    app_fingerprint.name
                )));
            }
            AppBinding::Operations(binding) => {
                // Every pinned operation must exist in the record's current
                // catalog — a pin the catalog no longer declares leaves
                // nothing coherent to consent to.
                let declared = rest_definitions
                    .iter()
                    .find(|(id, _, _)| *id == binding.app)
                    .map(|(_, _, definition)| &definition.catalog.operations);
                let Some(declared) = declared else {
                    return Err(ServerError::conflict(format!(
                        "connected app {:?} is a {} app, so its operations binding \
                         cannot be granted",
                        app_fingerprint.name, app_fingerprint.kind
                    )));
                };
                if let Some(operation_id) = binding
                    .operation_ids
                    .iter()
                    .find(|operation_id| !declared.contains_key(*operation_id))
                {
                    return Err(ServerError::conflict(format!(
                        "operation {operation_id:?} is not declared by connected app \
                         {:?}, so this app cannot be granted",
                        app_fingerprint.name
                    )));
                }
                bindings.push(AppGrantBinding::Operations(AppOperationsGrantBinding {
                    app: binding.app,
                    operation_ids: binding.operation_ids.clone(),
                    fingerprint: app_fingerprint.fingerprint,
                }));
            }
        }
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
    current: &BTreeMap<ConnectedAppId, AppFingerprint>,
) -> bool {
    current
        .get(&binding.app())
        .is_some_and(|app| app.fingerprint == binding.fingerprint())
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

/// Whether a granted binding covers everything a current-manifest binding
/// pins, in the same vocabulary. A grant kept under the other vocabulary
/// covers nothing: consent named tools or operations, never "whatever the
/// record now speaks". A tools binding is never covered — the vocabulary is
/// retired (#1332), so a grant recorded before the retirement reads as
/// ungranted rather than keeping the legacy surface invokable.
fn binding_covered(pinned: &AppBinding, granted: &AppGrantBinding) -> bool {
    match (pinned, granted) {
        (AppBinding::Tools(_), _) => false,
        (AppBinding::Operations(pinned), AppGrantBinding::Operations(granted)) => pinned
            .operation_ids
            .iter()
            .all(|operation| granted.operation_ids.iter().any(|held| held == operation)),
        _ => false,
    }
}

/// Project grant state against the current manifest and current definitions.
///
/// The same three checks the invoke gate applies, evaluated for the whole
/// manifest: `granted` is true exactly when every pinned capability would
/// pass the gate right now. Shared with the library listing so its granted
/// badge is this verdict rather than a reimplementation of it.
pub(crate) fn grant_state(
    manifest: &AppManifest,
    grant: Option<&AppGrant>,
    current: &BTreeMap<ConnectedAppId, AppFingerprint>,
) -> AppGrantState {
    let bindings: Vec<AppGrantBindingState> = manifest
        .bindings
        .iter()
        .map(|binding| {
            let granted_binding = grant.and_then(|grant| {
                grant
                    .bindings
                    .iter()
                    .find(|candidate| candidate.app() == binding.app())
            });
            let covered = granted_binding.is_some_and(|granted| binding_covered(binding, granted));
            let definition_changed =
                granted_binding.is_some_and(|granted| !fingerprint_current(granted, current));
            let (tools, operation_ids) = match binding {
                AppBinding::Tools(binding) => (Some(binding.tools.clone()), None),
                AppBinding::Operations(binding) => (None, Some(binding.operation_ids.clone())),
            };
            AppGrantBindingState {
                app: binding.app(),
                name: current.get(&binding.app()).map(|app| app.name.clone()),
                tools,
                operation_ids,
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
