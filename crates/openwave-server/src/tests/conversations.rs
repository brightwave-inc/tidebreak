use super::*;

#[tokio::test]
async fn project_create_get_and_list() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let created = make_project(&router, &bearer).await;
    assert_eq!(created.title.as_deref(), Some("p"));
    assert_eq!(created.attachment_revision, 0);
    assert!(created.root_attachments.is_empty());
    assert!(serde_json::to_value(&created)
        .unwrap()
        .get("workspace_dir")
        .is_none());

    let fetched: Project = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{}", created.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(fetched, created);

    let listed: Vec<Project> = {
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(listed, vec![created]);
}

async fn patch_project(
    router: &Router,
    bearer: &str,
    project: ProjectId,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/projects/{project}"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn project_title_patch_is_trimmed_bounded_and_clearable() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project = make_project(&router, &bearer).await;

    let oversized_create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"title": "x".repeat(121)}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized_create.status(), StatusCode::BAD_REQUEST);

    let renamed = patch_project(
        &router,
        &bearer,
        project.id,
        serde_json::json!({"title": "  Research workspace  "}),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(
        json_body::<Project>(renamed).await.title.as_deref(),
        Some("Research workspace")
    );

    let oversized = patch_project(
        &router,
        &bearer,
        project.id,
        serde_json::json!({"title": "x".repeat(121)}),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .get_project(project.id)
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Research workspace")
    );

    let cleared = patch_project(
        &router,
        &bearer,
        project.id,
        serde_json::json!({"title": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json_body::<Project>(cleared).await.title, None);

    let missing = patch_project(
        &router,
        &bearer,
        ProjectId::new(),
        serde_json::json!({"title": "missing"}),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_project_removes_only_an_empty_project() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let empty = make_project(&router, &bearer).await;
    let deleted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/projects/{}", empty.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(store.get_project(empty.id).await.unwrap().is_none());

    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/projects/{}", empty.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let nonempty = make_project(&router, &bearer).await;
    let chat_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": nonempty.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chat_response.status(), StatusCode::CREATED);

    let blocked = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/projects/{}", nonempty.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(blocked).await;
    assert_eq!(info.kind, "project_not_empty");
    assert!(store.get_project(nonempty.id).await.unwrap().is_some());
}

#[tokio::test]
async fn chat_can_be_filed_under_a_project() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project = make_project(&router, &bearer).await;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": project.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.project_id, Some(project.id));
}

#[tokio::test]
async fn project_chat_snapshots_ordered_opaque_root_defaults() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let root_b = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let root_a = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let project = Project {
        id: ProjectId::new(),
        title: Some("pathless".into()),
        attachment_revision: 3,
        root_attachments: vec![root_b, root_a],
        created_at: chrono::Utc::now(),
    };
    store.create_project(&project).await.unwrap();

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": project.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.attachment_revision, 1);
    assert_eq!(
        chat.root_attachments,
        vec![
            ChatRootAttachment {
                root_id: root_b,
                origin: RootAttachmentOrigin::ProjectDefault,
            },
            ChatRootAttachment {
                root_id: root_a,
                origin: RootAttachmentOrigin::ProjectDefault,
            },
        ]
    );
}

#[tokio::test]
async fn chat_referencing_an_unknown_project_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": ProjectId::new()}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "not_found");
}

