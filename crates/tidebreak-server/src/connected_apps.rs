//! The `rest_api` connected-app definition and the combined per-request
//! fingerprint lookup grant enforcement runs against.
//!
//! An `mcp_server` record's typed definition, validation, and fingerprint
//! live with the MCP runtime ([`crate::mcp_config`]); this module is the
//! same layer for the `rest_api` kind, plus the one lookup that spans both:
//! [`current_app_fingerprints`], the map the grant, invoke, and library
//! surfaces compare pinned fingerprints against. Everything reads live per
//! request — never cached across one — so enforcement always judges the
//! definition an id resolves to *now*.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use tidebreak_core::connected_app::ConnectedAppKind;
use tidebreak_core::id::{ConnectedAppId, HostRootId};
use tidebreak_core::local_app::AppGrantBinding;
use tidebreak_core::SecretProvider;

use crate::host_folders::folder_fingerprint;
use crate::openapi_catalog::OperationCatalog;
use crate::rest_executor::{
    CredentialPlacement, ReqwestRestTransport, RestApiTarget, RestCredential, RestExecuteError,
    RestExecutor, RestHostResolver, RestOperationRequest, RestOperationResponse, RestTransport,
    TokioRestHostResolver,
};
use crate::state::AppState;

/// The typed definition a `rest_api` connected-app record carries: where
/// operations execute, what the ingested catalog declares, and which stored
/// credential (if any) authenticates them.
///
/// Closed on both ends: unknown fields refuse to parse, so a record written
/// by a future shape is treated as not-configured rather than partially
/// honored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestApiDefinition {
    /// Base URL the operation path templates append to.
    pub base_url: String,
    /// The bounded operation catalog ingested from the app's OpenAPI
    /// document.
    pub catalog: OperationCatalog,
    /// Stored credential reference and placement, when the API needs one.
    /// The referenced value never appears in the definition.
    pub credential: Option<RestCredential>,
}

impl RestApiDefinition {
    /// The executor target this definition describes.
    pub(crate) fn target(&self) -> RestApiTarget {
        RestApiTarget {
            base_url: self.base_url.clone(),
            credential: self.credential.clone(),
        }
    }
}

/// The profile secret-store key a `rest_api` record's credential value lives
/// under, derived from the record id and nothing else.
///
/// Deriving the key server-side is what keeps the settings surface from ever
/// writing — or deleting — a secret another feature owns: a record cannot
/// point its credential at an arbitrary store key and have the CRUD routes
/// act on it. [`crate::secret_rehome`] enumerates these keys from the stored
/// records so re-homed profiles keep their REST credentials.
pub(crate) fn rest_credential_secret_key(id: ConnectedAppId) -> String {
    format!("connected_app.{id}.credential")
}

/// Parse a `rest_api` record's definition JSON, failing closed per record.
pub(crate) fn parse_rest_api_definition(
    definition: &serde_json::Value,
) -> Result<RestApiDefinition, String> {
    serde_json::from_value(definition.clone())
        .map_err(|error| format!("invalid rest_api definition: {error}"))
}

