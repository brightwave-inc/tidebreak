use super::*;
#[tokio::test]
async fn document_file_content_supports_full_head_and_single_range_responses() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let raw = b"0123456789".to_vec();
    let accepted: serde_json::Value = json_body(
        post_raw(
            &router,
            &bearer,
            "/documents/raw?title=sample.pdf",
            Some("application/pdf"),
            raw.clone(),
        )
        .await,
    )
    .await;
    let uri = format!(
        "/documents/{}/file-content",
        accepted["document_id"].as_str().unwrap()
    );

    assert_eq!(
        request_document_file_content(&router, axum::http::Method::GET, &uri, None, None, None,)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let full = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(
        full.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/pdf"
    );
    assert_eq!(
        full.headers()
            .get(header::CONTENT_LENGTH)
            .unwrap()
            .to_str()
            .unwrap(),
        "10"
    );
    assert_eq!(
        full.headers()
            .get(header::ACCEPT_RANGES)
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes"
    );
    assert_eq!(
        to_bytes(full.into_body(), usize::MAX).await.unwrap(),
        raw.as_slice()
    );

    let head = request_document_file_content(
        &router,
        axum::http::Method::HEAD,
        &uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/pdf"
    );
    assert_eq!(
        head.headers()
            .get(header::CONTENT_LENGTH)
            .unwrap()
            .to_str()
            .unwrap(),
        "10"
    );
    assert!(to_bytes(head.into_body(), usize::MAX)
        .await
        .unwrap()
        .is_empty());

    for (range, expected_range, expected) in [
        ("bytes=2-5", "bytes 2-5/10", &b"2345"[..]),
        ("bytes=7-", "bytes 7-9/10", &b"789"[..]),
        ("bytes=-3", "bytes 7-9/10", &b"789"[..]),
        ("bytes=8-99", "bytes 8-9/10", &b"89"[..]),
    ] {
        let response = request_document_file_content(
            &router,
            axum::http::Method::GET,
            &uri,
            Some(&bearer),
            Some(range),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            expected_range,
            "{range}"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            expected.len().to_string(),
            "{range}"
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            expected,
            "{range}"
        );
    }

    let conditional = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &uri,
        Some(&bearer),
        Some("bytes=2-5"),
        Some("\"unsupported-validator\""),
    )
    .await;
    assert_eq!(conditional.status(), StatusCode::OK);
    assert!(conditional.headers().get(header::CONTENT_RANGE).is_none());
    assert_eq!(
        to_bytes(conditional.into_body(), usize::MAX).await.unwrap(),
        raw.as_slice()
    );

    for range in [
        "bytes=10-",
        "bytes=5-2",
        "bytes=-0",
        "bytes=0-1,3-4",
        "items=0-1",
        "malformed",
    ] {
        let response = request_document_file_content(
            &router,
            axum::http::Method::GET,
            &uri,
            Some(&bearer),
            Some(range),
            None,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{range}"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes */10",
            "{range}"
        );
        assert!(to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn document_file_content_streams_only_the_requested_range() {
    let (upload_router, token, mut state, _store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let raw: Vec<u8> = (0..(2 * 1024 * 1024))
        .map(|index| u8::try_from(index % 251).expect("remainder fits in u8"))
        .collect();
    let accepted: serde_json::Value = json_body(
        post_raw(
            &upload_router,
            &bearer,
            "/documents/raw?title=large.pdf",
            Some("application/pdf"),
            raw.clone(),
        )
        .await,
    )
    .await;
    let tracker = Arc::new(RangeOnlyBlobStore {
        bytes: Arc::new(raw.clone()),
        full_reads: AtomicUsize::new(0),
        requested_ranges: Mutex::new(Vec::new()),
    });
    state.blobs = tracker.clone();
    let router = app(state);
    let selected_range = 1_048_576_u64..1_048_608;
    let uri = format!(
        "/documents/{}/file-content",
        accepted["document_id"].as_str().unwrap()
    );

    let response = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &uri,
        Some(&bearer),
        Some("bytes=1048576-1048607"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        raw[1_048_576..1_048_608]
    );
    assert_eq!(tracker.full_reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        tracker.requested_ranges.lock().unwrap().as_slice(),
        std::slice::from_ref(&selected_range)
    );

    let head = request_document_file_content(
        &router,
        axum::http::Method::HEAD,
        &uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert!(to_bytes(head.into_body(), usize::MAX)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        tracker.requested_ranges.lock().unwrap().as_slice(),
        std::slice::from_ref(&selected_range)
    );
}

#[tokio::test]
async fn document_file_content_cancels_its_blob_stream_when_response_is_dropped() {
    let (upload_router, token, mut state, _store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let raw = b"cancellation source".to_vec();
    let accepted: serde_json::Value = json_body(
        post_raw(
            &upload_router,
            &bearer,
            "/documents/raw?title=cancel.txt",
            Some("text/plain"),
            raw.clone(),
        )
        .await,
    )
    .await;
    let stream_dropped = Arc::new(AtomicBool::new(false));
    state.blobs = Arc::new(CancellationAwareBlobStore {
        byte_len: u64::try_from(raw.len()).expect("usize always fits in u64"),
        stream_dropped: Arc::clone(&stream_dropped),
    });
    let router = app(state);
    let uri = format!(
        "/documents/{}/file-content",
        accepted["document_id"].as_str().unwrap()
    );

    let response = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!stream_dropped.load(Ordering::SeqCst));
    drop(response);
    assert!(stream_dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn document_file_content_preserves_document_scope_before_blob_access() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project = make_project(&router, &bearer).await;
    let other_project = make_project(&router, &bearer).await;
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;

    let project_document: serde_json::Value = json_body(
        post_raw(
            &router,
            &bearer,
            &format!("/projects/{}/documents/raw", project.id),
            Some("text/plain"),
            b"project original".to_vec(),
        )
        .await,
    )
    .await;
    let project_document_id: openwave_core::DocumentId = project_document["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let project_uri = format!(
        "/projects/{}/documents/{project_document_id}/file-content",
        project.id
    );
    let project_response = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &project_uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(project_response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(project_response.into_body(), usize::MAX)
            .await
            .unwrap(),
        &b"project original"[..]
    );

    for uri in [
        format!("/documents/{project_document_id}/file-content"),
        format!(
            "/projects/{}/documents/{project_document_id}/file-content",
            other_project.id
        ),
        format!(
            "/chats/{}/documents/{project_document_id}/file-content",
            chat.id
        ),
    ] {
        assert_eq!(
            request_document_file_content(
                &router,
                axum::http::Method::GET,
                &uri,
                Some(&bearer),
                None,
                None,
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }

    let chat_document: serde_json::Value = json_body(
        post_raw(
            &router,
            &bearer,
            &format!("/chats/{}/documents/raw", chat.id),
            Some("text/markdown"),
            b"# chat original".to_vec(),
        )
        .await,
    )
    .await;
    let chat_document_id: openwave_core::DocumentId = chat_document["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let chat_uri = format!(
        "/chats/{}/documents/{chat_document_id}/file-content",
        chat.id
    );
    let chat_response = request_document_file_content(
        &router,
        axum::http::Method::GET,
        &chat_uri,
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(chat_response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(chat_response.into_body(), usize::MAX)
            .await
            .unwrap(),
        &b"# chat original"[..]
    );
    assert_eq!(
        request_document_file_content(
            &router,
            axum::http::Method::GET,
            &format!(
                "/chats/{}/documents/{chat_document_id}/file-content",
                other_chat.id
            ),
            Some(&bearer),
            None,
            None,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    let source_blob = store
        .get_document(project_document_id)
        .await
        .unwrap()
        .unwrap()
        .source_blob
        .unwrap();
    openwave_core::FsBlobStore::new(dir.path().join("blobs"))
        .delete(source_blob.id)
        .unwrap();
    assert_eq!(
        request_document_file_content(
            &router,
            axum::http::Method::GET,
            &project_uri,
            None,
            None,
            None,
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request_document_file_content(
            &router,
            axum::http::Method::GET,
            &format!("/documents/{project_document_id}/file-content"),
            Some(&bearer),
            None,
            None,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request_document_file_content(
            &router,
            axum::http::Method::GET,
            &project_uri,
            Some(&bearer),
            None,
            None,
        )
        .await
        .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn raw_ingest_retains_exact_bytes_and_decodes_before_responding() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let raw = b"raw \xff source\n".to_vec();
    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fraw.txt",
        Some("text/plain; charset=utf-8"),
        raw.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        document_id,
        openwave_core::DocumentId::derive("file:///raw.txt")
    );

    let document = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(document.media_type, "text/plain; charset=utf-8");
    let source_blob = document.source_blob.unwrap();
    let blobs = openwave_core::FsBlobStore::new(dir.path().join("blobs"));
    assert_eq!(blobs.get(source_blob.id).await.unwrap().unwrap(), raw);
    assert_eq!(document.canonical_text, String::from_utf8_lossy(&raw));
}

#[tokio::test]
async fn streamed_raw_ingest_accepts_large_chunked_sources_and_deduplicates_by_content() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    // One byte over the legacy raw route's limit proves this endpoint is not
    // collected by Axum's buffered-body extractor before blob publication.
    let raw = vec![b'x'; MAX_RAW_DOCUMENT_BYTES + 1];
    let source_blob = openwave_core::DocumentSourceBlob::from_bytes(&raw);
    let sha256: String = source_blob
        .sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let uri = format!(
        "/chats/{}/documents/raw-stream?title=large.md&sha256={sha256}&byte_len={}",
        chat.id,
        raw.len()
    );

    let response = post_streamed_raw(
        &router,
        &bearer,
        &uri,
        "text/markdown",
        vec![raw[..64 * 1024].to_vec(), raw[64 * 1024..].to_vec()],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(response).await;
    assert_eq!(accepted["already_present"], false);
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        document_id,
        openwave_core::DocumentId::derive_for_chat_content(chat.id, source_blob.sha256)
    );
    assert_eq!(
        openwave_core::FsBlobStore::new(dir.path().join("blobs"))
            .get(source_blob.id)
            .await
            .unwrap()
            .as_deref(),
        Some(raw.as_slice())
    );

    let response = post_streamed_raw(&router, &bearer, &uri, "text/markdown", vec![raw]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let duplicate: serde_json::Value = json_body(response).await;
    assert_eq!(duplicate["already_present"], true);
    assert_eq!(duplicate["document_id"], accepted["document_id"]);
    assert_eq!(
        store
            .list_document_ids(openwave_core::DocumentScope::Chat(chat.id))
            .await
            .unwrap(),
        vec![document_id]
    );
}

#[tokio::test]
async fn raw_ingest_enforces_media_type_body_and_project_scope() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let missing_type = post_raw(&router, &bearer, "/documents/raw", None, b"body".to_vec()).await;
    assert_eq!(missing_type.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(missing_type).await;
    assert_eq!(error.kind, "bad_request");

    let empty = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        Vec::new(),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let project = make_project(&router, &bearer).await;
    let response = post_raw(
        &router,
        &bearer,
        &format!("/projects/{}/documents/raw", project.id),
        Some("text/markdown"),
        b"# scoped".to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id = accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        store
            .get_document(document_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project.id)
    );
}

#[tokio::test]
async fn raw_ingest_persists_a_safe_title_without_requiring_a_source_path() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?title=meeting%20notes.md",
        Some("text/markdown"),
        b"# Notes".to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id = accepted["document_id"].as_str().unwrap().parse().unwrap();
    let document = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(document.title.as_deref(), Some("meeting notes.md"));
    assert_eq!(document.source_uri, None);

    let spoofed = post_raw(
        &router,
        &bearer,
        "/documents/raw?title=report%E2%80%AEtxt.md",
        Some("text/markdown"),
        b"# Notes".to_vec(),
    )
    .await;
    assert_eq!(spoofed.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn raw_ingest_has_an_explicit_limit_and_preserves_payload_too_large() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let boundary = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        vec![b'x'; MAX_RAW_DOCUMENT_BYTES],
    )
    .await;
    assert_eq!(boundary.status(), StatusCode::CREATED);

    let too_large = post_raw(
        &router,
        &bearer,
        "/documents/raw",
        Some("text/plain"),
        vec![b'x'; MAX_RAW_DOCUMENT_BYTES + 1],
    )
    .await;
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: AgentErrorInfo = json_body(too_large).await;
    assert_eq!(error.kind, "payload_too_large");
}

#[tokio::test]
async fn project_document_routes_enforce_corpus_identity_and_ownership() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project_a = make_project(&router, &bearer).await;
    let project_b = make_project(&router, &bearer).await;
    let uri = "file:///shared-source.txt";

    let root: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "loose corpus zephyr"}),
        )
        .await,
    )
    .await;
    let a: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_a.id),
            serde_json::json!({"uri": uri, "content": "project alpha aurora"}),
        )
        .await,
    )
    .await;
    let b: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_b.id),
            serde_json::json!({"uri": uri, "content": "project beta nebula"}),
        )
        .await,
    )
    .await;

    assert_eq!(
        root["document_id"],
        openwave_core::DocumentId::derive(uri).to_string()
    );
    assert_eq!(
        a["document_id"],
        openwave_core::DocumentId::derive_for_project(project_a.id, uri).to_string()
    );
    assert_eq!(
        b["document_id"],
        openwave_core::DocumentId::derive_for_project(project_b.id, uri).to_string()
    );
    assert_ne!(root["document_id"], a["document_id"]);
    assert_ne!(a["document_id"], b["document_id"]);

    let request = |method: axum::http::Method, uri: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let a_id = a["document_id"].as_str().unwrap();
    let b_id = b["document_id"].as_str().unwrap();
    let listing: serde_json::Value = json_body(
        request(
            axum::http::Method::GET,
            format!("/projects/{}/documents", project_a.id),
        )
        .await,
    )
    .await;
    assert_eq!(listing["documents"].as_array().unwrap().len(), 1);
    assert_eq!(listing["documents"][0]["document_id"], a["document_id"]);
    assert_eq!(
        listing["documents"][0]["project_id"],
        project_a.id.to_string()
    );

    assert_eq!(
        request(
            axum::http::Method::GET,
            format!("/projects/{}/documents/{b_id}", project_a.id),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            axum::http::Method::DELETE,
            format!("/projects/{}/documents/{a_id}", project_b.id),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(axum::http::Method::DELETE, format!("/documents/{a_id}"),)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(axum::http::Method::GET, format!("/documents/{a_id}"),)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store
            .get_document(a_id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project_a.id)
    );

    let unknown = ProjectId::new();
    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{unknown}/documents"),
            serde_json::json!({"uri": uri, "content": "orphan"}),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            axum::http::Method::DELETE,
            format!("/projects/{}/documents/{a_id}", project_a.id),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn chat_document_routes_isolate_sources_and_delete_lifecycle() {
    let (router, token, store, _dir) = test_app_with(Arc::new(FakeProvider)).await;
    let bearer = format!("Bearer {token}");
    let first = make_chat(&router, &bearer).await;
    let second = make_chat(&router, &bearer).await;

    let uri = "file:///shared-name.txt";
    let first_ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/chats/{}/documents", first.id),
            serde_json::json!({"uri": uri, "content": "first conversation source"}),
        )
        .await,
    )
    .await;
    let second_ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/chats/{}/documents", second.id),
            serde_json::json!({"uri": uri, "content": "second conversation source"}),
        )
        .await,
    )
    .await;
    let first_id: openwave_core::DocumentId = first_ingest["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let second_id: openwave_core::DocumentId = second_ingest["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(first_id, second_id);
    assert_eq!(
        store.get_document(first_id).await.unwrap().unwrap().chat_id,
        Some(first.id)
    );
    assert_eq!(
        store
            .get_document(second_id)
            .await
            .unwrap()
            .unwrap()
            .chat_id,
        Some(second.id)
    );

    let get = |uri: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    let first_list: serde_json::Value =
        json_body(get(format!("/chats/{}/documents", first.id)).await).await;
    let second_list: serde_json::Value =
        json_body(get(format!("/chats/{}/documents", second.id)).await).await;
    assert_eq!(first_list["documents"].as_array().unwrap().len(), 1);
    assert_eq!(
        first_list["documents"][0]["document_id"],
        first_id.to_string()
    );
    assert_eq!(second_list["documents"].as_array().unwrap().len(), 1);
    assert_eq!(
        second_list["documents"][0]["document_id"],
        second_id.to_string()
    );
    // The happy path, which this test asserted around rather than through: the
    // conversation that owns a source can fetch it by id. Without this, a
    // detail route that never resolves anything still passes the isolation
    // assertions below.
    let owned: serde_json::Value =
        json_body(get(format!("/chats/{}/documents/{first_id}", first.id)).await).await;
    assert_eq!(owned["document_id"], first_id.to_string());
    assert_eq!(
        get(format!("/chats/{}/documents/{second_id}", first.id))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(format!("/documents/{first_id}")).await.status(),
        StatusCode::NOT_FOUND
    );
    let legacy_list: serde_json::Value = json_body(get("/documents".into()).await).await;
    assert!(legacy_list["documents"].as_array().unwrap().is_empty());

    let deleted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", first.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(store.get_document(first_id).await.unwrap(), None);
    assert!(store.get_document(second_id).await.unwrap().is_some());
}

