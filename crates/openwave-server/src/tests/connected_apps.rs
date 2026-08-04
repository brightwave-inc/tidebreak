//! `/connected-apps`: the Settings CRUD surface for `rest_api` connected
//! apps and the combined per-kind listing.
//!
//! What these pin: configuration-time ingest (a bad document persists
//! nothing), the credential lifecycle (set / keep / none, the derived secret
//! key, and the re-home enumeration that keeps it), the managed-profile
//! posture (upsert refused, delete allowed), and that no response on this
//! surface ever carries a credential value.

use super::*;

use openwave_core::connected_app::ConnectedAppKind;
use openwave_core::id::{AppId, AppRevisionId, ConnectedAppId};
use openwave_core::local_app::{
    AppBinding, AppManifest, AppOperationsBinding, CreateApp, NewAppRevision,
};
use serde_json::json;

const CREDENTIAL_VALUE: &str = "sk-rest-credential-hunter2";

/// An authenticated API whose state (store + secret store) stays in reach.
async fn connected_apps_test_app() -> (Router, String, AppState, tempfile::TempDir) {
    let (dir, store) = temp_db_store("connected-apps.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    (app(state.clone()), bearer, state, dir)
}

/// An OpenAPI document declaring `listIssues` and `getIssue`.
fn issues_spec() -> String {
    json!({
        "openapi": "3.0.3",
        "info": { "title": "Issues", "version": "1" },
        "paths": {
            "/issues": {
                "get": { "operationId": "listIssues" }
            },
            "/issues/{id}": {
                "get": {
                    "operationId": "getIssue",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true,
                          "schema": { "type": "string" } }
                    ]
                }
            }
        }
    })
    .to_string()
}

fn upsert_body(base_url: &str, credential: serde_json::Value) -> String {
    json!({
        "name": "issues",
        "base_url": base_url,
        "openapi_document": issues_spec(),
        "credential": credential,
    })
    .to_string()
}

