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
async fn raw_ingest_retains_exact_bytes_and_runs_the_async_pipeline() {
    let (router, token, store, dir, worker) = test_app_with_worker().await;
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        document_id,
        openwave_core::DocumentId::derive("file:///raw.txt")
    );

    let pending = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(pending.media_type, "text/plain; charset=utf-8");
    let source_blob = pending.source_blob.unwrap();
    let blobs = openwave_core::FsBlobStore::new(dir.path().join("blobs"));
    assert_eq!(blobs.get(source_blob.id).await.unwrap().unwrap(), raw);

    run_parse_and_index(&worker).await;
    let ready = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(ready.canonical_text, String::from_utf8_lossy(&raw));
    assert_eq!(
        ready.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
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

/// End-to-end proof that a PDF ingested through the raw route parses (via the
/// liteparse parser registered in the real pipeline) and indexes to `Ready`,
/// with the extracted text as canonical text. Only runs with the parser feature.
#[cfg(feature = "parse-liteparse")]
#[tokio::test]
async fn raw_ingest_parses_and_indexes_a_pdf_end_to_end() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let pdf = include_bytes!("../fixtures/minimal.pdf").to_vec();

    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Freport.pdf",
        Some("application/pdf"),
        pdf,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();

    run_parse_and_index(&worker).await;

    let ready = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(
        ready.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
    assert!(
        ready.canonical_text.contains("liteparse ingest report"),
        "expected the PDF's extracted text as canonical text, got: {:?}",
        ready.canonical_text
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
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
async fn headless_embedding_keeps_document_api_on_its_primary_bearer() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .oneshot(
            Request::builder()
                .uri("/documents")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
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
    assert_eq!(boundary.status(), StatusCode::ACCEPTED);

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
async fn ingest_then_search_finds_the_passage() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");

    let ingest = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({
            "uri": "file:///solar.txt",
            "content": "Jupiter is the largest planet in the Solar System, a gas giant.",
        }),
    )
    .await;
    assert_eq!(ingest.status(), StatusCode::ACCEPTED);
    let ingest: serde_json::Value = json_body(ingest).await;
    assert!(ingest["document_id"].is_string());
    assert!(ingest["job_id"].is_string());
    assert_eq!(ingest["processing_status"], "queued");
    let document_id = ingest["document_id"].as_str().unwrap().parse().unwrap();
    let record = store
        .get_document(document_id)
        .await
        .unwrap()
        .expect("source record should be durable before the response");
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 1);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );

    run_parse_and_index(&worker).await;

    // The worker's activated generation is searchable over the shared index.
    let search = post_json(
        &router,
        &bearer,
        "/search",
        serde_json::json!({ "query": "largest gas giant planet", "k": 1 }),
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let results: serde_json::Value = json_body(search).await;
    let citations = results["citations"].as_array().unwrap();
    assert_eq!(citations.len(), 1);
    assert!(citations[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("Jupiter"));
    assert_eq!(citations[0]["document_id"], ingest["document_id"]);
    let response_json = serde_json::to_string(&results).unwrap();
    assert!(!response_json.contains("file:///solar.txt"));
    assert!(!response_json.contains(&record.revision_token.to_string()));
    assert!(!response_json.contains("revision_token"));
    assert!(!response_json.contains("content_revision"));
    assert!(citations[0].get("source").is_none());
    assert!(citations[0].get("generation").is_none());
}

#[tokio::test]
async fn maximum_search_output_and_private_evidence_commit_together() {
    let (router, token, store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let embedder = Arc::new(HashEmbedder::default());
    let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    for ordinal in 0..openwave_retrieval::MAX_SEARCH_RESULTS {
        let document_id = openwave_core::DocumentId::new();
        let generation = openwave_core::DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
        };
        let mut snippet = format!("fact {ordinal} ");
        snippet.push_str(&"x".repeat(1_024 - snippet.len()));
        let embedding = embedder.embed_query(&snippet).await.unwrap();
        vectors
            .stage_document_generation(
                document_id,
                generation,
                vec![VectorRecord {
                    chat_id: Some(chat.id),
                    project_id: None,
                    source: openwave_retrieval::DocumentSource::Inline,
                    generation: Some(generation),
                    chunk: openwave_retrieval::Chunk::new(
                        document_id,
                        0,
                        openwave_core::ByteSpan::new(0, snippet.len()),
                        snippet,
                    ),
                    embedding,
                }],
            )
            .await
            .unwrap();
        assert!(vectors
            .activate_document_generation(document_id, generation)
            .await
            .unwrap());
    }
    let tool = openwave_retrieval::SearchTool::new(embedder, vectors);
    let output = tool
        .execute(
            &ToolCtx::new_legacy_workspace(chat.id, None, dir.path().to_path_buf()),
            serde_json::json!({"query": "fact", "k": 9999}),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(
        output.private_evidence.len(),
        openwave_retrieval::MAX_SEARCH_RESULTS
    );
    assert!(output.content.len() <= ToolCallRecord::MAX_RESULT_BYTES);

    let created_at = chrono::Utc::now();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "max_search".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "fact", "k": 9999}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &ToolCallResolution::Completed {
                    result: output.content.clone(),
                },
                created_at + chrono::Duration::seconds(1),
                &output.private_evidence,
            )
            .await
            .unwrap(),
        openwave_core::ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store.list_retrieval_evidence(call.id).await.unwrap().len(),
        output.private_evidence.len()
    );
}

