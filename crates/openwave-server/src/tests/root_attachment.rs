use super::*;

async fn test_app_with_executor_id(
    executor_id: uuid::Uuid,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let retrieval = build_retrieval();
    let state = AppState::new_with_client_executor_id(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig::default(),
        executor_id,
    )
    .unwrap();
    let token = state.token.clone();
    (app(state), token, store, dir)
}

async fn get_native(router: &Router, bearer: &str, uri: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_conflict_kind(response: axum::response::Response, expected: &str) {
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(body["kind"], expected);
}

#[tokio::test]
async fn embedded_renderer_bearer_cannot_reach_canonical_document_routes() {
    let (router, token, _store, _dir) = test_app_with_executor_id(uuid::Uuid::new_v4()).await;
    let bearer = format!("Bearer {token}");
    let ingest_body = serde_json::json!({
        "uri": "file:///Users/private/review-sentinel.md",
        "content": "private-content-sentinel",
        "media_type": "text/markdown",
    });
    let accepted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/documents")
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(ingest_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(accepted).await;
    let document_id = accepted["document_id"].as_str().unwrap();
    let project_id = ProjectId::new();

    for (method, uri, body) in [
        ("GET", "/documents".to_owned(), Body::empty()),
        ("GET", format!("/documents/{document_id}"), Body::empty()),
        (
            "POST",
            "/documents".to_owned(),
            Body::from(ingest_body.to_string()),
        ),
        (
            "POST",
            "/documents/raw".to_owned(),
            Body::from("private-content-sentinel"),
        ),
        ("DELETE", format!("/documents/{document_id}"), Body::empty()),
        (
            "GET",
            format!("/projects/{project_id}/documents"),
            Body::empty(),
        ),
        (
            "GET",
            format!("/projects/{project_id}/documents/{document_id}"),
            Body::empty(),
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&bytes);
        for sentinel in [
            "/Users/private",
            "review-sentinel",
            "private-content-sentinel",
        ] {
            assert!(
                !body.contains(sentinel),
                "renderer denial leaked {sentinel}"
            );
        }
    }

    let native_detail = get_native(&router, &bearer, &format!("/documents/{document_id}")).await;
    assert_eq!(native_detail.status(), StatusCode::OK);
    let native_detail: serde_json::Value = json_body(native_detail).await;
    assert_eq!(
        native_detail["uri"],
        "file:///Users/private/review-sentinel.md"
    );
    assert_eq!(native_detail["content"], "private-content-sentinel");
}

fn root_attachment_begin_body(
    root_id: HostRootId,
    action: RootAttachmentChangeAction,
    revision: i64,
    created_at: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "root_id": root_id,
        "action": action,
        "expected_attachment_revision": revision,
        "created_at": created_at,
    })
}