#[tokio::test]
async fn chat_creation_reports_not_found_when_project_deletion_wins_after_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let database = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("project-chat-race.db").display()
        ))
        .await
        .unwrap(),
    );
    let project = Project {
        id: ProjectId::new(),
        title: Some("delete during chat creation".into()),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    database.create_project(&project).await.unwrap();
    let injected = Arc::new(PauseTerminalStore::new(
        database.clone(),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.delete_project_after_next_get();
    let (router, token, _store, _dir) = test_app_from_parts(Arc::new(FakeProvider), injected, dir);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"project_id": project.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "not_found");
    assert!(database.get_project(project.id).await.unwrap().is_none());
    assert!(database.list_chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn models_catalog_is_served() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    let models = catalog["models"].as_array().unwrap();
    assert!(!models.is_empty());
    assert!(models.iter().any(|m| m["provider"] == "anthropic"));
    // Each entry carries a human label and capability metadata.
    let opus = models
        .iter()
        .find(|m| m["id"] == "claude-opus-4-8")
        .expect("curated Anthropic model is present");
    assert_eq!(opus["display_name"], "Claude Opus 4.8");
    assert_eq!(opus["key"], "anthropic::claude-opus-4-8");
    assert_eq!(opus["context_window"], 1_000_000);
    assert_eq!(opus["max_output_tokens"], 128_000);
    assert_eq!(
        opus["input_modalities"],
        serde_json::json!(["text", "image"])
    );
    assert!(opus["supports_reasoning"].as_bool().unwrap());
    assert!(opus["multimodal"].as_bool().unwrap());
    assert!(!opus["available"].as_bool().unwrap());
    // The catalog carries the effort levels the model itself accepts, so a
    // client can offer exactly those and no more.
    assert_eq!(
        opus["reasoning_efforts"],
        serde_json::json!(["low", "medium", "high", "xhigh", "max"])
    );
    // A model that reasons but rejects the effort parameter advertises no
    // levels at all, which is what tells a client to hide the control.
    let haiku = models
        .iter()
        .find(|m| m["id"] == "claude-haiku-4-5-20251001")
        .expect("curated Anthropic model is present");
    assert!(haiku["supports_reasoning"].as_bool().unwrap());
    assert_eq!(haiku["reasoning_efforts"], serde_json::json!([]));
    // The GPT-5 line adds an off level, and only the 5.6 generation reaches
    // `max`.
    let gpt = models
        .iter()
        .find(|m| m["id"] == "gpt-5.5")
        .expect("curated OpenAI model is present");
    assert_eq!(
        gpt["reasoning_efforts"],
        serde_json::json!(["none", "low", "medium", "high", "xhigh"])
    );
}

#[tokio::test]
async fn chat_created_with_a_model() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "claude-x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;
    assert_eq!(chat.model.as_deref(), Some("claude-x"));
}

#[tokio::test]
async fn chat_created_with_empty_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"model": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

/// PATCH a chat's model with a raw JSON body, returning the response.
pub(super) async fn patch_chat(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/chats/{chat}"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn patch_chat_sets_and_clears_the_model() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(chat.model, None);

    let set = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": "m1"}),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(set).await.model.as_deref(), Some("m1"));

    let cleared = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(cleared).await.model, None);
}

