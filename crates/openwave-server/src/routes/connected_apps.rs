//! `/connected-apps` — the renderer-facing settings surface of the
//! connected-app record class: one listing across both kinds, and CRUD for
//! the `rest_api` kind.
//!
//! `mcp_server` records stay editable through the MCP runtime's own routes
//! (`/mcp/servers`); this surface projects their health beside the REST
//! entries so Settings can show one Connected apps page. Everything here is
//! renderer-safe: catalog counts and credential *status*, never a definition
//! blob, a document, or a credential value.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use openwave_core::connected_app::{validate_connected_app, ConnectedApp, ConnectedAppKind};
use openwave_core::id::ConnectedAppId;

use crate::connected_apps::{
    parse_rest_api_definition, rest_credential_secret_key, RestApiDefinition,
};
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::mcp_config::McpHealth;
use crate::openapi_catalog::{
    enumerate_openapi_operations, ingest_openapi_document, sha256_hex, MAX_OPENAPI_DOCUMENT_BYTES,
};
use crate::providers::managed_profile_refusal;
use crate::rest_executor::{
    admit_base_url, fetch_spec_document, CredentialPlacement, RestCredential,
};
use crate::state::AppState;

/// Body bound for the `rest_api` upsert route: the OpenAPI document travels
/// as a JSON string, so the ingest bound is doubled for escaping, plus slack
/// for the other fields. An over-bound *document* still gets the ingest
/// refusal's precise message; this limit only stops unbounded bodies.
pub const MAX_REST_CONNECTED_APP_BODY_BYTES: usize = 2 * MAX_OPENAPI_DOCUMENT_BYTES + 64 * 1024;

/// The renderer's listing of every configured connected app, across kinds.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ConnectedAppsInfo {
    /// MCP entries in the runtime's configuration order, then REST entries in
    /// storage order (oldest first).
    pub apps: Vec<ConnectedAppInfo>,
}

/// One connected app, projected per kind for the Settings listing.
///
/// Closed and renderer-safe: an `mcp_server` entry carries the runtime's
/// health projection, a `rest_api` entry carries catalog and credential
/// *status* — never transport definitions, documents, or values.
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectedAppInfo {
    McpServer {
        /// The record id app bindings name.
        id: ConnectedAppId,
        /// Display name — also the namespace the server's tools mount under.
        name: String,
        health: McpHealth,
        tool_count: usize,
        /// The bare mounted tool names (after the `mcp__{server}__` prefix),
        /// bounded by the per-server discovery cap. Names only — never
        /// remote-authored descriptions — the consent sheet's posture.
        tools: Vec<String>,
        diagnostic: Option<String>,
        /// The gateway MCP endpoint slug this record mounts, when it is
        /// gateway-backed rather than a local stdio/HTTP definition.
        gateway_endpoint: Option<String>,
        /// Display names of the organization's entitled apps that ride this
        /// record's gateway endpoint. Empty for local records — and, by
        /// graceful degradation, when the gateway is unreachable or predates
        /// the apps surface: the entry then renders without org-app names
        /// rather than erroring.
        gateway_apps: Vec<String>,
        /// How many local mini-apps hold a live grant binding this record —
        /// a count only, never app names or ids, the renderer-safety posture
        /// of this surface. Grants of library-deleted apps do not count.
        used_by_app_count: usize,
    },
    RestApi {
        /// The record id app bindings name.
        id: ConnectedAppId,
        name: String,
        base_url: String,
        /// Operations the ingested catalog declares.
        operation_count: usize,
        /// Hex SHA-256 of the raw OpenAPI document the catalog was ingested
        /// from.
        document_sha256: String,
        credential_status: RestCredentialStatus,
        /// Where the stored credential value is placed at request time, when
        /// one is referenced. The placement (and a custom header *name*) is
        /// configuration; the value never appears on this surface.
        placement: Option<CredentialPlacement>,
        updated_at: DateTime<Utc>,
        /// How many local mini-apps hold a live grant binding this record —
        /// a count only, never app names or ids, the renderer-safety posture
        /// of this surface. Grants of library-deleted apps do not count.
        used_by_app_count: usize,
    },
}