#[tokio::test]
async fn document_catalog_pages_metadata_and_keeps_project_content_private() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///catalog.txt",
                "media_type": "text/markdown",
                "content": "# Catalog\n\nDurable source",
            }),
        )
        .await,
    )
    .await;
    let id = ingested["document_id"].as_str().unwrap().to_owned();

    for suffix in ["second", "third"] {
        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({
                    "uri": format!("file:///{suffix}.txt"),
                    "content": format!("{suffix} document"),
                }),
            )
            .await
            .status(),
            StatusCode::CREATED
        );
    }

    let project = make_project(&router, &bearer).await;
    let project_document_id = openwave_core::DocumentId::new();
    let now = chrono::Utc::now();
    store
        .create_document(&openwave_core::DocumentRecord {
            chat_id: None,
            id: project_document_id,
            project_id: Some(project.id),
            source_uri: Some("file:///project-secret.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            source_blob: None,
            canonical_text: "project-only source".into(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();

    let get = |uri: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let first = get("/documents?limit=2".into()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = json_body(first).await;
    let first_documents = first["documents"].as_array().unwrap();
    assert_eq!(first_documents.len(), 2);
    let cursor = first["next_cursor"].as_str().expect("a second page");
    assert!(first_documents.iter().all(|summary| {
        summary.get("content").is_none() && summary.get("revision_token").is_none()
    }));

    let second = get(format!("/documents?limit=2&cursor={cursor}")).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = json_body(second).await;
    let second_documents = second["documents"].as_array().unwrap();
    assert_eq!(second_documents.len(), 1);
    assert!(second["next_cursor"].is_null());

    let listed_ids: std::collections::HashSet<_> = first_documents
        .iter()
        .chain(second_documents)
        .map(|summary| summary["document_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed_ids.len(), 3);
    assert!(listed_ids.contains(id.as_str()));
    assert!(!listed_ids.contains(project_document_id.to_string().as_str()));

    let catalog_summary = first_documents
        .iter()
        .chain(second_documents)
        .find(|summary| summary["document_id"] == id)
        .unwrap();
    assert_eq!(catalog_summary["uri"], "file:///catalog.txt");
    assert_eq!(catalog_summary["media_type"], "text/markdown");
    assert_eq!(
        catalog_summary["source_byte_len"],
        serde_json::json!("# Catalog\n\nDurable source".len())
    );
    assert_eq!(catalog_summary["readable"], true);
    let detail = get(format!("/documents/{id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: serde_json::Value = json_body(detail).await;
    assert_eq!(detail["content"], "# Catalog\n\nDurable source");
    assert_eq!(detail["document_id"], id);
    assert!(detail.get("revision_token").is_none());

    assert_eq!(
        get(format!("/documents/{project_document_id}"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        get("/documents?limit=0".into()).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get("/documents?cursor=garbage".into()).await.status(),
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        get(format!("/documents/{}", openwave_core::DocumentId::new()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn document_catalog_cursor_preserves_nanosecond_ordering() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let mut expected = Vec::new();

    for nanos in [900, 800, 700] {
        let id = openwave_core::DocumentId::new();
        let created_at = chrono::DateTime::from_timestamp(1_700_000_000, nanos).unwrap();
        store
            .create_document(&openwave_core::DocumentRecord {
                id,
                chat_id: None,
                project_id: None,
                source_uri: Some(format!("file:///{nanos}.txt")),
                media_type: "text/plain".into(),
                title: None,
                source_blob: None,
                canonical_text: nanos.to_string(),
                created_at,
                updated_at: created_at,
            })
            .await
            .unwrap();
        expected.push(id.to_string());
    }

    let mut uri = "/documents?limit=1".to_owned();
    let mut actual = Vec::new();
    loop {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: serde_json::Value = json_body(response).await;
        let documents = page["documents"].as_array().unwrap();
        assert_eq!(documents.len(), 1);
        actual.push(documents[0]["document_id"].as_str().unwrap().to_owned());
        let Some(cursor) = page["next_cursor"].as_str() else {
            break;
        };
        uri = format!("/documents?limit=1&cursor={cursor}");
    }

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn concurrent_same_document_ingests_are_last_write_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("concurrent-ingest.db").display()
        ))
        .await
        .unwrap(),
    );
    let blobs = Arc::new(FirstPutGatedBlobStore {
        inner: openwave_core::FsBlobStore::new(dir.path().join("blobs")),
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Notify::new(),
    });
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
    state.blobs = blobs.clone();
    let token = state.token.clone();
    let router = app(state);
    let bearer = format!("Bearer {token}");

    let first_router = router.clone();
    let first_bearer = bearer.clone();
    let first = tokio::spawn(async move {
        post_json(
            &first_router,
            &first_bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "first version",
            }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), blobs.entered.notified())
        .await
        .expect("first request did not reach blob publication");

    let second_router = router.clone();
    let second_bearer = bearer.clone();
    let second = tokio::spawn(async move {
        post_json(
            &second_router,
            &second_bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "second version",
            }),
        )
        .await
    });
    let second = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .expect("later request was blocked by an earlier publication")
        .unwrap();
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(
        blobs.calls.load(Ordering::SeqCst),
        2,
        "independent requests publish their blobs without a document write lock"
    );
    blobs.release.notify_one();
    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let record = store
        .get_document(openwave_core::DocumentId::derive("file:///concurrent.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.canonical_text, "first version");
    assert!(record.source_blob.is_some());
}

#[tokio::test]
async fn re_ingesting_the_same_uri_is_idempotent() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let doc = serde_json::json!({
        "uri": "file:///notes.txt",
        "content": "one two three four five six seven eight nine ten",
    });

    let first: serde_json::Value =
        json_body(post_json(&router, &bearer, "/documents", doc.clone()).await).await;
    let second: serde_json::Value =
        json_body(post_json(&router, &bearer, "/documents", doc).await).await;
    // Same URI => same derived document id => replaced in place.
    assert_eq!(first["document_id"], second["document_id"]);
    let document_id = first["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        store
            .get_document(document_id)
            .await
            .unwrap()
            .unwrap()
            .canonical_text,
        "one two three four five six seven eight nine ten"
    );
}

#[tokio::test]
async fn a_padded_uri_targets_the_same_document() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Surrounding whitespace must not change the derived document id, or
    // "re-ingest the same file" would silently create a second document.
    let padded: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "  file:///a.txt  ", "content": "hello world" }),
        )
        .await,
    )
    .await;
    let clean: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "file:///a.txt", "content": "hello world" }),
        )
        .await,
    )
    .await;
    assert_eq!(padded["document_id"], clean["document_id"]);
}