#[tokio::test]
async fn project_document_routes_enforce_corpus_identity_and_ownership() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
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

    for _ in 0..6 {
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
    }

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
        post_json(
            &router,
            &bearer,
            &format!("/documents/{a_id}/retry"),
            serde_json::Value::Null,
        )
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

    let root_search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "loose corpus zephyr", "k": 1}),
        )
        .await,
    )
    .await;
    assert_eq!(
        root_search["citations"][0]["document_id"],
        root["document_id"]
    );
    let a_search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/search", project_a.id),
            serde_json::json!({"query": "project beta nebula", "k": 1}),
        )
        .await,
    )
    .await;
    assert_eq!(a_search["citations"][0]["document_id"], a["document_id"]);

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
async fn chat_document_routes_isolate_sources_search_and_delete_lifecycle() {
    let embedder = Arc::new(HashEmbedder::default());
    let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    let (retrieval, _search) = build_retrieval(embedder.clone(), vectors.clone());
    let (router, token, store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
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

    for (chat_id, text) in [
        (first.id, "alpha-only evidence"),
        (second.id, "beta-only evidence"),
    ] {
        let document_id = openwave_core::DocumentId::new();
        vectors
            .upsert(vec![VectorRecord {
                chat_id: Some(chat_id),
                project_id: None,
                source: openwave_retrieval::DocumentSource::Inline,
                generation: None,
                chunk: openwave_retrieval::Chunk::new(
                    document_id,
                    0,
                    openwave_retrieval::ByteSpan::new(0, text.len()),
                    text,
                ),
                embedding: embedder.embed_query(text).await.unwrap(),
            }])
            .await
            .unwrap();
    }
    let search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/chats/{}/search", first.id),
            serde_json::json!({"query": "alpha-only evidence", "k": 10}),
        )
        .await,
    )
    .await;
    let rendered = serde_json::to_string(&search).unwrap();
    assert!(rendered.contains("alpha-only evidence"));
    assert!(!rendered.contains("beta-only evidence"));

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
    assert!(store
        .get_pending_document_retirement(first_id)
        .await
        .unwrap()
        .is_some());
    assert!(store.get_document(second_id).await.unwrap().is_some());
}

#[tokio::test]
async fn failed_indexing_leaves_authoritative_source_stale_for_retry() {
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(FailingEmbedder),
        Arc::new(InMemoryVectorStore::new(8)),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");

    let response = post_json(
        &router,
        &bearer,
        "/documents",
        serde_json::json!({
            "uri": "file:///retry.txt",
            "content": "authoritative even when embedding fails",
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(_)
    ));
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::RetryScheduled(_)
    ));

    let record = store
        .get_document(openwave_core::DocumentId::derive("file:///retry.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.canonical_text,
        "authoritative even when embedding fails"
    );
    assert_eq!(record.content_revision, 1);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );
}