/// Whether a `rest_api` record's referenced credential currently resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RestCredentialStatus {
    /// The record references no credential (a public API).
    None,
    /// The referenced secret exists in the profile secret store.
    Configured,
    /// The record references a secret the store does not hold — the executor
    /// will refuse until the value is entered again.
    Missing,
}

/// `PUT /connected-apps/rest/{id}` body: the complete configuration of one
/// `rest_api` connected app, with the OpenAPI document to ingest — inline, or
/// fetched from a URL under a hash pin. Ingested once, here; only the bounded
/// operation catalog (with the document's hash) is stored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestConnectedAppUpsert {
    pub name: String,
    pub base_url: String,
    /// The raw JSON OpenAPI document, when supplied inline. Exactly one of
    /// this and `openapi_document_url` must be present.
    #[serde(default)]
    pub openapi_document: Option<String>,
    /// URL to fetch the document from at upsert time, under the executor's
    /// egress hygiene. Requires `document_sha256`.
    #[serde(default)]
    pub openapi_document_url: Option<String>,
    /// Hex SHA-256 the (fetched or inline) document must hash to — the pin
    /// carried forward from the preview, so what was previewed is exactly
    /// what ingests. Required with a URL source, optional inline.
    #[serde(default)]
    pub document_sha256: Option<String>,
    /// When present, ingest only these `operationId`s: the catalog is the
    /// selection, and the rest of the document is not judged. Absent means
    /// the whole document must ingest, as before.
    #[serde(default)]
    pub operation_ids: Option<Vec<String>>,
    pub credential: RestCredentialUpdate,
}

/// What the upsert does about the credential. Externally tagged and closed:
/// `"none"`, `"keep"`, or `{"set": {"value": …, "placement": …}}` — so an
/// edit can rotate, keep, or clear without the form ever reading the value
/// back.
///
/// No `Debug` on the carrying types: the value must never reach a log or an
/// error, and a derive would hand it to the first `{:?}` that sees the body.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RestCredentialUpdate {
    /// Clear: the record references no credential and the stored value is
    /// deleted.
    None,
    /// Preserve the existing record's credential reference and placement
    /// unchanged.
    Keep,
    /// Store a new value and reference it with the given placement.
    Set(RestCredentialSet),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestCredentialSet {
    /// The credential value. Written to the profile secret store under the
    /// record's derived key and never persisted, echoed, or logged anywhere
    /// else.
    pub value: String,
    pub placement: CredentialPlacement,
}

/// `GET /connected-apps` — every configured connected app, both kinds.
pub async fn get_connected_apps(
    State(state): State<AppState>,
) -> Result<Json<ConnectedAppsInfo>, ServerError> {
    Ok(Json(connected_apps_info(&state).await?))
}