#[tokio::test]
async fn ingest_rejects_empty_content() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let bad_ingest = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({ "content": "   " }),
    )
    .await;
    assert_eq!(bad_ingest.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_accepts_any_media_type_via_the_fallback_parser() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // A text-like body with an unrecognized media type is accepted and remains
    // directly readable because the fallback decodes valid UTF-8.
    let textual = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fnotes.log",
        Some("application/octet-stream"),
        b"level=info service started ok".to_vec(),
    )
    .await;
    assert_eq!(textual.status(), StatusCode::CREATED);
    let textual_id: openwave_core::DocumentId = json_body::<serde_json::Value>(textual).await
        ["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Binary PDF bytes reach the fallback parser and are retained without
    // decoded canonical text.
    let binary = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fdocument.pdf",
        Some("application/pdf"),
        b"%PDF-1.7\n%\xFF\xFF\n".to_vec(),
    )
    .await;
    assert_eq!(binary.status(), StatusCode::CREATED);
    let binary_id: openwave_core::DocumentId = json_body::<serde_json::Value>(binary).await
        ["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let textual_doc = store.get_document(textual_id).await.unwrap().unwrap();
    assert_eq!(textual_doc.canonical_text, "level=info service started ok");

    let binary_doc = store.get_document(binary_id).await.unwrap().unwrap();
    assert!(binary_doc.canonical_text.is_empty());
    assert_eq!(
        openwave_core::SourceReadiness::of(binary_doc.is_readable()),
        openwave_core::SourceReadiness::StoredNoText
    );
}