#[tokio::test]
async fn explicit_retry_revives_the_exact_terminal_job() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///manual-retry.txt",
                "content": "retry the exact failed generation"
            }),
        )
        .await,
    )
    .await;
    let id: openwave_core::DocumentId = ingested["document_id"].as_str().unwrap().parse().unwrap();
    let job_id: openwave_core::DocumentJobId =
        ingested["job_id"].as_str().unwrap().parse().unwrap();
    let now = chrono::Utc::now();
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, job_id);
    assert_eq!(
        store
            .record_document_job_failure(
                job_id,
                claimed.lease_token.unwrap(),
                chrono::Utc::now(),
                None,
                "manual_test_failure",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/documents/{id}/retry"))
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried = store.get_document_job(job_id).await.unwrap().unwrap();
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.id, job_id);
}

#[tokio::test]
async fn explicit_retry_selects_the_failed_parse_stage() {
    let (router, token, store, dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let raw = b"parse retry succeeds through the durable worker".to_vec();
    let source_blob = openwave_core::DocumentSourceBlob::from_bytes(&raw);
    let blob_id = source_blob.id;
    let blobs = openwave_core::FsBlobStore::new(dir.path().join("blobs"));
    openwave_core::BlobStore::put(&blobs, blob_id, raw.clone())
        .await
        .unwrap();
    let source = openwave_core::DocumentSourceUpsert {
        chat_id: None,
        id: openwave_core::DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///parse-retry.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        source_blob,
        updated_at: chrono::Utc::now(),
    };
    let (_, parse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "plain-text-lossy-v1", 1)
        .await
        .unwrap();
    let claim_at = parse_job.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, parse_job.id);
    assert_eq!(
        store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "parse_failed",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/documents/{}/retry", source.id))
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried = store.get_document_job(parse_job.id).await.unwrap().unwrap();
    assert_eq!(retried.id, parse_job.id);
    assert_eq!(retried.kind, openwave_core::DocumentJobKind::Parse);
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, document_stage::MAX_PARSE_ATTEMPTS);

    assert_eq!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(parse_job.id)
    );
    let jobs = store.list_document_jobs(source.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Succeeded);
    assert_eq!(jobs[1].kind, openwave_core::DocumentJobKind::Index);
    assert_eq!(jobs[1].status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(jobs[1].id)
    );
    let document = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(document.canonical_text.as_bytes(), raw);
    assert_eq!(
        document.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
}

#[tokio::test]
async fn project_retry_revives_only_the_owned_terminal_job() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let project_a = make_project(&router, &bearer).await;
    let project_b = make_project(&router, &bearer).await;
    let ingested: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents", project_a.id),
            serde_json::json!({
                "uri": "file:///project-manual-retry.txt",
                "content": "retry only within the owning project"
            }),
        )
        .await,
    )
    .await;
    let document_id: openwave_core::DocumentId =
        ingested["document_id"].as_str().unwrap().parse().unwrap();
    let job_id: openwave_core::DocumentJobId =
        ingested["job_id"].as_str().unwrap().parse().unwrap();
    let now = chrono::Utc::now();
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, job_id);
    assert_eq!(
        store
            .record_document_job_failure(
                job_id,
                claimed.lease_token.unwrap(),
                chrono::Utc::now(),
                None,
                "project_manual_test_failure",
                None,
            )
            .await
            .unwrap(),
        Some(openwave_core::DocumentJobStatus::Failed)
    );

    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/documents/{document_id}/retry"),
            serde_json::Value::Null,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents/{document_id}/retry", project_b.id),
            serde_json::Value::Null,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store
            .get_document_job(job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        openwave_core::DocumentJobStatus::Failed
    );

    let response = post_json(
        &router,
        &bearer,
        &format!("/projects/{}/documents/{document_id}/retry", project_a.id),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response: serde_json::Value = json_body(response).await;
    assert_eq!(response["document_id"], document_id.to_string());
    assert_eq!(response["job_id"], job_id.to_string());
    let retried = store.get_document_job(job_id).await.unwrap().unwrap();
    assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.id, job_id);
}

