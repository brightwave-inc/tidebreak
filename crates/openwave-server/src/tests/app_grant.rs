//! `/apps/{id}/grant`: the renderer-facing consent surface.
//!
//! The lifecycle (consent → invoke → revoke, staleness on reconfiguration,
//! widened-manifest re-consent) is covered end to end in
//! [`super::app_invoke`]; this module pins the projection contract — what the
//! grant endpoints may and may not put on the wire.

use super::*;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    AppBinding, AppManifest, AppToolsBinding, CreateApp, NewAppRevision,
};
use serde_json::json;

async fn grant_request(
    router: &Router,
    bearer: &str,
    method: &str,
    app_id: AppId,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/apps/{app_id}/grant"))
                .header("authorization", bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn create_app_bound_to(
    store: &Arc<dyn Store>,
    app: openwave_core::id::ConnectedAppId,
    tools: &[&str],
) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Grant fixture".into(),
                    bindings: vec![AppBinding::Tools(AppToolsBinding {
                        app,
                        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
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
    app_id
}

/// The consent surface carries names only. A command server's definition —
/// executable, arguments, literal environment values, selected environment
/// names — must never appear in any grant response, asserted on the raw
/// serialized JSON rather than a parsed projection so nothing can hide in an
/// unmodeled field. Consent itself now conflicts — tool bindings are retired
/// (#1332) — and the refusal must be just as leak-free as the projection.
/// Also pins the 404 for an unknown app and the conflict for a binding whose
/// server is not configured (nothing coherent to consent to).
#[tokio::test]
async fn grant_responses_carry_names_only_never_definitions_or_env_values() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    // A disabled stdio server never spawns, so the definition can carry
    // conspicuously secret-shaped configuration for the leak assertions.
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
                        "name": "cmd",
                        "command": "/opt/secret-location/docs-mcp",
                        "args": ["--secret-flag"],
                        "env": ["LITERAL_NAME"],
                        "env_values": {"LITERAL_NAME": "literal-value-hunter2"},
                        "env_from": ["PARENT_SECRET_NAME"],
                        "enabled": false
                    }]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let cmd = connected_app_id(&store, "cmd").await;
    let app_id = create_app_bound_to(&store, cmd, &["mcp__cmd__doit"]).await;

    let mut responses = Vec::new();
    let state = grant_request(&router, &bearer, "GET", app_id).await;
    assert_eq!(state.status(), StatusCode::OK);
    responses.push(("GET", body_string(state).await));
    // Tool bindings are retired: there is nothing grantable behind this
    // manifest, so consent conflicts instead of recording a grant.
    let refused = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    responses.push(("POST", body_string(refused).await));

    // The sheet still names the server and its pinned tools, so it can say
    // what the manifest asked for and why it cannot be granted.
    assert!(
        responses[0].1.contains("\"cmd\"") && responses[0].1.contains("mcp__cmd__doit"),
        "GET: the sheet needs server and tool names: {}",
        responses[0].1
    );
    for (method, body) in &responses {
        for leaked in [
            "secret-location",
            "docs-mcp",
            "--secret-flag",
            "literal-value-hunter2",
            "LITERAL_NAME",
            "PARENT_SECRET_NAME",
        ] {
            assert!(
                !body.contains(leaked),
                "{method}: {leaked:?} must not reach the renderer: {body}"
            );
        }
    }
    let before: serde_json::Value = serde_json::from_str(&responses[0].1).unwrap();
    assert_eq!(before["granted"], json!(false));

    // An unknown app is 404 on the whole surface.
    let missing = grant_request(&router, &bearer, "GET", AppId::new()).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // A binding to a connected app that is not configured cannot be granted:
    // there is no definition to pin the consent to.
    let unbound = create_app_bound_to(
        &store,
        openwave_core::id::ConnectedAppId::new(),
        &["mcp__ghost__walk"],
    )
    .await;
    let refused = grant_request(&router, &bearer, "POST", unbound).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

/// The `rest_api` side of the same projection contract: consent still grants,
/// and neither the base URL, the document hash, nor the credential reference
/// behind the record ever reaches the renderer — the sheet gets the record's
/// display name and the pinned operation ids, nothing else.
#[tokio::test]
async fn rest_grant_responses_grant_and_carry_names_only() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let connected = openwave_core::id::ConnectedAppId::new();
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
            openwave_core::connected_app::ConnectedAppKind::RestApi,
            &[openwave_core::connected_app::ConnectedApp {
                id: connected,
                name: "issues".into(),
                kind: openwave_core::connected_app::ConnectedAppKind::RestApi,
                definition: json!({
                    "base_url": "https://internal.example.com/secret-mount/v2",
                    "catalog": &catalog,
                    "credential": {
                        "secret_name": "issues-credential-reference",
                        "placement": "bearer"
                    },
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
                    name: "Grant fixture".into(),
                    bindings: vec![AppBinding::Operations(
                        openwave_core::local_app::AppOperationsBinding {
                            app: connected,
                            operation_ids: vec!["listIssues".into()],
                        },
                    )],
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

    let mut responses = Vec::new();
    let state = grant_request(&router, &bearer, "GET", app_id).await;
    assert_eq!(state.status(), StatusCode::OK);
    responses.push(("GET", body_string(state).await));
    let consented = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(consented.status(), StatusCode::OK);
    responses.push(("POST", body_string(consented).await));

    for (method, body) in &responses {
        assert!(
            body.contains("\"issues\"") && body.contains("listIssues"),
            "{method}: the sheet needs the record and operation names: {body}"
        );
        for leaked in [
            "internal.example.com",
            "secret-mount",
            "issues-credential-reference",
            &catalog.document_sha256,
        ] {
            assert!(
                !body.contains(leaked),
                "{method}: {leaked:?} must not reach the renderer: {body}"
            );
        }
    }
    let before: serde_json::Value = serde_json::from_str(&responses[0].1).unwrap();
    assert_eq!(before["granted"], json!(false));
    let after: serde_json::Value = serde_json::from_str(&responses[1].1).unwrap();
    assert_eq!(after["granted"], json!(true));
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}
