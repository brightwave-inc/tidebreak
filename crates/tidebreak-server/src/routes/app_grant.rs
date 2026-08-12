//! `/apps/{id}/grant` — the renderer-facing surface of the app-grant consent
//! object: read grant state, record consent, revoke.
//!
//! The renderer never supplies grant content. Consent (`POST`) is a bare
//! affirmative — the server computes the grant from the app's *current*
//! manifest and the connected-app definitions, folder registrations, and
//! gateway catalogs *current* at that moment, so a stale sheet can never
//! grant capabilities the manifest no longer pins or pin a definition that
//! has since changed.
//! State (`GET`) is renderer-safe metadata: connected-app, folder, and
//! operation *names* with coverage/staleness booleans, never definitions,
//! never paths, never the gateway deployment behind a gateway app, and never
//! environment or credential values.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;

use tidebreak_core::id::{AppId, ConnectedAppId, HostRootId};
use tidebreak_core::local_app::{
    AppBinding, AppFolderGrantBinding, AppGatewayOperationsGrantBinding, AppGrant, AppGrantBinding,
    AppManifest, AppOperationsGrantBinding, AppRecord, AppRevision, FolderAccess,
};

use crate::connected_apps::{current_fingerprints, current_rest_definitions, CurrentFingerprints};
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::host_folders::folder_fingerprint;
use crate::state::AppState;

/// Renderer-safe grant state for one app: the consent sheet's whole input.
///
/// `bindings` follows the app's **current** revision's manifest — ids and
/// names only. The definitions behind the connected apps, the paths behind
/// the folders, and any environment or credential values they select, are
/// deliberately absent from this projection.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantState {
    /// Whether a live grant fully covers the current manifest with every
    /// bound definition unchanged since consent — the "no sheet needed"
    /// verdict. A manifest with no bindings is vacuously granted: there is
    /// nothing to consent to, so no sheet is shown. When `false`,
    /// (re-)consent is required before every pinned capability is invokable.
    pub granted: bool,
    /// The current manifest's bindings, one entry per bound connected app or
    /// folder.
    pub bindings: Vec<AppGrantBindingState>,
}

/// One current-manifest binding, projected for the consent sheet.
///
/// Exactly one of `app`, `folder`, and `gateway_app` is present, matching what
/// the binding names: a local record id, a broker root id, or the gateway's
/// own connected-app id. An app-keyed or gateway row carries `operation_ids`;
/// a folder row carries `access`. A gateway row names the gateway's app and
/// the operations pinned under it, and — once the live read answers — that
/// app's display name; the gateway's deployment URL is never projected, the
/// same names-only posture the `rest_api` rows hold. The sheet derives the
/// combined-consent exfiltration warning (docs/folder-bindings.md) from the
/// rows themselves: a manifest with both a folder row and a network row —
/// local or gateway — can read files and reach the network.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppGrantBindingState {
    /// Connected app the manifest binds, by record id, for an app-keyed
    /// binding.
    pub app: Option<ConnectedAppId>,
    /// Connected folder the manifest binds, by broker root id, for a folder
    /// binding.
    pub folder: Option<HostRootId>,
    /// Gateway connected app the manifest binds, by the gateway's own app id,
    /// for a gateway binding. The id alone — never the gateway that serves
    /// it.
    pub gateway_app: Option<String>,
    /// The access level a folder binding requests.
    pub access: Option<FolderAccess>,
    /// The bound connected app's, folder's, or gateway app's display name,
    /// absent when nothing configured, approved, or readable answers to the
    /// id — the sheet says so instead of showing a raw id alone.
    pub name: Option<String>,
    /// Catalog `operationId`s the current manifest pins under this app, for a
    /// `rest_api` or gateway binding.
    pub operation_ids: Option<Vec<String>>,
    /// Whether the live grant covers every listed capability under this
    /// binding and its target still matches the granted fingerprint.
    pub granted: bool,
    /// Whether a grant names this binding's target but it changed — a
    /// reconfigured record, a disconnected folder — since consent: the
    /// "changed since you agreed" affordance, distinct from a binding that
    /// was simply never granted.
    pub definition_changed: bool,
}