#[tokio::test]
async fn failed_update_keeps_the_prior_active_generation_searchable() {
    let embedder = Arc::new(FailAfterFirstBatchEmbedder {
        inner: HashEmbedder::default(),
        calls: AtomicUsize::new(0),
    });
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        embedder,
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///updated.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "obsolete searchable phrase"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    run_parse_and_index(&worker).await;
    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "replacement failed to embed"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Completed(_)
    ));
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::RetryScheduled(_)
    ));

    let search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "obsolete searchable phrase"}),
        )
        .await,
    )
    .await;
    assert_eq!(search["citations"].as_array().unwrap().len(), 1);
    assert!(search["citations"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("obsolete"));
    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.canonical_text, "replacement failed to embed");
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
}

#[tokio::test]
async fn update_enqueues_without_calling_legacy_vector_retirement() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store.clone(),
    ));
    let (router, token, store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///retirement.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "still authoritative"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    vector_store.fail_next_delete();
    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "must not publish"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );

    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(record.indexed_at, None);
}

#[tokio::test]
async fn first_ingest_persists_source_without_attempting_vector_retirement() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    vector_store.fail_next_delete();
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store,
    ));
    let (router, token, store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let uri = "file:///first-source.txt";

    assert_eq!(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({"uri": uri, "content": "source comes first"}),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    let record = store
        .get_document(openwave_core::DocumentId::derive(uri))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.indexed_revision, None);
    assert_eq!(
        record.processing_status,
        openwave_core::DocumentProcessingStatus::Queued
    );
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
            StatusCode::ACCEPTED
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
            canonical_fingerprint: None,
            source_regions: Vec::new(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: openwave_core::DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at: now,
            updated_at: now,
            indexed_at: None,
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
    assert_eq!(catalog_summary["content_revision"], 1);
    assert_eq!(catalog_summary["processing_status"], "queued");
    assert!(catalog_summary["indexed_revision"].is_null());

    let detail = get(format!("/documents/{id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: serde_json::Value = json_body(detail).await;
    assert_eq!(detail["content"], "");
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
                canonical_fingerprint: None,
                source_regions: Vec::new(),
                content_revision: 1,
                revision_token: uuid::Uuid::new_v4(),
                processing_status: openwave_core::DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at,
                updated_at: created_at,
                indexed_at: None,
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
async fn concurrent_same_document_ingests_publish_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("concurrent-ingest.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
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
        retrieval.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.blobs = blobs.clone();
    let token = state.token.clone();
    let worker = document_worker::DocumentWorker::new(
        store.clone(),
        state.blobs.clone(),
        retrieval,
        state.document_job_wake.clone(),
        state.document_writes.clone(),
        document_worker::DocumentWorkerConfig::default(),
    );
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
    let mut second = tokio::spawn(async move {
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
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "later request must not complete while the first publication is blocked"
    );
    assert_eq!(
        blobs.calls.load(Ordering::SeqCst),
        1,
        "later request must block before publishing its blob"
    );
    blobs.release.notify_one();
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    assert_eq!(blobs.calls.load(Ordering::SeqCst), 2);
    let record = store
        .get_document(openwave_core::DocumentId::derive("file:///concurrent.txt"))
        .await
        .unwrap()
        .unwrap();
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.content_revision, 2);
    assert_eq!(record.indexed_revision, None);
    let jobs = store.list_document_jobs(record.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Cancelled);
    assert_eq!(jobs[1].status, openwave_core::DocumentJobStatus::Queued);

    run_parse_and_index(&worker).await;
    let record = store.get_document(record.id).await.unwrap().unwrap();
    assert_eq!(record.canonical_text, "second version");
    assert_eq!(record.indexed_revision, Some(2));
}

#[tokio::test]
async fn deleting_a_document_removes_it_from_the_index() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");
    let ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "uri": "file:///doc.txt", "content": "Jupiter is a gas giant." }),
        )
        .await,
    )
    .await;
    let id = ingest["document_id"].as_str().unwrap().to_string();
    run_parse_and_index(&worker).await;

    let delete = |id: String| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/documents/{id}"))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Retired(_)
    ));
    // Gone from the index.
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "gas giant" }),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
    // Idempotent: deleting again is still accepted.
    assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
    assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
}