/// SHA-256 fingerprint of a `rest_api` definition as configured, the value an
/// app grant pins the bound connected app to.
///
/// The digest is taken over the UTF-8 bytes of a compact JSON object with
/// **exactly these keys, in exactly this order** (serde serializes struct
/// fields in declaration order, and every key is always present):
///
/// ```json
/// {"v":2,
///  "kind":"rest_api",
///  "base_url":string,
///  "document_sha256":string,
///  "credential_reference":string|null,
///  "placement":"bearer"|{"header":name}|null}
/// ```
///
/// `document_sha256` is the catalog's hash of the raw OpenAPI document bytes,
/// so the ingested operations enter the form only through the document they
/// came from — a re-serialization of the catalog cannot move the fingerprint.
/// `credential_reference` is the secret-store *name*: rotating the credential
/// value behind the same reference never changes what the user consented to,
/// while repointing the reference does. The value itself never enters the
/// form, so the fingerprint can never be a value oracle. `kind` roots the
/// form in the connected-app vocabulary so no two kinds can collide on a
/// canonical serialization, and `v:2` aligns with the `mcp_server` form's
/// current version.
///
/// **This canonical form is a compatibility surface.** Persisted grants store
/// the digest; changing the form (or the meaning of any field in it)
/// invalidates every existing grant and must bump `v`.
pub(crate) fn rest_api_fingerprint(definition: &RestApiDefinition) -> [u8; 32] {
    use sha2::Digest as _;

    #[derive(Serialize)]
    struct CanonicalDefinition<'a> {
        v: u32,
        kind: &'static str,
        base_url: &'a str,
        document_sha256: &'a str,
        credential_reference: Option<&'a str>,
        placement: Option<&'a CredentialPlacement>,
    }

    let canonical = CanonicalDefinition {
        v: 2,
        kind: "rest_api",
        base_url: &definition.base_url,
        document_sha256: &definition.catalog.document_sha256,
        credential_reference: definition
            .credential
            .as_ref()
            .map(|credential| credential.secret_name.as_str()),
        placement: definition
            .credential
            .as_ref()
            .map(|credential| &credential.placement),
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("a canonical definition serializes infallibly to JSON");
    sha2::Sha256::digest(&bytes).into()
}

/// One configured connected app's display name, kind, and current definition
/// fingerprint — the tuple grant enforcement compares per record id. For an
/// `mcp_server` app the name is also the namespace its tools mount under.
pub(crate) struct AppFingerprint {
    pub(crate) name: String,
    pub(crate) kind: ConnectedAppKind,
    pub(crate) fingerprint: [u8; 32],
}

/// SHA-256 fingerprint of one gateway connected app as the gateway currently
/// describes it — the value an app grant pins a gateway binding to.
///
/// The digest is taken over the UTF-8 bytes of a compact JSON object with
/// **exactly these keys, in exactly this order** (serde serializes struct
/// fields in declaration order, and every key is always present):
///
/// ```json
/// {"v":2,
///  "kind":"gateway_app",
///  "gateway_base_url":string,
///  "gateway_app_id":string,
///  "catalog_sha256":string}
/// ```
///
/// `gateway_base_url` is the origin of the gateway that served the app, so
/// re-pairing this profile to a different gateway makes every gateway grant
/// stale rather than silently carrying consent across deployments.
/// `catalog_sha256` is the hash of the operation catalog the gateway declared
/// for the app, so a re-ingested app re-prompts. Entitlement is deliberately
/// absent: it is the gateway's live predicate, re-evaluated on every call, and
/// losing it must fail the call rather than revoke the consent. No credential
/// enters the form because none exists locally — the gateway injects the
/// viewer's own. `kind` roots the form in the connected-app vocabulary so no
/// two kinds can collide on a canonical serialization, and `v:2` matches the
/// `rest_api` and `mcp_server` forms' current version.
///
/// **This canonical form is a compatibility surface.** Persisted grants store
/// the digest; changing the form (or the meaning of any field in it)
/// invalidates every existing grant and must bump `v`.
pub(crate) fn gateway_app_fingerprint(
    gateway_base_url: &str,
    gateway_app_id: &str,
    catalog_sha256: &str,
) -> [u8; 32] {
    use sha2::Digest as _;

    #[derive(Serialize)]
    struct CanonicalGatewayApp<'a> {
        v: u32,
        kind: &'static str,
        gateway_base_url: &'a str,
        gateway_app_id: &'a str,
        catalog_sha256: &'a str,
    }

    let canonical = CanonicalGatewayApp {
        v: 2,
        kind: "gateway_app",
        gateway_base_url,
        gateway_app_id,
        catalog_sha256,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("a canonical definition serializes infallibly to JSON");
    sha2::Sha256::digest(&bytes).into()
}