/// `GET /apps/{id}/grant` — the app's current grant state.
pub async fn get_app_grant_state(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<Json<AppGrantState>, ServerError> {
    let (app, revision) = current_live_app(&state, app_id).await?;
    let grant = state.store.get_app_grant(app.id).await?;
    let current = current_fingerprints(
        &state,
        &crate::connected_apps::gateway_apps_bound_by(&revision.manifest.bindings),
    )
    .await?;
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
/// manifest, the current definition fingerprints, and the currently approved
/// folders, then replaces any previous grant wholesale.
pub async fn post_app_grant(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
) -> Result<Json<AppGrantState>, ServerError> {
    let (app, revision) = current_live_app(&state, app_id).await?;
    let current = current_fingerprints(
        &state,
        &crate::connected_apps::gateway_apps_bound_by(&revision.manifest.bindings),
    )
    .await?;
    let rest_definitions = current_rest_definitions(&state).await?;
    let mut bindings = Vec::with_capacity(revision.manifest.bindings.len());
    for binding in &revision.manifest.bindings {
        match binding {
            // A folder binding pins a host-approved registration. An
            // embedding without the host-folder seam, or a folder no longer
            // approved, leaves nothing coherent to consent to — the same
            // fail-closed reading as an unconfigured connected app.
            AppBinding::Folder(binding) => {
                if !current.folders.contains_key(&binding.folder) {
                    return Err(ServerError::conflict(format!(
                        "folder {} is not a connected folder, so this app cannot be \
                         granted",
                        binding.folder
                    )));
                }
                bindings.push(AppGrantBinding::Folder(AppFolderGrantBinding {
                    folder: binding.folder,
                    access: binding.access,
                    fingerprint: folder_fingerprint(binding.folder, binding.access),
                }));
            }
            AppBinding::GatewayOperations(binding) => {
                // A gateway binding pins an app only the gateway describes,
                // so consent needs a live read to pin. With no session there
                // is nothing to ask, and with nothing answering to the id
                // there is no catalog to fingerprint — both leave nothing
                // coherent to consent to, the same fail-closed reading as an
                // unconfigured connected app or a folder that is no longer
                // approved.
                if !current.gateway_session {
                    return Err(ServerError::conflict(format!(
                        "there is no gateway session, so gateway app {} cannot be \
                         granted",
                        binding.gateway_app
                    )));
                }
                let Some(gateway_app) = current.gateway_apps.get(&binding.gateway_app) else {
                    return Err(ServerError::conflict(format!(
                        "gateway app {} is not available, so this app cannot be granted",
                        binding.gateway_app
                    )));
                };
                // Every pinned operation must exist in the app's live
                // catalog, exactly as a `rest_api` pin must exist in the
                // record's — a pin the gateway no longer declares leaves
                // nothing coherent to consent to.
                if let Some(operation_id) = binding
                    .operation_ids
                    .iter()
                    .find(|operation_id| !gateway_app.operation_ids.contains(operation_id))
                {
                    return Err(ServerError::conflict(format!(
                        "operation {operation_id:?} is not declared by gateway app \
                         {:?}, so this app cannot be granted",
                        gateway_app.name
                    )));
                }
                bindings.push(AppGrantBinding::GatewayOperations(
                    AppGatewayOperationsGrantBinding {
                        gateway_app: binding.gateway_app.clone(),
                        operation_ids: binding.operation_ids.clone(),
                        fingerprint: gateway_app.fingerprint,
                    },
                ));
            }
            AppBinding::Operations(binding) => {
                // A binding whose connected app is not configured cannot be
                // pinned to a definition, so there is nothing coherent to
                // consent to. An unparseable rest_api definition reads the
                // same way, by design.
                let Some(app_fingerprint) = current.apps.get(&binding.app) else {
                    return Err(ServerError::conflict(format!(
                        "connected app {} is not configured, so this app cannot be \
                         granted",
                        binding.app
                    )));
                };
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
    register_at_the_gateway(&state, app.id, &revision.manifest);
    Ok(Json(grant_state(
        &revision.manifest,
        Some(&grant),
        &current,
    )))
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

/// Register a just-granted app at the gateway and relay the author's consent
/// for it — off the request, after the grant is already durable.
///
/// Consent to a gateway binding is consent to relay through a shared app the
/// gateway holds, so the registration is part of making the grant usable
/// rather than something to ask about separately. It is deliberately neither
/// part of the transaction nor part of the response: the grant is a local
/// decision that a gateway which is down, unreachable, or slow must never
/// fail — and must never hold the consent sheet open while it times out.
/// Everything here is re-attempted on the first invoke, which registers and
/// consents on the spot if this could not, so the only cost of a failure is
/// that the first call pays for it.
///
/// Nothing is attempted for a manifest that binds no gateway app: there is
/// nothing at the gateway for it to be.
fn register_at_the_gateway(state: &AppState, app_id: AppId, manifest: &AppManifest) {
    if crate::connected_apps::gateway_apps_bound_by(&manifest.bindings).is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let Some(base_url) = crate::gateway_drafts::registration_base_url(&state.gateway).await
        else {
            return;
        };
        match state.gateway_drafts.relay_consent(app_id, &base_url).await {
            Ok(crate::connected_apps::GatewayConsentRelay::Consented) => {}
            Ok(outcome) => tracing::info!(
                %app_id,
                "this app is not registered at the gateway yet: {outcome:?}"
            ),
            Err(error) => tracing::warn!(
                %app_id,
                "could not register this app at the gateway: {error}"
            ),
        }
    });
}

/// Resolve a live app and its current revision, or 404.
///
/// A soft-deleted app answers as missing on the consent surface, exactly as
/// it does on invoke.
pub(crate) async fn current_live_app(
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
/// pins, in the same vocabulary. A grant kept under another vocabulary
/// covers nothing: consent named operations or a folder at an access level,
/// never "whatever the target now speaks". A `read_write` folder grant
/// covers a `read` pin: a revision that narrows its access stays granted,
/// mirroring how a narrowed operation set stays covered.
fn binding_covered(pinned: &AppBinding, granted: &AppGrantBinding) -> bool {
    match (pinned, granted) {
        (AppBinding::Operations(pinned), AppGrantBinding::Operations(granted)) => pinned
            .operation_ids
            .iter()
            .all(|operation| granted.operation_ids.iter().any(|held| held == operation)),
        (AppBinding::Folder(pinned), AppGrantBinding::Folder(granted)) => {
            granted.folder == pinned.folder
                && (granted.access == pinned.access || granted.access == FolderAccess::ReadWrite)
        }
        (AppBinding::GatewayOperations(pinned), AppGrantBinding::GatewayOperations(granted)) => {
            granted.gateway_app == pinned.gateway_app
                && pinned
                    .operation_ids
                    .iter()
                    .all(|operation| granted.operation_ids.iter().any(|held| held == operation))
        }
        _ => false,
    }
}

/// The grant binding covering one manifest binding's target, when the grant
/// names it: app-keyed bindings match by record id, folder bindings by root
/// id, and gateway bindings by the gateway's app id.
fn granted_binding_for<'a>(
    grant: Option<&'a AppGrant>,
    binding: &AppBinding,
) -> Option<&'a AppGrantBinding> {
    let grant = grant?;
    grant.bindings.iter().find(|candidate| match binding {
        AppBinding::Folder(binding) => {
            matches!(candidate, AppGrantBinding::Folder(granted) if granted.folder == binding.folder)
        }
        AppBinding::GatewayOperations(binding) => candidate
            .gateway_app()
            .is_some_and(|granted| granted == binding.gateway_app),
        AppBinding::Operations(binding) => candidate.app() == Some(binding.app),
    })
}

/// Project grant state against the current manifest, definitions, and
/// approved folders.
///
/// The same checks the invoke gate applies, evaluated for the whole
/// manifest: `granted` is true exactly when every pinned capability would
/// pass the gate right now. Shared with the library listing so its granted
/// badge is this verdict rather than a reimplementation of it.
pub(crate) fn grant_state(
    manifest: &AppManifest,
    grant: Option<&AppGrant>,
    current: &CurrentFingerprints,
) -> AppGrantState {
    let bindings: Vec<AppGrantBindingState> = manifest
        .bindings
        .iter()
        .map(|binding| {
            let granted_binding = granted_binding_for(grant, binding);
            let covered = granted_binding.is_some_and(|granted| binding_covered(binding, granted));
            let target_current =
                granted_binding.is_some_and(|granted| current.grant_binding_current(granted));
            let (operation_ids, access) = match binding {
                AppBinding::Operations(binding) => (Some(binding.operation_ids.clone()), None),
                AppBinding::GatewayOperations(binding) => {
                    (Some(binding.operation_ids.clone()), None)
                }
                AppBinding::Folder(binding) => (None, Some(binding.access)),
            };
            let name = match binding {
                AppBinding::Folder(binding) => current.folders.get(&binding.folder).cloned(),
                AppBinding::GatewayOperations(binding) => current
                    .gateway_apps
                    .get(&binding.gateway_app)
                    .map(|app| app.name.clone()),
                AppBinding::Operations(binding) => {
                    current.apps.get(&binding.app).map(|app| app.name.clone())
                }
            };
            AppGrantBindingState {
                app: binding.app(),
                folder: match binding {
                    AppBinding::Folder(binding) => Some(binding.folder),
                    _ => None,
                },
                gateway_app: binding.gateway_app().map(str::to_owned),
                access,
                name,
                operation_ids,
                granted: covered && target_current,
                definition_changed: granted_binding.is_some() && !target_current,
            }
        })
        .collect();
    // A manifest that binds nothing has nothing to consent to: every invoke
    // fails the pin check before the consent gate is reached, so prompting
    // would gate the frame on a vacuous "yes". Granted, with or without a
    // stored grant.
    //
    // Otherwise the invoke gate also pins targets the grant names beyond the
    // current manifest, so the overall verdict does too.
    let granted = manifest.bindings.is_empty()
        || grant.is_some_and(|grant| {
            bindings.iter().all(|binding| binding.granted)
                && grant
                    .bindings
                    .iter()
                    .all(|binding| current.grant_binding_current(binding))
        });
    AppGrantState { granted, bindings }
}