/// A per-chat choice of model, effort, mode, or network policy becomes the
/// default the next chat seeds from; an explicit value in the create request
/// still wins, and clearing a choice clears its sticky default too.
#[tokio::test]
async fn chat_settings_stick_to_the_next_chat() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let first = make_chat(&router, &bearer).await;

    let patched = patch_chat(
        &router,
        &bearer,
        first.id,
        serde_json::json!({
            "model": "m-sticky",
            "reasoning_effort": "high",
            "permission_mode": "allow",
            "network_policy": {"mode": "package_managers"},
        }),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);

    let second = make_chat(&router, &bearer).await;
    assert_eq!(second.model.as_deref(), Some("m-sticky"));
    assert_eq!(
        second.reasoning_effort,
        Some(openwave_core::ReasoningEffort::High)
    );
    assert_eq!(
        second.permission_mode,
        Some(openwave_core::PermissionMode::Allow)
    );
    assert_eq!(
        second.network_policy,
        openwave_core::NetworkPolicy::PackageManagers
    );

    // An explicit value in the create request beats the sticky default.
    let explicit: Chat = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "permission_mode": "plan",
                            "network_policy": {"mode": "off"},
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        explicit.permission_mode,
        Some(openwave_core::PermissionMode::Plan)
    );
    assert_eq!(explicit.network_policy, openwave_core::NetworkPolicy::Off);
    // Untouched fields still seed from the sticky defaults.
    assert_eq!(explicit.model.as_deref(), Some("m-sticky"));

    // An explicit creation choice is recorded like a mid-chat one: the home
    // composer's pickers only ever reach `POST /chats`.
    let after_explicit = make_chat(&router, &bearer).await;
    assert_eq!(
        after_explicit.permission_mode,
        Some(openwave_core::PermissionMode::Plan)
    );
    assert_eq!(
        after_explicit.network_policy,
        openwave_core::NetworkPolicy::Off
    );

    // `GET /settings` exposes the same defaults, so a composer can display
    // what an unspecified create will seed.
    let settings: serde_json::Value = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        settings["chat_defaults"],
        serde_json::json!({
            "model": "m-sticky",
            "reasoning_effort": "high",
            "permission_mode": "plan",
            "network_policy": {"mode": "off"},
        })
    );

    // Clearing the per-chat choice clears the sticky default with it.
    let cleared = patch_chat(
        &router,
        &bearer,
        first.id,
        serde_json::json!({"model": null, "permission_mode": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let third = make_chat(&router, &bearer).await;
    assert_eq!(third.model, None);
    assert_eq!(third.permission_mode, None);
}

#[tokio::test]
async fn rejected_chat_creation_does_not_change_sticky_defaults() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let rejected = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "t".repeat(routes::MAX_CHAT_TITLE_CHARS + 1),
                        "model": "m-rejected",
                        "reasoning_effort": "high",
                        "permission_mode": "allow",
                        "network_policy": {"mode": "off"},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let next = make_chat(&router, &bearer).await;
    assert_eq!(next.model, None);
    assert_eq!(next.reasoning_effort, None);
    assert_eq!(next.permission_mode, None);
    assert_eq!(next.network_policy, openwave_core::NetworkPolicy::Open);
}

#[tokio::test]
async fn chat_network_policy_defaults_open_and_persists_normalized_exact_hosts() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(chat.network_policy, openwave_core::NetworkPolicy::Open);

    let updated = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({
            "network_policy": {
                "mode": "allowed_hosts",
                "allowed_hosts": [" API.Example.COM. ", "api.example.com"],
                "package_managers": true
            }
        }),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = json_body::<Chat>(updated).await;
    assert_eq!(
        updated.network_policy,
        openwave_core::NetworkPolicy::AllowedHosts {
            allowed_hosts: vec!["api.example.com".into()],
            package_managers: true,
        }
    );

    let fetched: Chat = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fetched.network_policy, updated.network_policy);

    for host in ["*.example.com", "127.0.0.1"] {
        let rejected = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "network_policy": {
                    "mode": "allowed_hosts",
                    "allowed_hosts": [host],
                    "package_managers": false
                }
            }),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST, "{host}");
    }
}

#[tokio::test]
async fn chat_title_patch_is_trimmed_bounded_and_clearable() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let renamed = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "  Project notes  "}),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(
        json_body::<Chat>(renamed).await.title.as_deref(),
        Some("Project notes")
    );

    let rejected = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "must not persist", "model": ""}),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let too_long = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "t".repeat(routes::MAX_CHAT_TITLE_CHARS + 1)}),
    )
    .await;
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let fetched: Chat = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fetched.title.as_deref(), Some("Project notes"));

    let cleared = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": null}),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json_body::<Chat>(cleared).await.title, None);
}

#[tokio::test]
async fn delete_chat_removes_a_quiesced_conversation_and_reports_safe_conflicts() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let chat_scratch = dir.path().join("scratch").join(chat.id.to_string());
    std::fs::create_dir_all(chat_scratch.join("artifacts")).unwrap();
    std::fs::write(chat_scratch.join("artifacts/brief.md"), "private").unwrap();

    let deleted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(store.get_chat(chat.id).await.unwrap().is_none());
    assert!(!chat_scratch.exists());

    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let linked = make_chat(&router, &bearer).await;
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), "keep").unwrap();
        symlink(
            outside.path(),
            dir.path().join("scratch").join(linked.id.to_string()),
        )
        .unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/chats/{}", linked.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("keep.txt")).unwrap(),
            "keep"
        );
    }

    let active = make_chat(&router, &bearer).await;
    store
        .accept_turn(TurnId::new(), active.id, "fake", "do not remove")
        .await
        .unwrap();
    let blocked = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", active.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(blocked).await;
    assert_eq!(info.kind, "chat_active");
    assert!(store.get_chat(active.id).await.unwrap().is_some());
}