#[tokio::test]
async fn durable_worker_retries_a_failed_tombstone_publication() {
    let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        vector_store.clone(),
    ));
    let (router, token, store, _dir, worker) =
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
    let bearer = format!("Bearer {token}");
    let ingest: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///retry-delete.txt",
                "content": "retire this searchable source"
            }),
        )
        .await,
    )
    .await;
    run_parse_and_index(&worker).await;
    let id = ingest["document_id"].as_str().unwrap();
    vector_store.fail_next_delete();

    let delete = |id: &str| {
        let router = router.clone();
        let bearer = bearer.clone();
        let uri = format!("/documents/{id}");
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(delete(id).await, StatusCode::ACCEPTED);
    assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
    assert!(worker.run_once().await.is_err());
    let visible: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "searchable source"}),
        )
        .await,
    )
    .await;
    assert_eq!(visible["citations"].as_array().unwrap().len(), 1);

    assert!(matches!(
        worker.run_once().await.unwrap(),
        document_worker::WorkerOutcome::Retired(_)
    ));
    let cleared: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "searchable source"}),
        )
        .await,
    )
    .await;
    assert!(cleared["citations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn re_ingesting_the_same_uri_is_idempotent() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
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
    assert_eq!(first["job_id"], second["job_id"]);
    assert_eq!(first["content_revision"], second["content_revision"]);
    let document_id = first["document_id"].as_str().unwrap().parse().unwrap();
    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, openwave_core::DocumentJobKind::Parse);
    assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Queued);
    run_parse_and_index(&worker).await;

    // A broad search still returns each chunk once, not doubled.
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "three four five", "k": 50 }),
        )
        .await,
    )
    .await;
    let citations = results["citations"].as_array().unwrap();
    let ids: std::collections::HashSet<_> = citations
        .iter()
        .map(|c| c["chunk_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), citations.len());
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
async fn ingest_rejects_empty_content_and_search_rejects_empty_query() {
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

    let bad_search = post_json(
        &router,
        &bearer,
        "/search",
        serde_json::json!({ "query": "  " }),
    )
    .await;
    assert_eq!(bad_search.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_accepts_any_media_type_via_the_fallback_parser() {
    let (router, token, store, _dir, worker) = test_app_with_worker().await;
    let bearer = format!("Bearer {token}");

    // A text-like body with an unrecognized media type is accepted and stays
    // searchable: the fallback parser decodes valid UTF-8 into canonical text.
    let textual = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fnotes.log",
        Some("application/octet-stream"),
        b"level=info service started ok".to_vec(),
    )
    .await;
    assert_eq!(textual.status(), StatusCode::ACCEPTED);
    let textual_id: openwave_core::DocumentId = json_body::<serde_json::Value>(textual).await
        ["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Binary bytes with an unclaimed media type are accepted too, but retained
    // without decoded canonical text so they never pollute the search index.
    let binary = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fblob.bin",
        Some("application/octet-stream"),
        vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF],
    )
    .await;
    assert_eq!(binary.status(), StatusCode::ACCEPTED);
    let binary_id: openwave_core::DocumentId = json_body::<serde_json::Value>(binary).await
        ["document_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Two documents ⇒ two parse jobs and two index jobs.
    run_parse_and_index(&worker).await;
    run_parse_and_index(&worker).await;

    let textual_doc = store.get_document(textual_id).await.unwrap().unwrap();
    assert_eq!(
        textual_doc.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
    assert_eq!(textual_doc.canonical_text, "level=info service started ok");

    let binary_doc = store.get_document(binary_id).await.unwrap().unwrap();
    assert_eq!(
        binary_doc.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
    assert!(binary_doc.canonical_text.is_empty());
}

#[tokio::test]
async fn search_on_an_empty_index_returns_no_citations() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "anything" }),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn root_search_never_returns_project_owned_vectors() {
    let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
    vectors
        .upsert(vec![VectorRecord {
            chat_id: None,
            project_id: Some(ProjectId::new()),
            source: openwave_retrieval::DocumentSource::Inline,
            generation: None,
            chunk: openwave_retrieval::Chunk::new(
                openwave_core::DocumentId::new(),
                0,
                openwave_retrieval::ByteSpan::new(0, 14),
                "project secret",
            ),
            embedding: Embedding(vec![0.0; HashEmbedder::DEFAULT_DIMS]),
        }])
        .await
        .unwrap();
    let (retrieval, _search) = build_retrieval(Arc::new(HashEmbedder::default()), vectors);
    let (router, token, _store, _dir) =
        test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
    let results: serde_json::Value = json_body(
        post_json(
            &router,
            &format!("Bearer {token}"),
            "/search",
            serde_json::json!({"query": "project secret"}),
        )
        .await,
    )
    .await;
    assert!(results["citations"].as_array().unwrap().is_empty());
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
    let (_retrieval, mut tools, config) = agent_deps(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        Arc::new(UnavailableCodeExecution),
        Box::new(web_search::foreground_tool(
            store.clone(),
            Arc::new(MemSecrets::default()),
        )),
        Box::new(web_search::foreground_extract_tool(
            store.clone(),
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
        names.iter().any(|n| n == "search"),
        "search tool registered"
    );
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
    let retrieval = Arc::new(Retriever::new(
        Box::new(PlainTextParser::new()),
        Box::new(TextChunker::default()),
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    ));
    let (router, token, store_view, _dir) =
        test_app_from_parts(Arc::new(FakeProvider), retrieval, store.clone(), dir);
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
    assert!(record.canonical_text.is_empty());
    assert!(record.source_blob.is_some());
    assert_eq!(record.indexed_revision, None);
    assert_eq!(record.index_fingerprint, None);
    assert_eq!(record.indexed_at, None);

    let search: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({"query": "rebuildable source"}),
        )
        .await,
    )
    .await;
    assert!(search["citations"].as_array().unwrap().is_empty());
    assert_eq!(delete(id).await.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn resolve_embedder_uses_openai_only_when_enabled_and_keyed() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets = MemSecrets::default();
    providers::write_credential(
        &secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-openai-test"),
    )
    .await
    .unwrap();

    // Enabled + keyed → the real 1536-dim embedder. A stored credential takes
    // precedence over any env var, so this is deterministic; construction only,
    // no network call.
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            vertex_location: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let online = resolve_embedder(&*store, &secrets, true).await;
    assert_eq!(online.dimensions(), EMBED_DIMS);
    assert_ne!(EMBED_DIMS, HashEmbedder::default().dimensions());

    // Enabled + keyed but BYOK disallowed (managed profile, or an unreadable
    // policy) → document text must never egress through the stored key.
    let locked = resolve_embedder(&*store, &secrets, false).await;
    assert_eq!(locked.dimensions(), HashEmbedder::default().dimensions());

    // Disabled but keyed → the key is ignored (no silent egress), even though
    // it's present. Deterministic regardless of any ambient OPENAI_API_KEY,
    // since a disabled provider never consults the key at all.
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: false,
            base_url: None,
            vertex_location: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let offline = resolve_embedder(&*store, &secrets, true).await;
    assert_eq!(offline.dimensions(), HashEmbedder::default().dimensions());
}

#[cfg(feature = "vec-lance")]
#[tokio::test(flavor = "multi_thread")]
async fn connect_vector_store_opens_a_durable_lance_index_under_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());

    // Ingest into the store, then reopen from the same data_dir and confirm the
    // chunk survived — i.e. bind()'s production path really persists to disk.
    {
        let store = connect_vector_store(&config, 2).await.unwrap();
        let doc = openwave_retrieval::DocumentId::new();
        let chunk =
            openwave_retrieval::Chunk::new(doc, 0, openwave_retrieval::ByteSpan::new(0, 4), "note");
        store
            .upsert(vec![openwave_retrieval::VectorRecord {
                chat_id: None,
                project_id: None,
                source: openwave_retrieval::DocumentSource::Inline,
                generation: None,
                chunk,
                embedding: openwave_retrieval::Embedding(vec![1.0, 0.0]),
            }])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }
    assert!(
        dir.path().join("vectors").exists(),
        "lance dir created under data_dir"
    );
    let reopened = connect_vector_store(&config, 2).await.unwrap();
    assert_eq!(reopened.len().await.unwrap(), 1);
}

/// The lean build's stand-in still ingests and searches — it just forgets. A
/// release build never reaches it (`build.rs` rejects a release build without
/// `vec-lance`), so this pins the development behaviour rather than a shipped
/// one: the store works, sized to the embedder, and writes nothing to disk.
#[cfg(not(feature = "vec-lance"))]
#[tokio::test(flavor = "multi_thread")]
async fn connect_vector_store_falls_back_to_an_in_memory_index_without_vec_lance() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());

    let store = connect_vector_store(&config, 2).await.unwrap();
    let doc = openwave_retrieval::DocumentId::new();
    let chunk =
        openwave_retrieval::Chunk::new(doc, 0, openwave_retrieval::ByteSpan::new(0, 4), "note");
    store
        .upsert(vec![openwave_retrieval::VectorRecord {
            chat_id: None,
            project_id: None,
            source: openwave_retrieval::DocumentSource::Inline,
            generation: None,
            chunk,
            embedding: openwave_retrieval::Embedding(vec![1.0, 0.0]),
        }])
        .await
        .unwrap();
    assert_eq!(store.len().await.unwrap(), 1);

    assert!(
        !dir.path().join("vectors").exists(),
        "the in-memory store leaves no index on disk"
    );
    // A second connect starts empty: nothing carried over from the first.
    let reopened = connect_vector_store(&config, 2).await.unwrap();
    assert_eq!(reopened.len().await.unwrap(), 0);
}