/// Hash of a gateway app's operation catalog, derived from the operation ids
/// it declares: lowercase hex SHA-256 over the sorted ids joined by newlines.
///
/// A local stand-in, not a protocol: the gateway's catalog read carries no
/// document hash today, so the ids it declares are the only stable
/// description of the catalog available to pin. Sorting makes the digest
/// independent of the order the gateway happens to list them in. When the
/// gateway serves a catalog-document hash, this derivation is replaced by it
/// — which moves every gateway fingerprint, so that change must bump the
/// canonical form's `v`.
pub(crate) fn catalog_sha256_from_operation_ids(operation_ids: &[String]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;

    let mut sorted: Vec<&str> = operation_ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let digest = sha2::Sha256::digest(sorted.join("\n").as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// One gateway connected app's display name, live operation catalog, and
/// current fingerprint — the gateway-side twin of [`AppFingerprint`], keyed
/// by the gateway's app id rather than a local record id.
///
/// `operation_ids` is the catalog the fingerprint was taken over, kept beside
/// it so consent can check a manifest's pins against what the gateway
/// currently declares — the gateway-side twin of the `rest_api` catalog
/// lookup, without a second live read.
pub(crate) struct GatewayAppFingerprint {
    pub(crate) name: String,
    pub(crate) operation_ids: Vec<String>,
    pub(crate) fingerprint: [u8; 32],
}

/// The current fingerprint of every configured connected app, across kinds.
///
/// `mcp_server` entries come from the live MCP runtime; `rest_api` entries
/// are computed from the stored records. A `rest_api` record whose definition
/// does not parse is skipped — it reads as not-configured, so every grant
/// naming it fails closed to re-consent rather than matching anything.
pub(crate) async fn current_app_fingerprints(
    state: &AppState,
) -> tidebreak_core::Result<BTreeMap<ConnectedAppId, AppFingerprint>> {
    let mut current: BTreeMap<ConnectedAppId, AppFingerprint> = state
        .mcp
        .app_fingerprints()
        .await
        .into_iter()
        .map(|(id, app)| {
            (
                id,
                AppFingerprint {
                    name: app.name,
                    kind: ConnectedAppKind::McpServer,
                    fingerprint: app.fingerprint,
                },
            )
        })
        .collect();
    for (id, record_name, definition) in current_rest_definitions(state).await? {
        current.insert(
            id,
            AppFingerprint {
                name: record_name,
                kind: ConnectedAppKind::RestApi,
                fingerprint: rest_api_fingerprint(&definition),
            },
        );
    }
    Ok(current)
}

/// The live surface grant enforcement compares pinned fingerprints against,
/// across connected apps and connected folders — the one lookup the grant,
/// invoke, and library surfaces share.
pub(crate) struct CurrentFingerprints {
    /// Configured connected apps by record id.
    pub(crate) apps: BTreeMap<ConnectedAppId, AppFingerprint>,
    /// Host-approved connected folders, root id → display name. Empty when
    /// this embedding has no host-folder seam, so every folder binding fails
    /// closed to "not connected".
    pub(crate) folders: BTreeMap<HostRootId, String>,
    /// Gateway connected apps this profile can currently read, by the
    /// gateway's app id. Empty when nothing readable answers — no gateway, no
    /// session, or no catalog — so every gateway binding fails closed to
    /// re-consent rather than matching a fingerprint over a catalog nobody
    /// read.
    pub(crate) gateway_apps: BTreeMap<String, GatewayAppFingerprint>,
    /// Whether a gateway session answered the catalog read at all. False
    /// means the profile has no session to ask — unmanaged, signed out, or a
    /// gateway too old to serve catalogs — which consent reports differently
    /// from a session that answered and did not name the app. Vacuously true
    /// when no gateway app was needed, since nothing was read.
    pub(crate) gateway_session: bool,
}

impl CurrentFingerprints {
    /// Whether one granted binding still pins what its target carries right
    /// now. A missing record, a disconnected folder, or a gateway app that
    /// does not currently read back is a mismatch, never a match.
    pub(crate) fn grant_binding_current(&self, binding: &AppGrantBinding) -> bool {
        match binding {
            AppGrantBinding::Operations(binding) => self
                .apps
                .get(&binding.app)
                .is_some_and(|app| app.fingerprint == binding.fingerprint),
            AppGrantBinding::Folder(binding) => {
                self.folders.contains_key(&binding.folder)
                    && folder_fingerprint(binding.folder, binding.access) == binding.fingerprint
            }
            AppGrantBinding::GatewayOperations(binding) => self
                .gateway_apps
                .get(&binding.gateway_app)
                .is_some_and(|app| app.fingerprint == binding.fingerprint),
        }
    }
}

/// Read the whole current fingerprint surface, live per request.
///
/// `needed_gateway_apps` bounds the gateway half: each gateway app's catalog
/// is a separate live read, so callers name exactly the ids the bindings they
/// are about to judge mention. An id nothing readable answers to is simply
/// absent from the result, which reads as stale.
pub(crate) async fn current_fingerprints(
    state: &AppState,
    needed_gateway_apps: &BTreeSet<String>,
) -> tidebreak_core::Result<CurrentFingerprints> {
    let apps = current_app_fingerprints(state).await?;
    let mut folders = BTreeMap::new();
    if let Some(host) = &state.host_folders {
        for folder in host.approved_roots().await? {
            folders.insert(folder.root_id, folder.display_name);
        }
    }
    let gateway = current_gateway_app_fingerprints(state, needed_gateway_apps).await;
    let gateway_session = gateway.is_some();
    Ok(CurrentFingerprints {
        apps,
        folders,
        gateway_apps: gateway.unwrap_or_default(),
        gateway_session,
    })
}

/// The gateway app ids a set of manifest bindings names.
pub(crate) fn gateway_apps_bound_by<'a>(
    bindings: impl IntoIterator<Item = &'a tidebreak_core::local_app::AppBinding>,
) -> BTreeSet<String> {
    bindings
        .into_iter()
        .filter_map(|binding| binding.gateway_app().map(str::to_owned))
        .collect()
}

/// The gateway app ids a set of grant bindings names.
pub(crate) fn gateway_apps_granted_by<'a>(
    bindings: impl IntoIterator<Item = &'a AppGrantBinding>,
) -> BTreeSet<String> {
    bindings
        .into_iter()
        .filter_map(|binding| binding.gateway_app().map(str::to_owned))
        .collect()
}