#[tokio::test]
async fn root_attachment_begin_is_native_private_and_exactly_idempotent() {
    let executor_id = uuid::Uuid::new_v4();
    let (router, token, store, _dir) = test_app_with_executor_id(executor_id).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let change_id = RootAttachmentChangeId::new();
    let root_id = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let uri = format!(
        "/chats/{}/root-attachment-changes/{change_id}/begin",
        chat.id
    );
    let body = root_attachment_begin_body(
        root_id,
        RootAttachmentChangeAction::Attach,
        0,
        chrono::Utc::now(),
    );

    let bearer_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bearer_only.status(), StatusCode::UNAUTHORIZED);

    let mut untrusted = body.clone();
    untrusted["executor_id"] = serde_json::json!(uuid::Uuid::new_v4());
    let response = post_native_json(&router, &bearer, &uri, untrusted).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = post_native_json(&router, &bearer, &uri, body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let begun: serde_json::Value = json_body(response).await;
    assert_eq!(begun["disposition"], "begun");
    assert!(begun["change"].get("executor_id").is_none());
    let persisted = store
        .get_root_attachment_change(change_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.executor_id, executor_id);

    let response = post_native_json(&router, &bearer, &uri, body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let existing: serde_json::Value = json_body(response).await;
    assert_eq!(existing["disposition"], "existing");

    let changed = root_attachment_begin_body(
        HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        RootAttachmentChangeAction::Attach,
        0,
        body["created_at"].as_str().unwrap().parse().unwrap(),
    );
    let response = post_native_json(&router, &bearer, &uri, changed).await;
    assert_conflict_kind(response, "root_attachment_identity_conflict").await;

    let revision_chat = make_chat(&router, &bearer).await;
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{}/begin",
            revision_chat.id,
            RootAttachmentChangeId::new()
        ),
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            1,
            chrono::Utc::now(),
        ),
    )
    .await;
    assert_conflict_kind(response, "root_attachment_revision_conflict").await;

    let malformed = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/not-a-uuid/begin",
            chat.id
        ),
        body.clone(),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let nil_chat = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{}/begin",
            uuid::Uuid::nil(),
            RootAttachmentChangeId::new()
        ),
        body,
    )
    .await;
    assert_eq!(nil_chat.status(), StatusCode::BAD_REQUEST);

    let invalid_body = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{}/begin",
            chat.id,
            RootAttachmentChangeId::new()
        ),
        serde_json::json!({
            "root_id": uuid::Uuid::nil(),
            "action": "not_an_action",
            "expected_attachment_revision": -1,
            "created_at": "not-a-timestamp"
        }),
    )
    .await;
    assert_eq!(invalid_body.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn root_attachment_pending_is_bounded_scoped_and_non_disclosing() {
    let executor_id = uuid::Uuid::new_v4();
    let other_executor = uuid::Uuid::new_v4();
    let (router, token, store, _dir) = test_app_with_executor_id(executor_id).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let change_id = RootAttachmentChangeId::new();
    let created_at = chrono::Utc::now();
    let begin_uri = format!(
        "/chats/{}/root-attachment-changes/{change_id}/begin",
        chat.id
    );
    let response = post_native_json(
        &router,
        &bearer,
        &begin_uri,
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            0,
            created_at,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let competing_id = RootAttachmentChangeId::new();
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{competing_id}/begin",
            chat.id
        ),
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            1,
            created_at + chrono::Duration::seconds(1),
        ),
    )
    .await;
    assert_conflict_kind(response, "root_attachment_chat_busy").await;
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{competing_id}/begin",
            chat.id
        ),
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            1,
            created_at + chrono::Duration::seconds(1),
        ),
    )
    .await;
    let conflict: serde_json::Value = json_body(response).await;
    let conflict_text = conflict.to_string();
    assert!(!conflict_text.contains(&change_id.to_string()));
    assert!(!conflict_text.contains("current_attachment_revision"));

    let other_chat = make_chat(&router, &bearer).await;
    let other_id = RootAttachmentChangeId::new();
    store
        .begin_root_attachment_change(&BeginRootAttachmentChange {
            id: other_id,
            chat_id: other_chat.id,
            executor_id: other_executor,
            root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            expected_attachment_revision: 0,
            created_at: created_at - chrono::Duration::seconds(1),
        })
        .await
        .unwrap();

    let earlier_chat = make_chat(&router, &bearer).await;
    let earlier_id = RootAttachmentChangeId::new();
    store
        .begin_root_attachment_change(&BeginRootAttachmentChange {
            id: earlier_id,
            chat_id: earlier_chat.id,
            executor_id,
            root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            expected_attachment_revision: 0,
            created_at: created_at - chrono::Duration::seconds(2),
        })
        .await
        .unwrap();

    let response = get_native(&router, &bearer, "/root-attachment-changes/pending").await;
    assert_eq!(response.status(), StatusCode::OK);
    let pending: serde_json::Value = json_body(response).await;
    assert_eq!(pending["changes"].as_array().unwrap().len(), 2);
    assert_eq!(pending["changes"][0]["id"], earlier_id.to_string());
    assert_eq!(pending["changes"][1]["id"], change_id.to_string());
    assert!(pending["changes"][0].get("executor_id").is_none());
    assert!(!pending.to_string().contains(&other_id.to_string()));

    for uri in [
        "/root-attachment-changes/pending?limit=0",
        "/root-attachment-changes/pending?limit=257",
        "/root-attachment-changes/pending?unknown=1",
    ] {
        assert_eq!(
            get_native(&router, &bearer, uri).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn root_attachment_finish_is_exact_scoped_and_keeps_contradictions_pending() {
    let executor_id = uuid::Uuid::new_v4();
    let (router, token, store, _dir) = test_app_with_executor_id(executor_id).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let change_id = RootAttachmentChangeId::new();
    let begin_uri = format!(
        "/chats/{}/root-attachment-changes/{change_id}/begin",
        chat.id
    );
    let response = post_native_json(
        &router,
        &bearer,
        &begin_uri,
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            0,
            chrono::Utc::now(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let finish_uri = format!("/root-attachment-changes/{change_id}/finish");
    let terminal = serde_json::json!({
        "terminal": {
            "status": "completed",
            // Picker registration already attaches the root, so the exact
            // follow-up AttachRoot receipt legitimately reports no mutation.
            "broker_changed": false,
            "broker_currently_attached": true
        }
    });
    let response = post_native_json(&router, &bearer, &finish_uri, terminal.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let finished: serde_json::Value = json_body(response).await;
    assert_eq!(finished["disposition"], "finished");
    assert!(finished["change"].get("executor_id").is_none());
    let response = post_native_json(&router, &bearer, &finish_uri, terminal).await;
    assert_eq!(response.status(), StatusCode::OK);
    let existing: serde_json::Value = json_body(response).await;
    assert_eq!(existing["disposition"], "existing");

    let response = post_native_json(
        &router,
        &bearer,
        &finish_uri,
        serde_json::json!({
            "terminal": {
                "status": "completed",
                "broker_changed": true,
                "broker_currently_attached": true
            }
        }),
    )
    .await;
    assert_conflict_kind(response, "root_attachment_already_terminal").await;

    let other_chat = make_chat(&router, &bearer).await;
    let other_id = RootAttachmentChangeId::new();
    store
        .begin_root_attachment_change(&BeginRootAttachmentChange {
            id: other_id,
            chat_id: other_chat.id,
            executor_id: uuid::Uuid::new_v4(),
            root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            expected_attachment_revision: 0,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let response = post_native_json(
        &router,
        &bearer,
        &format!("/root-attachment-changes/{other_id}/finish"),
        serde_json::json!({
            "terminal": {
                "status": "completed",
                "broker_changed": true,
                "broker_currently_attached": true
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let contradictory_chat = make_chat(&router, &bearer).await;
    let contradictory_id = RootAttachmentChangeId::new();
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{contradictory_id}/begin",
            contradictory_chat.id
        ),
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            0,
            chrono::Utc::now(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = post_native_json(
        &router,
        &bearer,
        &format!("/root-attachment-changes/{contradictory_id}/finish"),
        serde_json::json!({
            "terminal": {
                "status": "failed",
                "broker_changed": true,
                "broker_currently_attached": true,
                "failure": {"code": "broker_error", "message": "failed", "retryable": true}
            }
        }),
    )
    .await;
    assert_conflict_kind(response, "root_attachment_broker_state_mismatch").await;
    let pending = store
        .list_pending_root_attachment_changes(executor_id, 64)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, contradictory_id);

    let malformed = post_native_json(
        &router,
        &bearer,
        &format!("/root-attachment-changes/{contradictory_id}/finish"),
        serde_json::json!({
            "terminal": {
                "status": "failed",
                "failure": {"code": "", "message": "failed", "retryable": false}
            }
        }),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let nil = post_native_json(
        &router,
        &bearer,
        "/root-attachment-changes/00000000-0000-0000-0000-000000000000/finish",
        serde_json::json!({
            "terminal": {
                "status": "completed",
                "broker_changed": false,
                "broker_currently_attached": true
            }
        }),
    )
    .await;
    assert_eq!(nil.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn disconnected_attach_receipt_rolls_back_product_intent_and_clears_pending() {
    let executor_id = uuid::Uuid::new_v4();
    let (router, token, store, _dir) = test_app_with_executor_id(executor_id).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let root_id = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let change_id = RootAttachmentChangeId::new();
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{change_id}/begin",
            chat.id
        ),
        root_attachment_begin_body(
            root_id,
            RootAttachmentChangeAction::Attach,
            0,
            chrono::Utc::now(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(store
        .get_chat(chat.id)
        .await
        .unwrap()
        .unwrap()
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == root_id));

    let response = post_native_json(
        &router,
        &bearer,
        &format!("/root-attachment-changes/{change_id}/finish"),
        serde_json::json!({
            "terminal": {
                "status": "failed",
                "broker_changed": true,
                "broker_currently_attached": false,
                "failure": {
                    "code": "broker_attachment_disconnected",
                    "message": "The selected folder was disconnected before synchronization completed.",
                    "retryable": false
                }
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let projected = store.get_chat(chat.id).await.unwrap().unwrap();
    assert!(!projected
        .root_attachments
        .iter()
        .any(|attachment| attachment.root_id == root_id));
    assert!(store
        .list_pending_root_attachment_changes(executor_id, 64)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn future_creation_time_cannot_wedge_a_pending_change() {
    let executor_id = uuid::Uuid::new_v4();
    let (router, token, store, _dir) = test_app_with_executor_id(executor_id).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let change_id = RootAttachmentChangeId::new();
    let created_at = chrono::Utc::now() + chrono::Duration::days(1);
    let response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/root-attachment-changes/{change_id}/begin",
            chat.id
        ),
        root_attachment_begin_body(
            HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            RootAttachmentChangeAction::Attach,
            0,
            created_at,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = post_native_json(
        &router,
        &bearer,
        &format!("/root-attachment-changes/{change_id}/finish"),
        serde_json::json!({
            "terminal": {
                "status": "completed",
                "broker_changed": true,
                "broker_currently_attached": true
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let finished = store
        .get_root_attachment_change(change_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.finished_at, Some(finished.created_at));
    assert!(store
        .list_pending_root_attachment_changes(executor_id, 64)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn generic_embedding_does_not_mount_durable_attachment_routes() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = get_native(&router, &bearer, "/root-attachment-changes/pending").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
