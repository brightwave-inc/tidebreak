//! Image attachment publication and turn acceptance over the HTTP surface.

use super::*;

use crate::routes::image_attachment::{gif_with_screen_size, png_header};

fn image_attachments_uri(chat: ChatId) -> String {
    format!("/chats/{chat}/attachments/images")
}

fn transcript_image_uri(chat: ChatId, attachment_id: uuid::Uuid) -> String {
    format!("/chats/{chat}/attachments/images/{attachment_id}")
}

/// Publish `bytes` for `chat` under a declared `Content-Type`.
async fn publish_image(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    declared: &str,
    bytes: Vec<u8>,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(image_attachments_uri(chat))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, declared)
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn publish_png(router: &Router, bearer: &str, chat: ChatId, bytes: Vec<u8>) -> uuid::Uuid {
    let response = publish_image(router, bearer, chat, "image/png", bytes).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let published: serde_json::Value = json_body(response).await;
    published["attachment_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// POST a message carrying published attachment ids.
async fn send_message_with_attachments(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    turn_id: TurnId,
    content: &str,
    attachments: &[uuid::Uuid],
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/messages"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": turn_id,
                        "content": content,
                        "attachments": attachments,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn stored_blob_count(data_dir: &std::path::Path) -> usize {
    let blobs = data_dir.join("blobs");
    let Ok(entries) = std::fs::read_dir(&blobs) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|ext| ext.to_str()) == Some("blob")
                && entry.metadata().is_ok_and(|meta| meta.is_file())
        })
        .count()
}

type OpenAiRequestCapture =
    Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<serde_json::Value>>>>;

/// Records the actual payload the configured OpenAI adapter sends, then emits a
/// minimal successful streaming response. This keeps the capability test local
/// while exercising the same resolver and adapter path as a desktop turn.
async fn capture_openai_image_request(
    axum::extract::State(capture): axum::extract::State<OpenAiRequestCapture>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    // The turn is not the only call this endpoint serves: a chat with no title
    // also gets a background titling call, on a different model, which can
    // arrive first. It is identifiable by its output constraint — the turn has
    // none — and it is not what this test is capturing.
    if request.get("text").is_none() {
        if let Some(sender) = capture.lock().unwrap().take() {
            let _ = sender.send(request);
        }
    }
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"The images arrived.\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n"
        ),
    )
}

fn one_pixel_jpeg() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0]));
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode_image(&image)
        .unwrap();
    bytes
}

fn spawn_turn_worker_with_image_blobs(state: &AppState) {
    let worker = crate::engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        crate::engine::internal::leg::LegDriverConfig::default(),
    )
    .with_blobs(state.blobs.clone());
    tokio::spawn(worker.run());
}