/// `PUT /connected-apps/rest/{id}` — create or replace one `rest_api`
/// connected app: admit the base URL, ingest the document, store the
/// credential, persist the record.
///
/// On a managed profile the whole route is refused before anything is read
/// or validated: local credential entry against arbitrary endpoints is
/// exactly what the managed lockdown exists to close, the same posture as
/// BYOK provider keys. An unreadable policy refuses too — fail closed.
pub async fn put_rest_connected_app(
    State(state): State<AppState>,
    Path(id): Path<ConnectedAppId>,
    Json(body): Json<RestConnectedAppUpsert>,
) -> Result<Json<ConnectedAppsInfo>, ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if policy.managed {
        return Err(managed_profile_refusal(
            "REST connected apps are managed by your organization's gateway",
        ));
    }

    admit_base_url(&body.base_url).map_err(|error| ServerError::bad_request(error.to_string()))?;

    let document = match (&body.openapi_document, &body.openapi_document_url) {
        (Some(_), Some(_)) => {
            return Err(ServerError::bad_request(
                "supply the OpenAPI document inline or by URL, not both",
            ))
        }
        (None, None) => {
            return Err(ServerError::bad_request(
                "an OpenAPI document is required, inline or by URL",
            ))
        }
        (Some(inline), None) => inline.as_bytes().to_vec(),
        (None, Some(url)) => {
            if body.document_sha256.is_none() {
                return Err(ServerError::bad_request(
                    "a URL-sourced document requires document_sha256 from the preview",
                ));
            }
            fetch_spec_document(url)
                .await
                .map_err(|error| ServerError::bad_request_kind("spec_fetch", error.to_string()))?
        }
    };
    // The pin closes the preview-to-upsert gap: if the published document
    // changed in between, what the picker showed is not what would ingest,
    // so nothing does.
    if let Some(expected) = &body.document_sha256 {
        let actual = sha256_hex(&document);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(ServerError::conflict(
                "the document no longer matches the previewed hash — it changed upstream; \
                 re-run the preview and reselect operations",
            ));
        }
    }
    let selection = match &body.operation_ids {
        None => None,
        Some(ids) => {
            if ids.is_empty() {
                return Err(ServerError::bad_request(
                    "operation_ids must select at least one operation when present",
                ));
            }
            Some(
                ids.iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
            )
        }
    };

    // Ingest once, at configuration time. The raw document is not stored;
    // only the bounded catalog — carrying the document's hash — is. The
    // ingest errors are bounded and renderer-safe, so they travel verbatim
    // as the form error.
    let catalog = ingest_openapi_document(&document, selection.as_ref())
        .map_err(|error| ServerError::bad_request_kind("openapi_ingest", error.to_string()))?;

    let mut rest_records = rest_records(&state).await?;
    let existing = rest_records.iter().find(|record| record.id == id);
    let created_at = existing.map(|record| record.created_at);

    let mut clear_stored_value = false;
    let credential = match body.credential {
        RestCredentialUpdate::None => {
            // The stored value is deleted only after the record no longer
            // references it, so a failed persist cannot leave a reference
            // pointing at nothing.
            clear_stored_value = true;
            None
        }
        RestCredentialUpdate::Keep => {
            let Some(existing) = existing else {
                return Err(ServerError::bad_request(
                    "credential \"keep\" requires an existing connected app to keep it from",
                ));
            };
            // "Keep" must reproduce exactly what the record holds; a
            // definition this build cannot read has no credential to carry
            // forward, and inventing one would not be keeping.
            parse_rest_api_definition(&existing.definition)
                .map_err(|_| {
                    ServerError::conflict(
                        "the existing definition could not be read, so its credential cannot \
                         be kept; set or clear the credential instead",
                    )
                })?
                .credential
        }
        RestCredentialUpdate::Set(set) => {
            if set.value.is_empty() {
                return Err(ServerError::bad_request(
                    "credential value must not be empty",
                ));
            }
            // The key is derived from the record id — never taken from the
            // request — so this surface can only ever write its own secrets.
            let key = rest_credential_secret_key(id);
            state.secrets.set_secret(&key, &set.value).await?;
            Some(RestCredential {
                secret_name: key,
                placement: set.placement,
            })
        }
    };

    let definition = RestApiDefinition {
        base_url: body.base_url,
        catalog,
        credential,
    };
    let now = Utc::now();
    let record = ConnectedApp {
        id,
        name: body.name,
        kind: ConnectedAppKind::RestApi,
        definition: serde_json::to_value(&definition)
            .map_err(|error| ServerError::internal(format!("definition serialization: {error}")))?,
        created_at: created_at.unwrap_or(now),
        updated_at: now,
    };
    validate_connected_app(&record).map_err(ServerError::bad_request)?;

    match rest_records.iter_mut().find(|record| record.id == id) {
        Some(slot) => *slot = record,
        None => rest_records.push(record),
    }
    state
        .store
        .replace_connected_apps(ConnectedAppKind::RestApi, &rest_records)
        .await?;

    if clear_stored_value {
        // Best-effort, and only ever the derived key: a leftover value is
        // unreferenced and inert, while failing the request here would leave
        // the caller believing the clear did not happen when the record
        // already stopped referencing it.
        let _ = state
            .secrets
            .delete_secret(&rest_credential_secret_key(id))
            .await;
    }

    Ok(Json(connected_apps_info(&state).await?))
}