/// One gateway connected app's catalog as a live read describes it: the
/// display name and the operation ids the gateway declares — the fingerprint
/// input and the consent-sheet label, and nothing else. No URL, no credential
/// material, and no upstream detail crosses this seam.
pub(crate) struct GatewayAppCatalog {
    pub(crate) name: String,
    pub(crate) operation_ids: Vec<String>,
}

/// The live per-app gateway catalog read the fingerprint surface depends on.
///
/// A seam rather than a direct call into the gateway runtime for the reason
/// [`RestOperationDispatcher`] is one: the grant, invoke, and library surfaces
/// are driven end to end in tests, and they cannot each stand up an OAuth
/// session against a fake deployment to do it. Production is the gateway
/// runtime itself.
#[async_trait]
pub(crate) trait GatewayCatalogSource: Send + Sync {
    /// The gateway's deployment URL and the catalog of each requested app
    /// that is enabled, entitled, and catalog-readable.
    ///
    /// `None` means this profile has no gateway session to answer with at all
    /// — unmanaged, misconfigured, signed out, or a gateway too old to serve
    /// catalogs. A present pair with an id missing from the map means the
    /// session answered and that app is not bindable, which reads the same way
    /// downstream: fingerprints fail closed either way.
    async fn gateway_app_catalogs(
        &self,
        needed: &BTreeSet<String>,
    ) -> Option<(String, BTreeMap<String, GatewayAppCatalog>)>;
}