#[tokio::test]
async fn chat_transcript_replays_only_visible_durable_messages() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    store
        .append_message(&Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            role: Role::User,
            reasoning: Default::default(),
            content: "remember this".into(),
            llm_content: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .append_event(
                chat.id,
                &AgentEvent::TextDelta {
                    text: "live".into()
                }
            )
            .await
            .unwrap(),
        1
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert_eq!(transcript["messages"][0]["role"], "user");
    assert_eq!(transcript["messages"][0]["content"], "remember this");
    assert_eq!(
        transcript["last_event_seq"], 0,
        "a nonterminal delta must replay after a durable transcript snapshot"
    );
}

#[tokio::test]
async fn a_retained_result_projection_survives_reopening_the_chat() {
    // Terminal activity used to be rebuilt from the stored failure code alone,
    // which recovers one enumerated setup signal and nothing else — so every
    // result card vanished on reload, including a command's own output. The
    // projection is now retained with the resolution and comes back with it.
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "finish a turn first").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    let turn = store
        .list_turn_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("accepted turn exists");

    let call_id = CallId::new();
    let started_at = chrono::Utc::now();
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-list".into(),
            name: "list_dir".into(),
            arguments: serde_json::json!({ "path": "reports" }),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at,
            resolved_at: None,
        })
        .await
        .unwrap();
    let preview = openwave_core::ToolResultPreview::Entries {
        entries: vec![openwave_core::ResultEntry::new(
            openwave_core::ResultEntryKind::File,
            "q3.md",
        )],
        failures: vec![openwave_core::ResultFailure::new("q4.md", "unreadable")],
        elided: 0,
    };
    assert_eq!(
        store
            .resolve_server_tool_call_with_artifacts(
                call_id,
                &ToolCallResolution::Completed {
                    result: "private model-facing listing".into(),
                },
                started_at + chrono::Duration::seconds(1),
                Some(&preview),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store.list_tool_calls(chat.id).await.unwrap()[0].result_preview,
        Some(preview.clone())
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    // The model-facing result text is not what came back — only the projection.
    assert!(!body.contains("private model-facing listing"));
    let transcript: serde_json::Value = serde_json::from_str(&body).unwrap();
    let card = transcript["tool_activity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["tool"] == "list_dir")
        .expect("the listing is in terminal activity");
    assert_eq!(card["result"]["tool"], "entries");
    assert_eq!(card["result"]["entries"][0]["label"], "q3.md");
    assert_eq!(card["result"]["failures"][0]["error"], "unreadable");
    assert_eq!(card["result_unreadable"], serde_json::json!(false));
}

#[tokio::test]
async fn transcript_tool_activity_is_allowlisted_and_redacts_canonical_tool_data() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "finish a turn first").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    let turn = store
        .list_turn_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("accepted turn exists");
    assert_eq!(turn.status, TurnRunStatus::Completed);

    let call_id = CallId::new();
    let started_at = chrono::Utc::now();
    let secret_path = "/Users/alice/Documents/payroll-secret.csv";
    let secret_result = "private tool result: 123-45-6789";
    let secret_provider_id = "provider-secret-call-id";
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: secret_provider_id.into(),
            name: "mcp__private_server__read_sensitive_path".into(),
            arguments: serde_json::json!({"path": secret_path}),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at,
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                call_id,
                &ToolCallResolution::Failed {
                    result: secret_result.into(),
                    error_code: "private_error_code".into(),
                    error_detail: Some("private diagnostic detail".into()),
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    // Spawn history carries only the derived child id. The task and canonical
    // call record stay server-side, while the transcript can reattach the
    // durable child snapshot to this exact historical step after a reload.
    let spawn_call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: spawn_call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-spawn".into(),
            name: openwave_core::SPAWN_SANDBOX_AGENT_TOOL.into(),
            arguments: serde_json::json!({"task": "private delegated task"}),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(3),
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                spawn_call_id,
                &ToolCallResolution::Completed {
                    result: "private child id".into(),
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    // A known, actionable failure may retain only its closed renderer signal.
    // Its model-facing result and durable error code still stay server-side.
    let configuration_call_id = CallId::new();
    let configuration_private_result = "private host configuration detail";
    store
        .accept_tool_call(&ToolCallRecord {
            id: configuration_call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-web-search".into(),
            name: openwave_core::WEB_SEARCH_TOOL.into(),
            arguments: serde_json::json!({"query": "private web query"}),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(4),
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                configuration_call_id,
                &ToolCallResolution::Failed {
                    result: configuration_private_result.into(),
                    error_code: openwave_core::ToolErrorCategory::ConfigurationRequired
                        .as_str()
                        .into(),
                    error_detail: None,
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    // An approval-in-flight call must stay on the event journal. Including it
    // in this durable snapshot could race its corresponding live event.
    store
        .accept_tool_call(&ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-pending".into(),
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "pending secret query"}),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(2),
            resolved_at: None,
        })
        .await
        .unwrap();

    let cancelled_call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: cancelled_call_id,
            chat_id: chat.id,
            turn_id: turn.id,
            provider_id: "provider-cancelled".into(),
            name: "request_folder_access".into(),
            arguments: serde_json::json!({"path": "/private/ignored"}),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: started_at + chrono::Duration::milliseconds(1),
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                cancelled_call_id,
                &ToolCallResolution::Cancelled {
                    result: "declined by the user".into(),
                },
                started_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for hidden in [
        secret_path,
        secret_result,
        secret_provider_id,
        "/private/ignored",
        "declined by the user",
        "pending secret query",
        configuration_private_result,
        "configuration_required",
        "private_error_code",
        "private diagnostic detail",
        "mcp__private_server__read_sensitive_path",
        "arguments",
        "provider_id",
        "execution",
        "error_code",
        "error_detail",
        "client_executor_id",
        "client_lease",
    ] {
        assert!(
            !body.contains(hidden),
            "renderer-safe transcript leaked canonical tool data: {hidden}"
        );
    }
    let transcript: serde_json::Value = serde_json::from_str(&body).unwrap();
    let activity = transcript["tool_activity"].as_array().unwrap();
    assert_eq!(activity.len(), 4);
    // The canonical call id is the one deliberate exception to redaction: the
    // MCP App payload route keys renderer-readable data on exactly this id, so
    // a rehydrated app view must be able to present it. Everything else about
    // the call — name, arguments, results, provider identity — stays hidden.
    assert!(activity.iter().any(|card| {
        card["tool"] == "other"
            && card["status"] == "failed"
            && card["call_id"] == serde_json::json!(call_id)
            && card["started_at"].is_string()
            && card["finished_at"].is_string()
    }));
    assert!(activity
        .iter()
        .any(|card| { card["tool"] == "request_folder_access" && card["status"] == "cancelled" }));
    assert!(activity.iter().any(|card| {
        card["tool"] == "spawn_sandbox_agent"
            && card["background_agent_run_id"]
                == serde_json::json!(openwave_core::AgentRunId::sandbox_for_spawn_call(
                    spawn_call_id
                ))
    }));
    assert!(activity.iter().any(|card| {
        card["tool"] == "web_search"
            && card["status"] == "failed"
            && card["result"] == serde_json::json!({"tool": "web_search_provider_required"})
    }));
}

#[tokio::test]
async fn transcript_cursor_replays_an_active_turn_after_the_durable_boundary() {
    let active_delta_entered = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(ReplayBoundaryProvider {
        calls: AtomicUsize::new(0),
        active_delta_entered: active_delta_entered.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "first turn").await,
        StatusCode::ACCEPTED
    );
    let first_events = wait_for_turn(&store, chat.id).await;
    let terminal_seq = first_events
        .iter()
        .find_map(|event| {
            matches!(event.event, AgentEvent::TurnCompleted { .. }).then_some(event.seq)
        })
        .expect("the completed first turn has a terminal journal event");

    assert_eq!(
        send_message(&router, &bearer, chat.id, "second turn").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), active_delta_entered.notified())
        .await
        .expect("second turn entered the live provider stream");
    for _ in 0..100 {
        if store
            .list_events(chat.id, terminal_seq)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(&event.event, AgentEvent::TextDelta { text } if text == "still streaming")
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert_eq!(transcript["last_event_seq"], terminal_seq);
    assert!(transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"] == "durable answer"));

    let replay = store
        .list_events(chat.id, transcript["last_event_seq"].as_i64().unwrap())
        .await
        .unwrap();
    assert!(replay
        .iter()
        .any(|event| matches!(event.event, AgentEvent::TurnStarted { .. })));
    assert!(replay.iter().any(
        |event| matches!(&event.event, AgentEvent::TextDelta { text } if text == "still streaming")
    ));
}

#[tokio::test]
async fn transcript_hydration_reconciles_an_active_steer_by_message_identity() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(GatedProvider { gate })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "start").await,
        StatusCode::ACCEPTED
    );
    for _ in 0..100 {
        if store
            .get_turn_run(turn_id)
            .await
            .unwrap()
            .is_some_and(|turn| turn.status == TurnRunStatus::Running)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, turn_id, "remember this", true).await,
        StatusCode::ACCEPTED
    );

    let mut steered = None;
    for _ in 0..200 {
        if let Some(event) = store
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|event| matches!(&event.event, AgentEvent::UserSteered { content, .. } if content == "remember this"))
        {
            steered = Some(event);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let steered = steered.expect("active steer was journaled");
    let message_id = match steered.event {
        AgentEvent::UserSteered { message_id, .. } => message_id.to_string(),
        _ => unreachable!("filtered steer event"),
    };

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    assert!(transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["id"] == message_id && message["content"] == "remember this"));

    let replay = store
        .list_events(chat.id, transcript["last_event_seq"].as_i64().unwrap())
        .await
        .unwrap();
    let replayed_message_id = replay.into_iter().find_map(|event| match event.event {
        AgentEvent::UserSteered { message_id, .. } => Some(message_id.to_string()),
        _ => None,
    });
    assert_eq!(replayed_message_id.as_deref(), Some(message_id.as_str()));

    // This is the renderer's reconciliation rule: the exact durable identity,
    // not matching text, suppresses a replayed steer already in the snapshot.
    let mut hydrated_ids = transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    assert!(!hydrated_ids.insert(message_id));
}