/// What the renderer may read in a native embedding, which is the configuration
/// the desktop runs and the one no other test in this module builds.
///
/// The reader's two routes — the narrowed detail projection and the bytes — are
/// on the primary bearer, because a renderer holds only that and is the thing
/// drawing the document. Before this, the whole document surface sat behind the
/// native-only credential, so every viewer in the desktop got a 401 and none of
/// them had ever loaded a file.
///
/// The full-fidelity surface stays where it was, and that is asserted here too:
/// its catalog carries `uri`, which for an unscoped source is a real filesystem
/// path. `root_attachment::embedded_renderer_bearer_cannot_reach_canonical_document_routes`
/// is the test that guards that, and it must keep passing unchanged.
#[tokio::test]
async fn a_native_embedding_serves_the_renderer_the_document_it_draws() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new_with_client_executor_id(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
        Uuid::new_v4(),
    )
    .unwrap();
    assert!(state.root_attachment_routes_enabled);
    let bearer = format!("Bearer {}", state.token);
    let executor = state.client_executor_token.to_string();
    let router = app(state);

    let chat = make_chat(&router, &bearer).await;
    // Ingest is full-fidelity and stays native-only, so the host's credential
    // sets the fixture up. The uri stands in for one a connected-folder import
    // records; it is the field the renderer must not be handed.
    let ingest: serde_json::Value = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{}/documents", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .header(crate::auth::CLIENT_EXECUTOR_HEADER, &executor)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "uri": "file:///Users/private/quarterly-sentinel.md",
                            "content": "Revenue rose.",
                            "media_type": "text/markdown",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let document_id = ingest["document_id"].as_str().unwrap().to_owned();

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

    // The document the reader opens, and the bytes its viewer draws.
    let detail = get(format!("/chats/{}/documents/{document_id}", chat.id)).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let body = String::from_utf8_lossy(&to_bytes(detail.into_body(), usize::MAX).await.unwrap())
        .into_owned();
    let detail: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["document_id"], document_id);
    // Present and a string; no worker runs here, so it is empty until one parses
    // the source. What matters is that the field the viewers read is served.
    assert!(detail["content"].is_string());
    assert_eq!(detail["media_type"], "text/markdown");
    assert_eq!(
        get(format!(
            "/chats/{}/documents/{document_id}/file-content",
            chat.id
        ))
        .await
        .status(),
        StatusCode::OK
    );

    // Nothing about where the file came from, and no index bookkeeping, travels
    // with it. This is the projection's whole purpose.
    for sentinel in [
        "/Users/private",
        "quarterly-sentinel",
        "uri",
        "content_revision",
        "index_fingerprint",
    ] {
        assert!(
            !body.contains(sentinel),
            "renderer detail leaked {sentinel}"
        );
    }

    // The full-fidelity surface and host authority are both still withheld.
    assert_eq!(
        get(format!("/chats/{}/documents", chat.id)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get("/root-attachment-changes/pending".into())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