/// The current fingerprint of every readable gateway connected app among
/// `needed`, by the gateway's app id.
///
/// `None` carries the source's own `None` through: no session answered, as
/// distinct from a session that answered without naming an app.
async fn current_gateway_app_fingerprints(
    state: &AppState,
    needed: &BTreeSet<String>,
) -> Option<BTreeMap<String, GatewayAppFingerprint>> {
    if needed.is_empty() {
        return Some(BTreeMap::new());
    }
    let (base_url, catalogs) = state.gateway_catalogs.gateway_app_catalogs(needed).await?;
    Some(
        catalogs
            .into_iter()
            .map(|(id, catalog)| {
                let catalog_sha256 = catalog_sha256_from_operation_ids(&catalog.operation_ids);
                let fingerprint = gateway_app_fingerprint(&base_url, &id, &catalog_sha256);
                (
                    id,
                    GatewayAppFingerprint {
                        name: catalog.name,
                        operation_ids: catalog.operation_ids,
                        fingerprint,
                    },
                )
            })
            .collect(),
    )
}

/// Every stored `rest_api` record whose definition parses, read live.
///
/// The consent computation needs the catalogs behind the fingerprints (to
/// check pinned operation ids) and invoke dispatch needs the whole
/// definition; both read through here so "configured" always means "parses
/// closed".
pub(crate) async fn current_rest_definitions(
    state: &AppState,
) -> tidebreak_core::Result<Vec<(ConnectedAppId, String, RestApiDefinition)>> {
    Ok(state
        .store
        .list_connected_apps()
        .await?
        .into_iter()
        .filter(|record| record.kind == ConnectedAppKind::RestApi)
        .filter_map(|record| {
            parse_rest_api_definition(&record.definition)
                .ok()
                .map(|definition| (record.id, record.name, definition))
        })
        .collect())
}

/// Dispatch seam for one governed REST operation, so the invoke route can be
/// driven against a fake transport in tests while production always executes
/// through the real [`RestExecutor`] stack.
#[async_trait]
pub(crate) trait RestOperationDispatcher: Send + Sync {
    async fn dispatch(
        &self,
        target: &RestApiTarget,
        catalog: &OperationCatalog,
        request: &RestOperationRequest,
    ) -> Result<RestOperationResponse, RestExecuteError>;
}

#[async_trait]
impl<T: RestTransport, R: RestHostResolver> RestOperationDispatcher for RestExecutor<T, R> {
    async fn dispatch(
        &self,
        target: &RestApiTarget,
        catalog: &OperationCatalog,
        request: &RestOperationRequest,
    ) -> Result<RestOperationResponse, RestExecuteError> {
        self.execute(target, catalog, request, None).await
    }
}

/// The production dispatcher: the governed executor over the real transport
/// and resolver, resolving credentials from the given secret store.
pub(crate) fn governed_rest_dispatcher(
    secrets: Arc<dyn SecretProvider>,
) -> Arc<dyn RestOperationDispatcher> {
    Arc::new(RestExecutor::new(
        ReqwestRestTransport,
        TokioRestHostResolver,
        secrets,
    ))
}

/// One shared-app operation call, as the invoke route assembled it.
///
/// The field names are the gateway's own invoke vocabulary rather than a
/// second spelling of it: `gateway_app` is what crosses the wire as
/// `connected_app_id`, and the three passthrough halves are opaque JSON the
/// server never interprets — a bundle authored against a harness frame speaks
/// exactly this shape to the gateway shell.
pub(crate) struct GatewayOperationRequest {
    /// The gateway connected app whose operation is being called.
    pub gateway_app: String,
    /// The operation id, as the app's catalog declares it.
    pub operation_id: String,
    /// Path-template values for the operation, when it takes any.
    pub path_parameters: Option<serde_json::Value>,
    /// Query values for the operation, when it takes any.
    pub query: Option<serde_json::Value>,
    /// JSON request body, when the operation declares one.
    pub body: Option<serde_json::Value>,
}