#[tokio::test]
async fn agent_deps_registers_server_tools_and_closed_foreground_capabilities() {
    struct UnavailableCodeExecution;

    #[async_trait::async_trait]
    impl openwave_code_execution::CodeExecutionProvider for UnavailableCodeExecution {
        async fn execute(
            &self,
            _request: openwave_code_execution::CodeExecutionRequest,
        ) -> std::result::Result<
            openwave_code_execution::CodeExecutionResponse,
            openwave_code_execution::CodeExecutionError,
        > {
            Err(openwave_code_execution::CodeExecutionError::NotConfigured)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("agent-deps.db").display()
        ))
        .await
        .unwrap(),
    );
    let extract_store = store.clone();
    let (mut tools, config) = agent_deps(
        Arc::new(UnavailableCodeExecution),
        Box::new(web_search::foreground_tool(
            store.clone(),
            Arc::new(MemSecrets::default()),
        )),
        Box::new(web_search::foreground_extract_tool(
            extract_store,
            Arc::new(MemSecrets::default()),
        )),
        store,
    );
    assert!(
        config.system_prompt.is_none(),
        "prompt must not be frozen before boot-time tools are mounted"
    );
    tools.register_client(ToolSpec {
        name: "mcp__test__lookup".into(),
        description: "untrusted remote metadata must not enter the operating prompt".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "private_runtime_value": {"default": "must-not-be-copied"}
            }
        }),
    });
    let surface = crate::turn_worker::freeze_foreground_turn_surface(Arc::new(tools), &config);
    let system_prompt = surface
        .agent_config
        .system_prompt
        .as_deref()
        .expect("production foreground turns receive an operating prompt");
    assert!(system_prompt.contains("You are OpenWave"));
    assert!(system_prompt.contains("## User clarification"));
    assert!(system_prompt.contains("## Conversation sources and citations"));
    assert!(system_prompt.contains("## Connected folders"));
    assert!(system_prompt.contains("## Background delegation"));
    assert!(system_prompt.contains("## External MCP tools"));
    assert!(!system_prompt.contains("mcp__test__lookup"));
    assert!(!system_prompt.contains("must-not-be-copied"));
    let names: Vec<String> = surface.tools.specs().into_iter().map(|s| s.name).collect();
    assert!(
        names.iter().any(|n| n == "list_sources"),
        "source listing tool registered"
    );
    assert!(
        names.iter().any(|n| n == "read_source"),
        "direct source read tool registered"
    );
    assert!(
        names.iter().any(|n| n == "read_file"),
        "file tools still present"
    );
    assert!(
        names.iter().any(|n| n == "create_deliverable"),
        "deliverable tool registered"
    );
    assert!(
        names
            .iter()
            .any(|n| n == openwave_code_execution::EXEC_TOOL_NAME),
        "code execution tool registered"
    );
    assert!(
        names.iter().any(|n| n == "web_search"),
        "foreground web search registered"
    );
    assert_eq!(
        surface.tools.get("web_search").unwrap().approval_class(),
        ApprovalClass::Sensitive
    );
    assert!(
        names.iter().any(|n| n == "web_extract"),
        "foreground web extraction registered beside web search"
    );
    assert_eq!(
        surface.tools.get("web_extract").unwrap().approval_class(),
        ApprovalClass::Sensitive
    );
    assert!([
        openwave_core::ASK_USER_QUESTIONS_TOOL,
        openwave_core::SPAWN_SANDBOX_AGENT_TOOL,
        openwave_core::WAIT_FOR_AGENTS_TOOL,
    ]
    .iter()
    .all(|orchestration| !names.iter().any(|name| name == orchestration)));
    let foreground = surface
        .tools
        .specs_for_foreground(true)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::HashSet<_>>();
    assert!([
        openwave_core::ASK_USER_QUESTIONS_TOOL,
        openwave_core::SPAWN_SANDBOX_AGENT_TOOL,
        openwave_core::WAIT_FOR_AGENTS_TOOL,
    ]
    .iter()
    .all(|name| foreground.contains(*name)));
    assert_eq!(
        surface
            .tools
            .execution(openwave_core::REQUEST_FOLDER_ACCESS_TOOL),
        Some(ToolCallExecution::Client)
    );
    // Importing a connected file is a native client continuation like the other
    // folder tools: the server advertises the contract but never executes it,
    // and a payload the broker would refuse cannot be checkpointed.
    assert_eq!(
        surface
            .tools
            .execution(openwave_core::IMPORT_CONNECTED_FILE_TOOL),
        Some(ToolCallExecution::Client)
    );
    assert!(surface
        .tools
        .get(openwave_core::IMPORT_CONNECTED_FILE_TOOL)
        .is_none());
    assert!(foreground.contains(openwave_core::IMPORT_CONNECTED_FILE_TOOL));
    assert!(surface.tools.client_arguments_are_valid(
        openwave_core::IMPORT_CONNECTED_FILE_TOOL,
        &serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "reports/q3.pdf" })
    ));
    assert!(!surface.tools.client_arguments_are_valid(
        openwave_core::IMPORT_CONNECTED_FILE_TOOL,
        &serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "../secret.pdf" })
    ));
    assert!(surface
        .tools
        .get(openwave_core::REQUEST_FOLDER_ACCESS_TOOL)
        .is_none());
    let spec = surface
        .tools
        .specs()
        .into_iter()
        .find(|spec| spec.name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL)
        .expect("folder consent tool is advertised");
    assert_eq!(spec, openwave_core::request_folder_access_tool_spec());
    assert!(surface.tools.client_arguments_are_valid(
        openwave_core::REQUEST_FOLDER_ACCESS_TOOL,
        &serde_json::json!({
            "reason": "Read the reports needed for this project",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents"
        })
    ));
    assert!(!surface.tools.client_arguments_are_valid(
        openwave_core::REQUEST_FOLDER_ACCESS_TOOL,
        &serde_json::json!({
            "reason": "Read reports",
            "requested_capabilities": ["write_files"],
            "path": "/Users/example/Documents"
        })
    ));
}

#[tokio::test]
async fn catalog_delete_failure_leaves_source_stale_and_repairable() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("delete-failure.db").display()
        ))
        .await
        .unwrap(),
    );
    let store = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    let (router, token, store_view, _dir) =
        test_app_from_parts(Arc::new(FakeProvider), store.clone(), dir);
    let bearer = format!("Bearer {token}");
    let uri = "file:///delete-failure.txt";
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "rebuildable source"}),
        )
        .await,
    )
    .await;
    let id = ingested["document_id"].as_str().unwrap().to_string();

    store.fail_next_document_delete();
    let delete = |id: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/documents/{id}"))
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(
        delete(id.clone()).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let record = store_view
        .get_document(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.canonical_text, "rebuildable source");
    assert!(record.source_blob.is_some());
    assert_eq!(delete(id).await.status(), StatusCode::ACCEPTED);
}
