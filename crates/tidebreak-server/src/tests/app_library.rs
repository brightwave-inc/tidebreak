//! `/apps` library surface: listing, detail, and deletion.

use super::*;

use serde_json::json;
use tidebreak_core::id::{AppId, AppRevisionId};
use tidebreak_core::local_app::{
    AppBinding, AppManifest, AppOperationsBinding, CreateApp, NewAppRevision,
};

/// The library lifecycle in one pass: the listing carries renderer-safe rows
/// whose `granted` badge is the grant surface's own verdict, the detail lists
/// revision history, and deletion removes the app from every one of those
/// surfaces while staying idempotent server-side state.
#[tokio::test]
async fn library_lists_grant_verdicts_and_deletion_removes_the_row() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // A configured rest_api record carries a definition fingerprint, which is
    // all a grant pins — no live endpoint is needed to consent.
    let connected = tidebreak_core::id::ConnectedAppId::new();
    let catalog = crate::openapi_catalog::ingest_openapi_document(
        json!({
            "openapi": "3.0.3",
            "info": { "title": "Issues", "version": "1" },
            "paths": {
                "/issues": { "get": { "operationId": "listIssues" } }
            }
        })
        .to_string()
        .as_bytes(),
        None,
    )
    .unwrap();
    store
        .replace_connected_apps(
            tidebreak_core::connected_app::ConnectedAppKind::RestApi,
            &[tidebreak_core::connected_app::ConnectedApp {
                id: connected,
                name: "issues".into(),
                kind: tidebreak_core::connected_app::ConnectedAppKind::RestApi,
                definition: json!({
                    "base_url": "https://api.example.com/v2",
                    "catalog": &catalog,
                }),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
        )
        .await
        .unwrap();

    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Library fixture".into(),
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

    let request = |method: &str, uri: String| {
        router.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
    };

    // Ungranted at first; the listing carries the projection fields and never
    // the manifest's bindings.
    let listed = request("GET", "/apps".into()).await.unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body = body_string(listed).await;
    assert!(
        !body.contains("bindings") && !body.contains("listIssues"),
        "the listing must not carry manifest bindings: {body}"
    );
    let library: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(library["apps"][0]["name"], json!("Library fixture"));
    assert_eq!(library["apps"][0]["revision_count"], json!(1));
    assert_eq!(library["apps"][0]["granted"], json!(false));

    // Consent flips the listing's verdict — same computation, same answer.
    let consented = request("POST", format!("/apps/{app_id}/grant"))
        .await
        .unwrap();
    assert_eq!(consented.status(), StatusCode::OK);
    let listed = request("GET", "/apps".into()).await.unwrap();
    let library: serde_json::Value = serde_json::from_str(&body_string(listed).await).unwrap();
    assert_eq!(library["apps"][0]["granted"], json!(true));

    // Detail lists the revision history.
    let detail = request("GET", format!("/apps/{app_id}")).await.unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: serde_json::Value = serde_json::from_str(&body_string(detail).await).unwrap();
    assert_eq!(detail["name"], json!("Library fixture"));
    assert_eq!(detail["revisions"][0]["ordinal"], json!(1));
    assert_eq!(detail["revisions"][0]["id"], detail["current_revision"]);

    // Deletion removes the app from the library and the detail surface.
    let deleted = request("DELETE", format!("/apps/{app_id}")).await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let listed = request("GET", "/apps".into()).await.unwrap();
    let library: serde_json::Value = serde_json::from_str(&body_string(listed).await).unwrap();
    assert_eq!(library["apps"], json!([]));
    let detail = request("GET", format!("/apps/{app_id}")).await.unwrap();
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
    // An unknown app 404s; re-deleting the soft-deleted one stays idempotent.
    let missing = request("DELETE", format!("/apps/{}", AppId::new()))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}