#[tokio::test]
async fn patch_chat_rejects_empty_model_and_unknown_chat() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let empty = patch_chat(&router, &bearer, chat.id, serde_json::json!({"model": ""})).await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let legacy_path = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"workspace_dir": "/tmp/legacy"}),
    )
    .await;
    assert_eq!(legacy_path.status(), StatusCode::BAD_REQUEST);

    let forged_roots = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"root_attachments": []}),
    )
    .await;
    assert_eq!(forged_roots.status(), StatusCode::BAD_REQUEST);

    let missing = patch_chat(
        &router,
        &bearer,
        ChatId::new(),
        serde_json::json!({"model": "m"}),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// #923: a managed permission-mode ceiling refuses over-ceiling selections
/// at the picker routes — create and patch alike — while stricter modes stay
/// the reader's choice. The ceiling names a maximum, never a fixed mode.
#[tokio::test]
async fn a_managed_ceiling_locks_over_ceiling_permission_modes() {
    struct CappedAtAsk;
    impl crate::managed_policy::OsPolicySource for CappedAtAsk {
        fn gateway_url(&self) -> openwave_core::Result<Option<String>> {
            Ok(None)
        }
        fn permission_mode_ceiling(
            &self,
        ) -> openwave_core::Result<Option<openwave_core::PermissionMode>> {
            Ok(Some(openwave_core::PermissionMode::Ask))
        }
    }

    let (dir, store) = temp_db_store("ceiling.db").await;
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
    state.os_policy = Arc::new(CappedAtAsk);
    let bearer = format!("Bearer {}", state.token.clone());
    let router = app(state);

    let chat = make_chat(&router, &bearer).await;
    let refused = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"permission_mode": "auto"}),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    let stricter = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"permission_mode": "plan"}),
    )
    .await;
    assert_eq!(stricter.status(), StatusCode::OK);

    let over_ceiling_creation = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"permission_mode": "allow"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(over_ceiling_creation.status(), StatusCode::CONFLICT);

    // A sticky `allow` recorded before the policy arrived seeds clamped to
    // the ceiling, exactly like the turn gate treats stored modes.
    store
        .set_setting("chat_default.permission_mode", &serde_json::json!("allow"))
        .await
        .unwrap();
    let seeded = make_chat(&router, &bearer).await;
    assert_eq!(
        seeded.permission_mode,
        Some(openwave_core::PermissionMode::Ask)
    );

    // The settings surface reports the same clamped mode, so a composer never
    // displays an autonomy the ceiling forbids.
    let settings: serde_json::Value = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(settings["chat_defaults"]["permission_mode"], "ask");
}
