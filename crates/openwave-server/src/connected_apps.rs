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

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use openwave_core::connected_app::ConnectedAppKind;
use openwave_core::id::{ConnectedAppId, HostRootId};
use openwave_core::local_app::AppGrantBinding;
use openwave_core::SecretProvider;

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

/// The current fingerprint of every configured connected app, across kinds.
///
/// `mcp_server` entries come from the live MCP runtime; `rest_api` entries
/// are computed from the stored records. A `rest_api` record whose definition
/// does not parse is skipped — it reads as not-configured, so every grant
/// naming it fails closed to re-consent rather than matching anything.
pub(crate) async fn current_app_fingerprints(
    state: &AppState,
) -> openwave_core::Result<BTreeMap<ConnectedAppId, AppFingerprint>> {
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
}

impl CurrentFingerprints {
    /// Whether one granted binding still pins what its target carries right
    /// now. A missing record or a disconnected folder is a mismatch, never a
    /// match.
    pub(crate) fn grant_binding_current(&self, binding: &AppGrantBinding) -> bool {
        match binding {
            AppGrantBinding::Tools(binding) => self
                .apps
                .get(&binding.app)
                .is_some_and(|app| app.fingerprint == binding.fingerprint),
            AppGrantBinding::Operations(binding) => self
                .apps
                .get(&binding.app)
                .is_some_and(|app| app.fingerprint == binding.fingerprint),
            AppGrantBinding::Folder(binding) => {
                self.folders.contains_key(&binding.folder)
                    && folder_fingerprint(binding.folder, binding.access) == binding.fingerprint
            }
        }
    }
}

/// Read the whole current fingerprint surface, live per request.
pub(crate) async fn current_fingerprints(
    state: &AppState,
) -> openwave_core::Result<CurrentFingerprints> {
    let apps = current_app_fingerprints(state).await?;
    let mut folders = BTreeMap::new();
    if let Some(host) = &state.host_folders {
        for folder in host.approved_roots().await? {
            folders.insert(folder.root_id, folder.display_name);
        }
    }
    Ok(CurrentFingerprints { apps, folders })
}

/// Every stored `rest_api` record whose definition parses, read live.
///
/// The consent computation needs the catalogs behind the fingerprints (to
/// check pinned operation ids) and invoke dispatch needs the whole
/// definition; both read through here so "configured" always means "parses
/// closed".
pub(crate) async fn current_rest_definitions(
    state: &AppState,
) -> openwave_core::Result<Vec<(ConnectedAppId, String, RestApiDefinition)>> {
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