/// Why a gateway relay could not happen at all — as distinct from a call the
/// gateway answered, which is a [`GatewayInvokeOutcome`].
///
/// Closed on purpose: the invoke route turns each of these into a typed
/// refusal, and a fourth reading would need its own refusal kind rather than a
/// free-form message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GatewayDispatchError {
    /// This profile has no gateway session to relay as — unmanaged, signed
    /// out, or pointed at a deployment it never signed into.
    NoSession,
    /// Nothing at the gateway answers for this app: no draft is registered
    /// for it, or the deployment predates the shared-app invoke route.
    NotRegistered,
    /// The gateway was reachable in principle but the call failed —
    /// transport, protocol, or an answer this client could not read. The
    /// message is host-authored and bounded; it carries no URL and no
    /// credential material.
    Unreachable(String),
}

/// Dispatch seam for one gateway shared-app relay, so the invoke route can be
/// driven end to end in tests without an OAuth session against a fake
/// deployment — the gateway twin of [`RestOperationDispatcher`]. Production is
/// the relay over the one gateway runtime.
#[async_trait]
pub(crate) trait GatewayInvokeDispatcher: Send + Sync {
    /// Relay `request` on behalf of the local app `app`, which the route has
    /// already checked the pin, the grant, and the fingerprint currency of.
    async fn dispatch(
        &self,
        owner: &tidebreak_core::OwnerId,
        app: tidebreak_core::id::AppId,
        request: &GatewayOperationRequest,
    ) -> Result<crate::connectors::GatewayInvokeOutcome, GatewayDispatchError>;
}

/// The registration lifecycle of one local app at one gateway deployment.
///
/// A local app's gateway bindings are relayed to a shared app the gateway
/// holds, so the app has to exist there before anything can be relayed. This
/// seam is what establishes and advances that, keyed by the deployment as
/// well as the app: a registration belongs to one gateway, so a re-paired
/// profile has no registration there, exactly as it has no current gateway
/// grant. Production is the store-backed
/// [`crate::gateway_drafts::GatewayDraftRegistry`]; tests drive the whole
/// ladder against a fake without an OAuth session.
#[async_trait]
pub(crate) trait GatewayDraftSource: Send + Sync {
    /// Ensure `app` is registered at `gateway_base_url` and that the revision
    /// the gateway serves is the app's current local one, registering or
    /// appending as needed.
    async fn ensure_registered(
        &self,
        owner: &tidebreak_core::OwnerId,
        app: tidebreak_core::id::AppId,
        gateway_base_url: &str,
    ) -> tidebreak_core::Result<GatewayRegistration>;

    /// Relay the author's consent for `app`'s registration, pinned to the
    /// revision the gateway is serving.
    ///
    /// Safe to relay without asking again because the local grant ladder has
    /// already run: the consent sheet displayed exactly the binding set this
    /// names, and the gateway computes the consented bindings server-side
    /// from the live revision, accepting only a revision pin from here.
    async fn relay_consent(
        &self,
        owner: &tidebreak_core::OwnerId,
        app: tidebreak_core::id::AppId,
        gateway_base_url: &str,
    ) -> tidebreak_core::Result<GatewayConsentRelay>;
}

/// Where a local app stands at one gateway deployment.
///
/// Closed on purpose, and deliberately not a `Result`: "this deployment does
/// not hold shared apps" and "the gateway said no" are both answers, and the
/// invoke ladder turns each into a different refusal. A gateway that could
/// not be reached at all is the `Err` half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GatewayRegistration {
    /// The gateway holds the app, serving `revision_id`.
    Registered {
        shared_app_id: String,
        revision_id: String,
    },
    /// Nothing at this deployment holds the app and nothing there can: the
    /// gateway predates shared-app registration, or it holds no app for this
    /// user to append to.
    NotRegistered,
    /// The gateway refused to register or advance the app, in its own words.
    Refused { message: String },
}