/// `DELETE /connected-apps/rest/{id}` — remove one `rest_api` connected app
/// and its stored credential.
///
/// Deliberately *not* refused on managed profiles, unlike the upsert:
/// removing a local record and deleting its stored credential only ever
/// narrows what the profile can reach — it is cleanup the lockdown wants,
/// never a widening — so refusing it would strand exactly the local
/// credentials the managed posture exists to retire.
pub async fn delete_rest_connected_app(
    State(state): State<AppState>,
    Path(id): Path<ConnectedAppId>,
) -> Result<StatusCode, ServerError> {
    let rest_records = rest_records(&state).await?;
    if !rest_records.iter().any(|record| record.id == id) {
        return Err(ServerError::not_found(format!(
            "no rest_api connected app {id}"
        )));
    }
    let remaining: Vec<ConnectedApp> = rest_records
        .into_iter()
        .filter(|record| record.id != id)
        .collect();
    state
        .store
        .replace_connected_apps(ConnectedAppKind::RestApi, &remaining)
        .await?;
    // Best-effort, after the record is gone: a failed secret delete must not
    // strand the record, and the derived key — never a name read out of the
    // definition — is the only secret this surface may touch.
    let _ = state
        .secrets
        .delete_secret(&rest_credential_secret_key(id))
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /connected-apps/rest/spec-preview` body: where the OpenAPI document
/// comes from. Externally tagged and closed: `{"url": …}` or
/// `{"document": …}`.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SpecPreviewSource {
    /// Fetch the document from this URL, with the executor's egress hygiene.
    Url(String),
    /// The raw JSON document, inline.
    Document(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPreviewRequest {
    pub source: SpecPreviewSource,
}

/// What a document declares, for the configuration form's operation picker.
/// Renderer-safe: ids, methods, paths, and truncated summaries only.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct SpecPreviewInfo {
    /// Hex SHA-256 of the raw document — the pin the upsert must carry back
    /// with a URL source.
    pub document_sha256: String,
    pub operations: Vec<SpecPreviewOperation>,
    /// Operations the document declares that cannot be selected (no
    /// well-formed `operationId`, an over-bound path, or a repeated id).
    pub unlistable: usize,
    /// Whether the operation list was cut at the inventory bound.
    pub truncated: bool,
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct SpecPreviewOperation {
    pub operation_id: String,
    /// Lowercase HTTP method, as a path item declares it.
    pub method: String,
    pub path: String,
    pub summary: Option<String>,
}

/// `POST /connected-apps/rest/spec-preview` — list what an OpenAPI document
/// declares so the form can offer an operation selection, without judging
/// whether the unselected remainder would ingest.
///
/// Refused on managed profiles like the upsert: this surface exists only to
/// configure local REST records, and the URL source performs egress.
pub async fn post_rest_spec_preview(
    State(state): State<AppState>,
    Json(body): Json<SpecPreviewRequest>,
) -> Result<Json<SpecPreviewInfo>, ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if policy.managed {
        return Err(managed_profile_refusal(
            "REST connected apps are managed by your organization's gateway",
        ));
    }
    let document = match &body.source {
        SpecPreviewSource::Document(inline) => inline.as_bytes().to_vec(),
        SpecPreviewSource::Url(url) => fetch_spec_document(url)
            .await
            .map_err(|error| ServerError::bad_request_kind("spec_fetch", error.to_string()))?,
    };
    let inventory = enumerate_openapi_operations(&document)
        .map_err(|error| ServerError::bad_request_kind("openapi_ingest", error.to_string()))?;
    Ok(Json(SpecPreviewInfo {
        document_sha256: inventory.document_sha256,
        operations: inventory
            .operations
            .into_iter()
            .map(|operation| SpecPreviewOperation {
                operation_id: operation.operation_id,
                method: operation.method.as_str().to_string(),
                path: operation.path,
                summary: operation.summary,
            })
            .collect(),
        unlistable: inventory.unlistable,
        truncated: inventory.truncated,
    }))
}

/// Every stored `rest_api` record, in storage order.
async fn rest_records(state: &AppState) -> Result<Vec<ConnectedApp>, ServerError> {
    Ok(state
        .store
        .list_connected_apps()
        .await?
        .into_iter()
        .filter(|record| record.kind == ConnectedAppKind::RestApi)
        .collect())
}

/// The combined listing behind `GET /connected-apps` and the upsert response.
async fn connected_apps_info(state: &AppState) -> Result<ConnectedAppsInfo, ServerError> {
    // `mcp_server` entries reuse the runtime's health projection, joined to
    // their record ids by namespace. A definition without a record id has
    // nothing bindings could name, so it is not listed here (the MCP page
    // still shows it).
    let ids: std::collections::BTreeMap<String, ConnectedAppId> = state
        .mcp
        .app_fingerprints()
        .await
        .into_iter()
        .map(|(id, fingerprint)| (fingerprint.name, id))
        .collect();
    let tool_names = state.mcp.tool_names().await;
    let servers = state.mcp.info().await.servers;
    // How many local mini-apps currently bind each record: distinct live
    // grants naming the id. The binding grammar forbids a grant naming one
    // connected app twice, so grants and apps count one-to-one.
    let mut used_by: std::collections::BTreeMap<ConnectedAppId, usize> =
        std::collections::BTreeMap::new();
    for grant in state.store.list_live_app_grants().await? {
        for binding in &grant.bindings {
            *used_by.entry(binding.app()).or_default() += 1;
        }
    }
    // Org-app display names by endpoint slug, so gateway-backed entries can
    // lead with the apps the organization granted instead of endpoint slugs.
    // Best-effort by design: fetched only when a gateway-backed record exists,
    // and any failure — signed out, unreachable, a gateway predating the apps
    // surface — degrades to entries without org-app names, never an error.
    let mut gateway_app_names: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    if servers
        .iter()
        .any(|server| server.definition.gateway_endpoint.is_some())
    {
        if let Ok(gateway_apps) = state.gateway.apps().await {
            for app in gateway_apps.apps.into_iter().filter(|app| app.enabled) {
                for slug in &app.mcp_endpoint_slugs {
                    gateway_app_names
                        .entry(slug.clone())
                        .or_default()
                        .push(app.name.clone());
                }
            }
        }
    }
    let mut apps: Vec<ConnectedAppInfo> = servers
        .into_iter()
        .filter_map(|server| {
            let id = *ids.get(&server.definition.name)?;
            let gateway_endpoint = server.definition.gateway_endpoint.clone();
            let gateway_apps = gateway_endpoint
                .as_ref()
                .and_then(|slug| gateway_app_names.get(slug).cloned())
                .unwrap_or_default();
            Some(ConnectedAppInfo::McpServer {
                id,
                tools: tool_names
                    .get(&server.definition.name)
                    .cloned()
                    .unwrap_or_default(),
                name: server.definition.name,
                health: server.health,
                tool_count: server.tool_count,
                diagnostic: server.diagnostic,
                gateway_endpoint,
                gateway_apps,
                used_by_app_count: used_by.get(&id).copied().unwrap_or(0),
            })
        })
        .collect();
    for record in rest_records(state).await? {
        // A record whose definition does not parse reads as not-configured —
        // the same fail-closed verdict grant enforcement gives it.
        let Ok(definition) = parse_rest_api_definition(&record.definition) else {
            continue;
        };
        let credential_status = match &definition.credential {
            None => RestCredentialStatus::None,
            Some(credential) => {
                // Presence only, never the value. A store read failure reads
                // as missing: prompting for re-entry is the safe direction.
                match state.secrets.get_secret(&credential.secret_name).await {
                    Ok(Some(_)) => RestCredentialStatus::Configured,
                    Ok(None) | Err(_) => RestCredentialStatus::Missing,
                }
            }
        };
        apps.push(ConnectedAppInfo::RestApi {
            id: record.id,
            name: record.name,
            base_url: definition.base_url,
            operation_count: definition.catalog.operations.len(),
            document_sha256: definition.catalog.document_sha256,
            credential_status,
            placement: definition.credential.map(|credential| credential.placement),
            updated_at: record.updated_at,
            used_by_app_count: used_by.get(&record.id).copied().unwrap_or(0),
        });
    }
    Ok(ConnectedAppsInfo { apps })
}