async fn put_rest(
    router: &Router,
    bearer: &str,
    id: ConnectedAppId,
    body: String,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/connected-apps/rest/{id}"))
                .header("authorization", bearer)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_listing(router: &Router, bearer: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/connected-apps")
                .header("authorization", bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn delete_rest(
    router: &Router,
    bearer: &str,
    id: ConnectedAppId,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/connected-apps/rest/{id}"))
                .header("authorization", bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn raw_body(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The one `rest_api` entry of a parsed listing body.
fn rest_entry(listing: &serde_json::Value) -> &serde_json::Value {
    listing["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "rest_api")
        .expect("a rest_api entry is listed")
}

/// The credential lifecycle over the API: `set` stores under the derived key
/// (which the re-home enumeration must include), `keep` preserves it across
/// an edit, `none` deletes it, and DELETE removes record and secret together.
/// Every response along the way is asserted on raw bytes to never carry the
/// credential value. The listing also projects a configured `mcp_server`
/// record beside the REST entry.
#[tokio::test]
async fn rest_credential_lifecycle_walks_set_keep_none_and_delete() {
    let (router, bearer, state, _dir) = connected_apps_test_app().await;
    let id = ConnectedAppId::new();
    let key = format!("connected_app.{id}.credential");

    // A disabled stdio server (never spawns) so the listing carries both kinds.
    let mcp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header("authorization", &bearer)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"servers": [{
                        "name": "docs",
                        "command": "/opt/docs-mcp",
                        "enabled": false
                    }]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mcp.status(), StatusCode::OK);

    // Set: ingest + persist + secret under the derived key.
    let put = put_rest(
        &router,
        &bearer,
        id,
        upsert_body(
            "https://api.example.com/v2",
            json!({ "set": { "value": CREDENTIAL_VALUE, "placement": "bearer" } }),
        ),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    let put_body = raw_body(put).await;
    assert!(
        !put_body.contains(CREDENTIAL_VALUE),
        "the credential value must never be echoed: {put_body}"
    );
    assert_eq!(
        state.secrets.get_secret(&key).await.unwrap().as_deref(),
        Some(CREDENTIAL_VALUE)
    );
    // The re-home pass enumerates the derived key from the stored record —
    // the guarantee that keeps re-homed profiles from losing REST credentials.
    assert!(secret_rehome::stored_secret_keys(&*state.store)
        .await
        .unwrap()
        .contains(&key));

    let listing_body = raw_body(get_listing(&router, &bearer).await).await;
    assert!(!listing_body.contains(CREDENTIAL_VALUE), "{listing_body}");
    let listing: serde_json::Value = serde_json::from_str(&listing_body).unwrap();
    let entry = rest_entry(&listing);
    assert_eq!(entry["id"], json!(id.to_string()));
    assert_eq!(entry["name"], json!("issues"));
    assert_eq!(entry["base_url"], json!("https://api.example.com/v2"));
    assert_eq!(entry["operation_count"], json!(2));
    assert_eq!(entry["credential_status"], json!("configured"));
    assert_eq!(entry["placement"], json!("bearer"));
    assert!(entry["document_sha256"].as_str().unwrap().len() == 64);
    assert!(
        listing["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "mcp_server" && entry["name"] == "docs"),
        "the listing carries both kinds: {listing_body}"
    );

    // Keep: an edit that does not touch the credential preserves it.
    let keep = put_rest(
        &router,
        &bearer,
        id,
        upsert_body("https://api.example.com/v3", json!("keep")),
    )
    .await;
    assert_eq!(keep.status(), StatusCode::OK);
    let listing: serde_json::Value =
        serde_json::from_str(&raw_body(get_listing(&router, &bearer).await).await).unwrap();
    assert_eq!(
        rest_entry(&listing)["credential_status"],
        json!("configured")
    );
    assert_eq!(
        state.secrets.get_secret(&key).await.unwrap().as_deref(),
        Some(CREDENTIAL_VALUE)
    );

    // None: the reference is cleared and the stored value deleted.
    let none = put_rest(
        &router,
        &bearer,
        id,
        upsert_body("https://api.example.com/v3", json!("none")),
    )
    .await;
    assert_eq!(none.status(), StatusCode::OK);
    assert_eq!(state.secrets.get_secret(&key).await.unwrap(), None);
    let listing: serde_json::Value =
        serde_json::from_str(&raw_body(get_listing(&router, &bearer).await).await).unwrap();
    assert_eq!(rest_entry(&listing)["credential_status"], json!("none"));

    // Delete removes the record and its secret; a second delete is a 404.
    state
        .secrets
        .set_secret(&key, CREDENTIAL_VALUE)
        .await
        .unwrap();
    assert_eq!(
        delete_rest(&router, &bearer, id).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(state.secrets.get_secret(&key).await.unwrap(), None);
    assert!(state
        .store
        .list_connected_apps()
        .await
        .unwrap()
        .iter()
        .all(|record| record.kind != ConnectedAppKind::RestApi));
    assert_eq!(
        delete_rest(&router, &bearer, id).await.status(),
        StatusCode::NOT_FOUND
    );
}

/// A minimal streamable-HTTP MCP fixture advertising one tool whose
/// remote-authored description must never reach the settings listing.
async fn serve_fake_http_mcp() -> std::net::SocketAddr {
    use axum::routing::post;

    async fn handler(body: String) -> ([(&'static str, &'static str); 1], String) {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "listing-fixture", "version": "1"}
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "lookup",
                    "description": "Remote-authored prose that stays out of settings",
                    "inputSchema": {"type": "object"}
                }]
            }),
            _ => json!({}),
        };
        (
            [("content-type", "application/json")],
            json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
    }

    let app = axum::Router::new().route("/mcp", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

/// The listing enumerates a mounted server's tool names — and only names:
/// the remote-authored tool description never crosses to this renderer
/// surface. A local record also reads as non-gateway (`gateway_endpoint`
/// null) with no org-app names.
#[tokio::test]
async fn the_listing_carries_mounted_tool_names_only() {
    let (router, bearer, _state, _dir) = connected_apps_test_app().await;
    let address = serve_fake_http_mcp().await;

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header("authorization", &bearer)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"servers": [{
                        "name": "docs",
                        "url": format!("http://{address}/mcp")
                    }]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let body = raw_body(get_listing(&router, &bearer).await).await;
    assert!(
        !body.contains("Remote-authored"),
        "tool descriptions must never reach the listing: {body}"
    );
    let listing: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entry = listing["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "mcp_server" && entry["name"] == "docs")
        .expect("the mounted server is listed");
    assert_eq!(entry["tools"], json!(["lookup"]));
    assert_eq!(entry["gateway_endpoint"], json!(null));
    assert_eq!(entry["gateway_apps"], json!([]));
}

/// Configuration-time refusals persist nothing: a rejected document (or an
/// inadmissible base URL) leaves no record and stores no credential value.
#[tokio::test]
async fn a_refused_upsert_persists_nothing() {
    let (router, bearer, state, _dir) = connected_apps_test_app().await;
    let id = ConnectedAppId::new();

    let swagger = json!({
        "name": "issues",
        "base_url": "https://api.example.com",
        "openapi_document": json!({"swagger": "2.0", "paths": {}}).to_string(),
        "credential": { "set": { "value": CREDENTIAL_VALUE, "placement": "bearer" } },
    })
    .to_string();
    let refused = put_rest(&router, &bearer, id, swagger).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = serde_json::from_str(&raw_body(refused).await).unwrap();
    assert_eq!(body["kind"], json!("openapi_ingest"));
    assert!(body["message"].as_str().unwrap().contains("Swagger 2.0"));

    let http = put_rest(
        &router,
        &bearer,
        id,
        json!({
            "name": "issues",
            "base_url": "http://api.example.com",
            "openapi_document": issues_spec(),
            "credential": "none",
        })
        .to_string(),
    )
    .await;
    assert_eq!(http.status(), StatusCode::BAD_REQUEST);

    assert!(state.store.list_connected_apps().await.unwrap().is_empty());
    assert_eq!(
        state
            .secrets
            .get_secret(&format!("connected_app.{id}.credential"))
            .await
            .unwrap(),
        None
    );
}

/// The managed posture: `rest_api` upserts are refused wholesale with the
/// stable `managed_profile` kind — local credential entry is what the
/// lockdown closes — while DELETE stays available, because removing a local
/// record and its credential only ever narrows.
#[tokio::test]
async fn managed_profiles_refuse_rest_upserts_but_allow_delete() {
    let (router, bearer, state, _dir) = connected_apps_test_app().await;
    let id = ConnectedAppId::new();

    let seeded = put_rest(
        &router,
        &bearer,
        id,
        upsert_body(
            "https://api.example.com/v2",
            json!({ "set": { "value": CREDENTIAL_VALUE, "placement": { "header": "X-Api-Key" } } }),
        ),
    )
    .await;
    assert_eq!(seeded.status(), StatusCode::OK);

    crate::managed_policy::provision(&*state.store, "https://corp.gateway")
        .await
        .unwrap();

    let refused = put_rest(
        &router,
        &bearer,
        id,
        upsert_body("https://api.example.com/v2", json!("keep")),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = serde_json::from_str(&raw_body(refused).await).unwrap();
    assert_eq!(body["kind"], json!("managed_profile"));

    assert_eq!(
        delete_rest(&router, &bearer, id).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(state.store.list_connected_apps().await.unwrap().is_empty());
}

/// A local app with one `rest_api` binding, created straight through the
/// store so grant tests don't route through the authoring surface.
fn bound_app(name: &str, connected: ConnectedAppId) -> CreateApp {
    CreateApp {
        id: AppId::new(),
        revision: NewAppRevision {
            id: AppRevisionId::new(),
            manifest: AppManifest {
                name: name.into(),
                bindings: vec![AppBinding::Operations(AppOperationsBinding {
                    app: connected,
                    operation_ids: vec!["listIssues".into()],
                })],
            },
            byte_len: 1,
            sha256: [0; 32],
            turn_id: None,
            producing_run_id: None,
            chat_id: None,
            created_at: chrono::Utc::now(),
        },
    }
}

async fn consent(router: &Router, bearer: &str, app_id: AppId) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{app_id}/grant"))
                .header("authorization", bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The listing counts the local apps whose live grant binds each record —
/// 0 before any consent, the granted count after, minus apps deleted from
/// the library — and projects the count only: no local-app name or id ever
/// reaches this surface.
#[tokio::test]
async fn the_listing_counts_local_apps_bound_to_each_record() {
    let (router, bearer, state, _dir) = connected_apps_test_app().await;
    let connected = ConnectedAppId::new();
    let put = put_rest(
        &router,
        &bearer,
        connected,
        upsert_body("https://api.example.com/v2", json!("none")),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);

    // No grants yet: the count is zero, not absent.
    let listing: serde_json::Value =
        serde_json::from_str(&raw_body(get_listing(&router, &bearer).await).await).unwrap();
    assert_eq!(rest_entry(&listing)["used_by_app_count"], json!(0));

    let first = bound_app("Issues board fixture", connected);
    let second = bound_app("Issues digest fixture", connected);
    let (first_id, second_id) = (first.id, second.id);
    state.store.create_app(&first).await.unwrap();
    state.store.create_app(&second).await.unwrap();
    consent(&router, &bearer, first_id).await;
    consent(&router, &bearer, second_id).await;

    let body = raw_body(get_listing(&router, &bearer).await).await;
    assert!(
        !body.contains("fixture") && !body.contains(&first_id.to_string()),
        "the listing must carry a count only, never local-app names or ids: {body}"
    );
    let listing: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rest_entry(&listing)["used_by_app_count"], json!(2));

    // A grant of a library-deleted app can no longer be exercised, so it
    // stops counting without an explicit revocation.
    assert!(state
        .store
        .delete_app(second_id, chrono::Utc::now())
        .await
        .unwrap());
    let listing: serde_json::Value =
        serde_json::from_str(&raw_body(get_listing(&router, &bearer).await).await).unwrap();
    assert_eq!(rest_entry(&listing)["used_by_app_count"], json!(1));
}

/// Editing a record's base URL through the settings route moves its
/// fingerprint, so an app grant pinned to the old definition reads ungranted
/// on the very next check — the same invalidation the invoke gate applies.
#[tokio::test]
async fn editing_a_rest_record_invalidates_an_existing_app_grant() {
    let (router, bearer, state, _dir) = connected_apps_test_app().await;
    let connected = ConnectedAppId::new();
    let put = put_rest(
        &router,
        &bearer,
        connected,
        upsert_body("https://api.example.com/v2", json!("none")),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);

    let app_id = AppId::new();
    state
        .store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Issues fixture".into(),
                    bindings: vec![AppBinding::Operations(AppOperationsBinding {
                        app: connected,
                        operation_ids: vec!["listIssues".into()],
                    })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();

    let grant_uri = format!("/apps/{app_id}/grant");
    let consent = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&grant_uri)
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(consent.status(), StatusCode::OK);
    let granted: serde_json::Value = json_body(consent).await;
    assert_eq!(granted["granted"], json!(true));

    let edited = put_rest(
        &router,
        &bearer,
        connected,
        upsert_body("https://api.elsewhere.example/v2", json!("none")),
    )
    .await;
    assert_eq!(edited.status(), StatusCode::OK);

    let state_after = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&grant_uri)
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state_after.status(), StatusCode::OK);
    let after: serde_json::Value = json_body(state_after).await;
    assert_eq!(after["granted"], json!(false));
}