/// What relaying the author's consent came back as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GatewayConsentRelay {
    /// The gateway recorded consent for the registered revision.
    Consented,
    /// Nothing at this deployment holds the app — see
    /// [`GatewayRegistration::NotRegistered`].
    NotRegistered,
    /// The gateway refused the consent, in its own words.
    Refused { message: String },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn definition(
        base_url: &str,
        document_sha256: &str,
        credential: Option<RestCredential>,
    ) -> RestApiDefinition {
        RestApiDefinition {
            base_url: base_url.into(),
            catalog: OperationCatalog {
                document_sha256: document_sha256.into(),
                operations: BTreeMap::new(),
            },
            credential,
        }
    }

    /// The invariants the canonical form exists for: the fingerprint follows
    /// the credential *reference* (never the value, which is not even
    /// representable in the definition), it follows repointing that
    /// reference, and it derives from exactly base URL + document hash +
    /// reference + placement — catalog operations beyond the document hash
    /// do not enter the form.
    #[test]
    fn rest_fingerprints_pin_references_and_derive_from_declared_fields_only() {
        let credential = |secret_name: &str, placement: CredentialPlacement| RestCredential {
            secret_name: secret_name.into(),
            placement,
        };
        let baseline = rest_api_fingerprint(&definition(
            "https://api.example.com/v2",
            "doc-hash-a",
            Some(credential("sentry-token", CredentialPlacement::Bearer)),
        ));

        // Rotating the credential VALUE behind the same reference is not a
        // definition change at all: the value never appears in the
        // definition, so the same definition always fingerprints the same.
        assert_eq!(
            rest_api_fingerprint(&definition(
                "https://api.example.com/v2",
                "doc-hash-a",
                Some(credential("sentry-token", CredentialPlacement::Bearer)),
            )),
            baseline
        );

        // Repointing the reference, moving the placement, dropping the
        // credential, changing the base URL, or swapping the document all
        // change what the user consented to.
        for changed in [
            definition(
                "https://api.example.com/v2",
                "doc-hash-a",
                Some(credential("other-token", CredentialPlacement::Bearer)),
            ),
            definition(
                "https://api.example.com/v2",
                "doc-hash-a",
                Some(credential(
                    "sentry-token",
                    CredentialPlacement::Header("X-Api-Key".into()),
                )),
            ),
            definition("https://api.example.com/v2", "doc-hash-a", None),
            definition(
                "https://api.example.com/v3",
                "doc-hash-a",
                Some(credential("sentry-token", CredentialPlacement::Bearer)),
            ),
            definition(
                "https://api.example.com/v2",
                "doc-hash-b",
                Some(credential("sentry-token", CredentialPlacement::Bearer)),
            ),
        ] {
            assert_ne!(rest_api_fingerprint(&changed), baseline, "{changed:?}");
        }

        // Catalog operations enter the form only through the document hash: a
        // catalog carrying extra parsed operations under the same document
        // hash fingerprints identically.
        let mut with_operations = definition(
            "https://api.example.com/v2",
            "doc-hash-a",
            Some(credential("sentry-token", CredentialPlacement::Bearer)),
        );
        with_operations.catalog.operations.insert(
            "listIssues".into(),
            crate::openapi_catalog::CatalogOperation {
                operation_id: "listIssues".into(),
                method: crate::openapi_catalog::HttpMethod::Get,
                path_template: "/issues".into(),
                parameters: Vec::new(),
                request_body: None,
            },
        );
        assert_eq!(rest_api_fingerprint(&with_operations), baseline);
    }

    /// The gateway canonical form pins exactly the three inputs the record
    /// names: the gateway origin, the app id, and the catalog hash. Moving
    /// any one moves the digest, and the catalog hash follows the declared
    /// operation ids as a set rather than the order they arrive in.
    #[test]
    fn gateway_fingerprints_derive_from_origin_app_and_catalog_only() {
        let ids = |ids: &[&str]| -> Vec<String> { ids.iter().map(|id| (*id).to_owned()).collect() };
        let catalog = catalog_sha256_from_operation_ids(&ids(&["listIssues", "getIssue"]));
        let baseline = gateway_app_fingerprint("https://gw.example.com", "app-1", &catalog);

        // The same three inputs always fingerprint the same, and the catalog
        // hash is order-independent: the gateway may list operations in any
        // order without re-prompting the user.
        assert_eq!(
            gateway_app_fingerprint(
                "https://gw.example.com",
                "app-1",
                &catalog_sha256_from_operation_ids(&ids(&["getIssue", "listIssues"])),
            ),
            baseline
        );

        // Re-pairing to another gateway, naming another app, or a catalog
        // that gained, lost, or renamed an operation each change what the
        // user consented to.
        for changed in [
            gateway_app_fingerprint("https://other.example.com", "app-1", &catalog),
            gateway_app_fingerprint("https://gw.example.com", "app-2", &catalog),
            gateway_app_fingerprint(
                "https://gw.example.com",
                "app-1",
                &catalog_sha256_from_operation_ids(&ids(&["listIssues"])),
            ),
            gateway_app_fingerprint(
                "https://gw.example.com",
                "app-1",
                &catalog_sha256_from_operation_ids(&ids(&[
                    "listIssues",
                    "getIssue",
                    "createIssue",
                ])),
            ),
        ] {
            assert_ne!(changed, baseline);
        }

        // No form collides across kinds even when the inputs coincide: the
        // `kind` key roots each canonical form in its own vocabulary.
        assert_ne!(
            baseline,
            rest_api_fingerprint(&definition("https://gw.example.com", &catalog, None))
        );
    }

    /// A gateway grant reads stale against a surface that answers nothing:
    /// an unread gateway map is the shape a profile with no session (or an
    /// app that lost its entitlement) produces, and a missing entry is a
    /// mismatch — so the binding re-prompts rather than matching.
    #[test]
    fn gateway_grants_read_stale_against_an_unread_gateway_surface() {
        let current = CurrentFingerprints {
            apps: BTreeMap::new(),
            folders: BTreeMap::new(),
            gateway_apps: BTreeMap::new(),
            gateway_session: false,
        };
        let granted = |fingerprint: [u8; 32]| {
            AppGrantBinding::GatewayOperations(
                tidebreak_core::local_app::AppGatewayOperationsGrantBinding {
                    gateway_app: "app-1".into(),
                    operation_ids: vec!["listIssues".into()],
                    fingerprint,
                },
            )
        };
        assert!(
            !current.grant_binding_current(&granted(gateway_app_fingerprint(
                "https://gw.example.com",
                "app-1",
                &catalog_sha256_from_operation_ids(&["listIssues".to_owned()]),
            )))
        );
        assert!(!current.grant_binding_current(&granted([0; 32])));
    }

    /// The definition fails closed: unknown fields, missing fields, and a
    /// non-object all refuse, and the executor's credential shapes are reused
    /// verbatim.
    #[test]
    fn definitions_parse_closed() {
        let parsed = parse_rest_api_definition(&json!({
            "base_url": "https://api.example.com",
            "catalog": { "document_sha256": "abc", "operations": {} },
            "credential": { "secret_name": "token", "placement": "bearer" },
        }))
        .unwrap();
        assert_eq!(
            parsed.credential,
            Some(RestCredential {
                secret_name: "token".into(),
                placement: CredentialPlacement::Bearer,
            })
        );
        for malformed in [
            json!({ "base_url": "https://api.example.com" }),
            json!({
                "base_url": "https://api.example.com",
                "catalog": { "document_sha256": "abc", "operations": {} },
                "extra": true,
            }),
            json!([]),
        ] {
            assert!(
                parse_rest_api_definition(&malformed).is_err(),
                "{malformed}"
            );
        }
    }
}
