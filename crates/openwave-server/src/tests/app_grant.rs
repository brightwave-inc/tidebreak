//! `/apps/{id}/grant`: the renderer-facing consent surface.
//!
//! The lifecycle (consent → invoke → revoke, staleness on reconfiguration,
//! widened-manifest re-consent) is covered end to end in
//! [`super::app_invoke`]; this module pins the projection contract — what the
//! grant endpoints may and may not put on the wire.

use super::*;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    AppBinding, AppManifest, AppOperationsBinding, CreateApp, NewAppRevision,
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
    operation_ids: &[&str],
) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Grant fixture".into(),
                    bindings: vec![AppBinding::Operations(AppOperationsBinding {
                        app,
                        operation_ids: operation_ids.iter().map(|id| (*id).to_owned()).collect(),
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

/// The surface's refusal edges: an unknown app is 404 everywhere, and a
/// binding to a connected app that is not configured cannot be granted —
/// there is no definition to pin the consent to.
#[tokio::test]
async fn unknown_apps_and_unconfigured_bindings_refuse_on_the_grant_surface() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // An unknown app is 404 on the whole surface.
    let missing = grant_request(&router, &bearer, "GET", AppId::new()).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // A binding to a connected app that is not configured reads ungranted
    // and conflicts on consent.
    let unbound = create_app_bound_to(
        &store,
        openwave_core::id::ConnectedAppId::new(),
        &["listIssues"],
    )
    .await;
    let state = grant_request(&router, &bearer, "GET", unbound).await;
    assert_eq!(state.status(), StatusCode::OK);
    let state: serde_json::Value = serde_json::from_str(&body_string(state).await).unwrap();
    assert_eq!(state["granted"], json!(false));
    let refused = grant_request(&router, &bearer, "POST", unbound).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

/// A manifest that binds nothing is granted vacuously — no consent sheet, no
/// stored grant needed. There is nothing to consent to: every invoke would
/// fail the pin check before the consent gate, so prompting would only gate
/// the frame on an empty "yes".
#[tokio::test]
async fn a_manifest_with_no_bindings_reads_granted_without_consent() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Calculator".into(),
                    bindings: vec![],
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

    let state = grant_request(&router, &bearer, "GET", app_id).await;
    assert_eq!(state.status(), StatusCode::OK);
    let state: serde_json::Value = serde_json::from_str(&body_string(state).await).unwrap();
    assert_eq!(state["granted"], json!(true));
    assert_eq!(state["bindings"], json!([]));
}

/// The projection contract: consent grants, and neither the base URL, the
/// document hash, nor the credential reference behind the record ever reaches
/// the renderer — the sheet gets the record's display name and the pinned
/// operation ids, nothing else. Asserted on the raw serialized JSON rather
/// than a parsed projection so nothing can hide in an unmodeled field.
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

/// Without a host-folder seam — headless serve, generic embeddings — a
/// manifest carrying a folder binding reads ungranted and consent conflicts:
/// the folder cannot resolve, honestly, instead of parking or half-granting
/// (docs/folder-bindings.md).
#[tokio::test]
async fn folder_bindings_read_ungranted_and_conflict_without_a_host_seam() {
    use openwave_core::local_app::{AppFolderBinding, FolderAccess};

    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Files fixture".into(),
                    bindings: vec![AppBinding::Folder(AppFolderBinding {
                        folder: openwave_core::id::HostRootId::from_uuid(uuid::Uuid::new_v4())
                            .unwrap(),
                        access: FolderAccess::Read,
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

    let state = grant_request(&router, &bearer, "GET", app_id).await;
    assert_eq!(state.status(), StatusCode::OK);
    let state: serde_json::Value = serde_json::from_str(&body_string(state).await).unwrap();
    assert_eq!(state["granted"], json!(false));

    let refused = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(refused).await;
    assert!(
        info.message.contains("not a connected folder"),
        "{}",
        info.message
    );
}

/// The folder consent lifecycle over the API with a host seam present: an
/// approved folder grants at the pinned access, the projection carries its
/// display name and access level (never a path), and disconnecting the
/// folder invalidates the grant on the next read — consent never outlives
/// the registration it named.
#[tokio::test]
async fn folder_bindings_grant_and_fail_closed_when_the_folder_disconnects() {
    use std::sync::Mutex as StdMutex;

    use openwave_core::local_app::{AppFolderBinding, FolderAccess};

    use crate::host_folders::{ApprovedFolder, HostFolders};

    struct FakeFolders(StdMutex<Vec<ApprovedFolder>>);

    #[async_trait::async_trait]
    impl HostFolders for FakeFolders {
        async fn approved_roots(&self) -> openwave_core::Result<Vec<ApprovedFolder>> {
            Ok(self.0.lock().unwrap().clone())
        }

        // The consent surface never dispatches I/O; a fake that fails loudly
        // if it ever does keeps this test about the grant object.
        async fn list_folder(
            &self,
            _root: openwave_core::id::HostRootId,
            _path: &str,
        ) -> Result<Vec<crate::host_folders::FolderEntry>, crate::host_folders::FolderOpError>
        {
            panic!("the grant surface must not dispatch folder I/O")
        }

        async fn read_file(
            &self,
            _root: openwave_core::id::HostRootId,
            _path: &str,
        ) -> Result<Vec<u8>, crate::host_folders::FolderOpError> {
            panic!("the grant surface must not dispatch folder I/O")
        }

        async fn write_file(
            &self,
            _root: openwave_core::id::HostRootId,
            _path: &str,
            _content: &[u8],
            _replace: bool,
        ) -> Result<crate::host_folders::FolderWriteReceipt, crate::host_folders::FolderOpError>
        {
            panic!("the grant surface must not dispatch folder I/O")
        }
    }

    let (dir, store) = temp_db_store("folder-grant.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = openwave_core::id::HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let folders = Arc::new(FakeFolders(StdMutex::new(vec![ApprovedFolder {
        root_id: root,
        display_name: "Tax documents".into(),
    }])));
    state.host_folders = Some(folders.clone());
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Files fixture".into(),
                    bindings: vec![AppBinding::Folder(AppFolderBinding {
                        folder: root,
                        access: FolderAccess::ReadWrite,
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

    // Ungranted at first; the row names the folder and its access level.
    let state_response = grant_request(&router, &bearer, "GET", app_id).await;
    assert_eq!(state_response.status(), StatusCode::OK);
    let before: serde_json::Value =
        serde_json::from_str(&body_string(state_response).await).unwrap();
    assert_eq!(before["granted"], json!(false));
    assert_eq!(before["bindings"][0]["folder"], json!(root));
    assert_eq!(before["bindings"][0]["access"], json!("read_write"));
    assert_eq!(before["bindings"][0]["name"], json!("Tax documents"));
    assert_eq!(before["bindings"][0]["app"], json!(null));

    // Consent grants at exactly the pinned access.
    let consented = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(consented.status(), StatusCode::OK);
    let after: serde_json::Value = serde_json::from_str(&body_string(consented).await).unwrap();
    assert_eq!(after["granted"], json!(true));
    assert_eq!(after["bindings"][0]["granted"], json!(true));

    // Disconnecting the folder invalidates the grant on the next read, with
    // the changed-since-you-agreed marker set.
    folders.0.lock().unwrap().clear();
    let stale = grant_request(&router, &bearer, "GET", app_id).await;
    let stale: serde_json::Value = serde_json::from_str(&body_string(stale).await).unwrap();
    assert_eq!(stale["granted"], json!(false));
    assert_eq!(stale["bindings"][0]["definition_changed"], json!(true));
    assert_eq!(stale["bindings"][0]["name"], json!(null));
}