#[tokio::test]
async fn publishing_returns_opaque_identity_and_bounded_metadata_only() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let response =
        publish_image(&router, &bearer, chat.id, "image/png", png_header(800, 600)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let published: serde_json::Value = json_body(response).await;
    let mut keys = published
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        ["attachment_id", "byte_len", "height", "media_type", "width"]
    );
    assert_eq!(published["media_type"], "image/png");
    assert_eq!(published["width"], 800);
    assert_eq!(published["height"], 600);
    // Nothing in the response can be turned back into a location on disk.
    let serialized = published.to_string();
    for forbidden in [
        "blob",
        "path",
        "dir",
        "tmp",
        ".png",
        std::env::temp_dir().to_str().unwrap(),
    ] {
        assert!(
            !serialized.contains(forbidden),
            "publish response leaked {forbidden}: {serialized}"
        );
    }

    // An unknown conversation is a 404 before any bytes are retained.
    let missing = publish_image(
        &router,
        &bearer,
        ChatId::new(),
        "image/png",
        png_header(8, 8),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_declared_type_that_disagrees_with_the_sniffed_bytes_is_refused() {
    let (router, token, _store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // PNG bytes announced as a JPEG: the bytes win, and the disagreement is
    // reported rather than quietly corrected to the sniffed type.
    let response = publish_image(&router, &bearer, chat.id, "image/jpeg", png_header(64, 64)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "image_attachment_media_type_mismatch");
    assert_eq!(
        stored_blob_count(dir.path()),
        0,
        "a refused attachment must not retain bytes"
    );

    // A file renamed to `.png` and declared as one is caught the same way: the
    // extension never enters the decision.
    let response = publish_image(
        &router,
        &bearer,
        chat.id,
        "image/png",
        gif_with_screen_size(8, 8),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "image_attachment_media_type_mismatch");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(image_attachments_uri(chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::from(png_header(64, 64)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "image_attachment_media_type_required");
}

#[tokio::test]
async fn a_non_image_an_unsupported_format_and_an_oversized_image_are_each_refused() {
    let (router, token, _store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    for (declared, bytes, expected) in [
        (
            "image/png",
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3 not an image".to_vec(),
            "image_attachment_not_an_image",
        ),
        (
            "image/png",
            b"II*\x00\x08\x00\x00\x00".to_vec(),
            "image_attachment_unsupported_format",
        ),
        (
            "image/png",
            png_header(tidebreak_core::MAX_IMAGE_DIMENSION + 1, 600),
            "image_attachment_dimensions_too_large",
        ),
        (
            "image/gif",
            gif_with_screen_size(0, 8),
            "image_attachment_zero_dimension",
        ),
        ("image/png", Vec::new(), "image_attachment_empty"),
    ] {
        let response = publish_image(&router, &bearer, chat.id, declared, bytes).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{expected}");
        let error: AgentErrorInfo = json_body(response).await;
        assert_eq!(error.kind, expected);
    }

    // Bytes past the per-image ceiling never reach a handler at all.
    let oversized = vec![0u8; tidebreak_core::MAX_IMAGE_BYTES as usize + 1];
    let response = publish_image(&router, &bearer, chat.id, "image/png", oversized).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    assert_eq!(
        stored_blob_count(dir.path()),
        0,
        "no refusal may retain bytes"
    );
}

#[tokio::test]
async fn a_turn_records_its_attachments_and_an_unpublished_id_is_refused() {
    let (router, token, state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let _ = &state;

    let attachment_id = publish_png(&router, &bearer, chat.id, png_header(320, 240)).await;
    let accepted = send_message_with_attachments(
        &router,
        &bearer,
        chat.id,
        TurnId::new(),
        "what is in this?",
        &[attachment_id],
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);

    let attachments = store.list_message_attachments(chat.id).await.unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].image.blob_id, attachment_id);
    assert_eq!(attachments[0].image.width, 320);
    assert_eq!(attachments[0].image.height, 240);
    assert_eq!(
        attachments[0].image.media_type,
        tidebreak_core::ImageMediaType::Png
    );

    // An id that was never published names no bytes, so the turn cannot be
    // accepted against it.
    let response = send_message_with_attachments(
        &router,
        &bearer,
        chat.id,
        TurnId::new(),
        "and this?",
        &[uuid::Uuid::new_v4()],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "image_attachment_not_found");
}

#[tokio::test]
async fn a_known_blob_id_requires_a_separate_publication_in_each_chat() {
    let (router, token, state, store, dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let first_chat = make_chat(&router, &bearer).await;
    let second_chat = make_chat(&router, &bearer).await;
    let bytes = png_header(480, 320);

    let attachment_id = publish_png(&router, &bearer, first_chat.id, bytes.clone()).await;
    assert!(store
        .get_published_chat_image(first_chat.id, attachment_id)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .get_published_chat_image(second_chat.id, attachment_id)
        .await
        .unwrap()
        .is_none());

    // Content ids are intentionally stable and may be learned elsewhere, but
    // knowing one does not publish it to another conversation.
    let rebound = send_message_with_attachments(
        &router,
        &bearer,
        second_chat.id,
        TurnId::new(),
        "describe the image from the other chat",
        &[attachment_id],
    )
    .await;
    assert_eq!(rebound.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(rebound).await;
    assert_eq!(error.kind, "image_attachment_not_found");
    assert!(store
        .list_messages(second_chat.id)
        .await
        .unwrap()
        .is_empty());

    // Promotion rechecks the same authority boundary. A forged/stale queued
    // row cannot bypass publication merely because it was already durable.
    let now = chrono::Utc::now();
    store
        .enqueue_queued_turn(&tidebreak_core::QueuedTurn {
            id: TurnId::new(),
            chat_id: second_chat.id,
            content: "queued cross-chat rebind".into(),
            attachments: vec![attachment_id],
            file_attachments: Vec::new(),
            invoked_skills: Vec::new(),
            voice_input_used: false,
            position: 0,
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    crate::routes::promote_queued_turns(&state).await.unwrap();
    assert!(store
        .list_queued_turns(second_chat.id)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .list_messages(second_chat.id)
        .await
        .unwrap()
        .is_empty());

    // Publishing the same bytes to the target chat creates independent
    // authority while retaining one globally deduplicated blob.
    let second_id = publish_png(&router, &bearer, second_chat.id, bytes).await;
    assert_eq!(second_id, attachment_id);
    assert_eq!(stored_blob_count(dir.path()), 1);
    assert!(store
        .get_published_chat_image(second_chat.id, attachment_id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            second_chat.id,
            TurnId::new(),
            "now describe this image",
            &[attachment_id],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store
            .list_message_attachments(second_chat.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn transcript_projects_image_identity_and_fetches_pixels_with_the_chat_bearer() {
    let (router, token, _state, _store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let bytes = png_header(320, 240);
    let attachment_id = publish_png(&router, &bearer, chat.id, bytes.clone()).await;

    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            "what is in this?",
            &[attachment_id],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );

    let transcript = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(transcript.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(transcript).await;
    let images = &transcript["messages"][0]["image_attachments"];
    assert_eq!(
        images,
        &serde_json::json!([{
            "attachment_id": attachment_id,
            "media_type": "image/png",
            "width": 320,
            "height": 240,
        }])
    );
    let encoded = images.to_string();
    for forbidden in ["byte_len", "bytes", "path", "blob", "data:"] {
        assert!(
            !encoded.contains(forbidden),
            "transcript attachment leaked {forbidden}: {encoded}"
        );
    }

    let pixels = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(chat.id, attachment_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pixels.status(), StatusCode::OK);
    assert_eq!(pixels.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(pixels.headers()[header::CACHE_CONTROL], "no-store");
    // Hardening a browser applies but nothing in the app surfaces. The exact
    // content type matters beyond correctness: the renderer refuses to draw a
    // blob whose type disagrees with the transcript's record.
    assert_eq!(pixels.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(pixels.headers()[header::CONTENT_DISPOSITION], "inline");
    assert_eq!(pixels.headers()[header::REFERRER_POLICY], "no-referrer");
    assert!(pixels
        .headers()
        .contains_key(header::CONTENT_SECURITY_POLICY));
    assert_eq!(
        pixels.headers()[header::CONTENT_LENGTH],
        bytes.len().to_string()
    );
    assert_eq!(
        to_bytes(pixels.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        bytes.as_slice()
    );

    // Publishing the same content in another chat does not grant that chat a
    // reference to this message attachment.
    let other_chat = make_chat(&router, &bearer).await;
    let absent = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(other_chat.id, attachment_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_cancelled_and_a_retried_turn_leave_exactly_one_set_of_stored_bytes() {
    let (router, token, _state, store, dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let bytes = png_header(640, 480);

    // The same picture attached twice is one content-addressed blob.
    let first = publish_png(&router, &bearer, chat.id, bytes.clone()).await;
    let second = publish_png(&router, &bearer, chat.id, bytes.clone()).await;
    assert_eq!(first, second);
    assert_eq!(stored_blob_count(dir.path()), 1);

    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "describe this",
            &[first],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    // An ambiguous HTTP retry of the same turn is idempotent, images included.
    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "describe this",
            &[first],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        1
    );
    assert_eq!(stored_blob_count(dir.path()), 1);

    // Cancel, then send the same image again as a fresh turn.
    let cancelled = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/cancel", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"turn_id": turn_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);

    let retried = publish_png(&router, &bearer, chat.id, bytes).await;
    assert_eq!(retried, first);
    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            "describe this",
            &[retried],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        2
    );
    assert_eq!(
        stored_blob_count(dir.path()),
        1,
        "publishing, cancelling, and retrying the same image must store one copy"
    );
}

#[tokio::test]
async fn an_exact_image_turn_retry_survives_a_managed_gateway_route_retarget() {
    let (dir, store) = temp_db_store("gateway-image-retry.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let gateway_url = "https://corp.gateway/";
    let credentials: crate::connectors::GatewayCredentials =
        serde_json::from_value(serde_json::json!({
            "base_url": gateway_url,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
    crate::connectors::CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();

    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned_policy, gateway_url).unwrap();
    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: gateway_url.into(),
            installation_id: Some("install-1".into()),
            models: vec![providers::CustomModelConfig {
                id: "gateway-opus".into(),
                upstream_id: Some("claude-opus-5".into()),
                context_window: 200_000,
                max_output_tokens: 32_000,
                ..Default::default()
            }],
            model_protocols: Default::default(),
            model_reasoning_efforts: Default::default(),
            member_catalog: Some("v1".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();

    let gateway = crate::gateway_runtime::GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    );
    let chatgpt = Arc::new(
        crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
    );
    let resolver = Arc::new(resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        gateway.clone(),
        chatgpt.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    ));
    let mut state = AppState::with_gateway_runtime(
        Config::desktop(dir.path()),
        store.clone(),
        resolver,
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "anthropic::claude-opus-5".into(),
            ..AgentConfig::default()
        },
        uuid::Uuid::new_v4(),
        gateway,
        chatgpt,
        provisioned_policy,
        os_policy,
    )
    .unwrap();
    state.root_attachment_routes_enabled = false;
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);
    let chat = make_chat(&router, &bearer).await;
    store
        .set_chat_model(chat.id, Some("anthropic::claude-opus-5".into()))
        .await
        .unwrap();
    let attachment_id = publish_png(&router, &bearer, chat.id, png_header(320, 240)).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "describe this",
            &[attachment_id],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    let accepted = store
        .get_turn_run(turn_id)
        .await
        .unwrap()
        .expect("the image turn was accepted");
    assert!(accepted
        .model
        .starts_with("model_gateway::__tidebreak_gateway_v1."));
    let original_model = accepted.model;

    // The deployment reuses the same local id for a different upstream route.
    // The accepted selector must no longer resolve, so execution remains
    // fail-closed instead of silently following the retargeted catalog row.
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: gateway_url.into(),
            installation_id: Some("install-1".into()),
            models: vec![providers::CustomModelConfig {
                id: "gateway-opus".into(),
                upstream_id: Some("claude-haiku-4-5-20251001".into()),
                context_window: 200_000,
                max_output_tokens: 8_192,
                ..Default::default()
            }],
            model_protocols: Default::default(),
            model_reasoning_efforts: Default::default(),
            member_catalog: Some("v2".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();
    assert!(
        providers::resolve_model_policy(&*store, &original_model, false, None)
            .await
            .unwrap()
            .is_none(),
        "the accepted frozen route must not follow a retargeted local model id"
    );

    // Storage owns idempotency. A byte-for-byte retry remains accepted even
    // though the immutable execution route has since become unavailable.
    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "describe this",
            &[attachment_id],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store
            .get_turn_run(turn_id)
            .await
            .unwrap()
            .expect("the accepted turn remains durable")
            .model,
        original_model
    );
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_tool_preview_is_fetchable_only_through_its_chat() {
    let (router, token, _state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let bytes = png_header(640, 480);
    let blob_id = publish_png(&router, &bearer, chat.id, bytes.clone()).await;
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "run a command")
        .await
        .unwrap();
    let call_id = CallId::new();
    let now = chrono::Utc::now();
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: "exec-1".into(),
            name: "exec".into(),
            arguments: serde_json::json!({"command": "python"}),
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
            created_at: now,
            resolved_at: None,
        })
        .await
        .unwrap();
    let preview = tidebreak_core::ToolResultPreview::Exec {
        exit_code: Some(0),
        timed_out: false,
        output_truncated: false,
        stdout: String::new(),
        stderr: String::new(),
        images: vec![tidebreak_core::ImageRef {
            blob_id,
            media_type: tidebreak_core::ImageMediaType::Png,
            width: 640,
            height: 480,
            byte_len: bytes.len() as u64,
        }],
        outputs: Vec::new(),
        degraded: None,
        backend: None,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            call_id,
            &ToolCallResolution::Completed {
                result: "preview ready".into(),
            },
            now,
            Some(&preview),
        )
        .await
        .unwrap();

    let available = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(chat.id, blob_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(available.status(), StatusCode::OK);
    let absent = router
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(other_chat.id, blob_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_screen_capture_preview_is_fetchable_only_through_its_chat() {
    let (router, token, _state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let bytes = png_header(2056, 1329);
    let blob_id = publish_png(&router, &bearer, chat.id, bytes.clone()).await;
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "look at the screen")
        .await
        .unwrap();
    let call_id = CallId::new();
    let now = chrono::Utc::now();
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: "capture-1".into(),
            name: "screen_capture".into(),
            arguments: serde_json::json!({}),
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
            created_at: now,
            resolved_at: None,
        })
        .await
        .unwrap();
    let preview = tidebreak_core::ToolResultPreview::ScreenCapture {
        image: tidebreak_core::ImageRef {
            blob_id,
            media_type: tidebreak_core::ImageMediaType::Png,
            width: 2056,
            height: 1329,
            byte_len: bytes.len() as u64,
        },
        mark_count: 12,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            call_id,
            &ToolCallResolution::Completed {
                result: "captured".into(),
            },
            now,
            Some(&preview),
        )
        .await
        .unwrap();

    let available = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(chat.id, blob_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(available.status(), StatusCode::OK);
    let absent = router
        .oneshot(
            Request::builder()
                .uri(transcript_image_uri(other_chat.id, blob_id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_turn_carrying_images_against_a_text_only_model_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("image-capability.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    // A user-registered OpenAI-compatible model: Tidebreak cannot know whether
    // an arbitrary endpoint accepts images, so it remains text-only.
    providers::write_config(
        &*store,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://compat.example/v1".into()),
            models: vec![providers::CustomModelConfig {
                id: "vendor/model".into(),
                upstream_id: None,
                display_name: Some("Vendor Model".into()),
                context_window: 65_536,
                max_output_tokens: 8_192,
                ..Default::default()
            }],
        },
    )
    .await
    .unwrap();
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderCredential::api_key("sk-local"),
    )
    .await
    .unwrap();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(resolver::ConfiguredResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(
                store.clone(),
                secrets.clone(),
                crate::managed_policy::MemoryProvisionedPolicy::new(),
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
            Arc::new(
                crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone())
                    .unwrap(),
            ),
            crate::managed_policy::MemoryProvisionedPolicy::new(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        )),
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "openai_compatible::vendor/model".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);
    let chat = make_chat(&router, &bearer).await;

    let policy =
        providers::resolve_model_policy(&*store, "openai_compatible::vendor/model", false, None)
            .await
            .unwrap()
            .expect("a registered custom model resolves");
    assert!(!policy
        .input_modalities
        .contains(&crate::model_registry::InputModality::Image));

    let attachment_id = publish_png(&router, &bearer, chat.id, png_header(200, 150)).await;
    let response = send_message_with_attachments(
        &router,
        &bearer,
        chat.id,
        TurnId::new(),
        "what is in this screenshot?",
        &[attachment_id],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "model_image_input_unsupported");
    // Refused, not silently stripped: no turn and no attachment were recorded.
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
    assert!(store
        .list_message_attachments(chat.id)
        .await
        .unwrap()
        .is_empty());

    // The same text without images still goes through, so the refusal is about
    // the attachments rather than the model being unusable.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "what is in this screenshot?").await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_curated_openai_model_answers_after_receiving_png_and_jpeg_attachments() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let capture: OpenAiRequestCapture = Arc::new(std::sync::Mutex::new(Some(sender)));
    let provider = axum::Router::new()
        .route(
            "/v1/responses",
            axum::routing::post(capture_openai_image_request),
        )
        .with_state(capture);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, provider).await;
    });
    let provider_base_url = format!("http://{address}/v1");
    providers::allow_test_loopback_provider_base_url(&provider_base_url);

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("image-capability.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some(provider_base_url),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-test"),
    )
    .await
    .unwrap();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(resolver::ConfiguredResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(
                store.clone(),
                secrets.clone(),
                crate::managed_policy::MemoryProvisionedPolicy::new(),
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
            Arc::new(
                crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone())
                    .unwrap(),
            ),
            crate::managed_policy::MemoryProvisionedPolicy::new(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        )),
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "openai::gpt-5.6-sol".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    spawn_turn_worker_with_image_blobs(&state);
    let router = app(state);
    let chat = make_chat(&router, &bearer).await;

    let png = publish_png(&router, &bearer, chat.id, png_header(1, 1)).await;
    let jpeg = publish_image(&router, &bearer, chat.id, "image/jpeg", one_pixel_jpeg()).await;
    assert_eq!(jpeg.status(), StatusCode::CREATED);
    let jpeg: serde_json::Value = json_body(jpeg).await;
    let jpeg: uuid::Uuid = jpeg["attachment_id"].as_str().unwrap().parse().unwrap();

    assert_eq!(
        send_message_with_attachments(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            "Describe these attachments.",
            &[png, jpeg],
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(
        |event| matches!(&event.event, AgentEvent::TextDelta { text } if text == "The images arrived.")
    ));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
        .await
        .expect("configured provider did not receive the image request")
        .expect("configured provider request capture was dropped");
    assert_eq!(request["model"], "gpt-5.6-sol");
    let content = request["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user" && message["content"].is_array())
        .and_then(|message| message["content"].as_array())
        .expect("the provider request has the user message parts");
    assert_eq!(content[0]["type"], "input_image");
    assert_eq!(content[1]["type"], "input_image");
    let text = content[2]["text"].as_str().unwrap();
    assert!(text.starts_with("# Important context\n\n<attachments>"));
    assert!(text.contains(&format!("image_1: id={png};")));
    assert!(text.contains(&format!("image_2: id={jpeg};")));
    assert!(text.ends_with("</attachments>\n\n# User message\n\nDescribe these attachments."));
    for (part, media_type) in content[..2].iter().zip(["image/png", "image/jpeg"]) {
        assert_eq!(part["type"], "input_image");
        assert!(
            part["image_url"]
                .as_str()
                .unwrap()
                .starts_with(&format!("data:{media_type};base64,")),
            "expected {media_type} data URL, got {part}"
        );
    }
    assert_eq!(content[2]["type"], "input_text");
    assert_eq!(content.len(), 3);
}
