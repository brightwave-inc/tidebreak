use super::*;

use tidebreak_code_execution::ExecutionWorkspaceId;
use tidebreak_core::{
    AgentRunId, AssistantCitationInput, CitationLocator, CreateOutput, DeliverableKind, DocumentId,
    DocumentUpsert, MessageId, NewOutputRevision, OutputId, OutputRevisionId, ResultEntry,
    ResultEntryKind, ToolResultPreview, TurnId,
};

/// GET one route with this test's bearer.
async fn get(router: &Router, bearer: &str, uri: &str) -> axum::response::Response {
    router
        .clone()
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

/// A response body as exact bytes — what an export writes to disk.
async fn raw_body(response: axum::response::Response) -> Vec<u8> {
    assert_eq!(response.status(), StatusCode::OK);
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

/// The whole output surface, driven headlessly over the routes the desktop now
/// uses too.
///
/// A background run publishes a file; the catalog, the preview, the revision
/// history, an edit, a restore, and the raw bytes an export writes to disk all
/// come back over HTTP with no native shell involved. The two properties this
/// is really defending are that a run's published file is readable from the
/// *conversation's* scratch (publisher and reader are invisible to each other
/// alone) and that history is append-only: a restore adds a head revision and
/// every earlier revision keeps its id and its exact bytes.
#[tokio::test]
async fn a_published_output_is_listed_read_revised_restored_and_exported_over_http() {
    let (router, token, store, dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // A background run wrote a file into its own workspace under `output/`.
    let scratch_root = dir.path().join("scratch");
    let run_id = AgentRunId::sandbox_for_spawn_call(CallId::new());
    let workspace = ExecutionWorkspaceId::parse(format!("agent-run-{run_id}")).unwrap();
    let output_dir = scratch_root
        .join(workspace.as_str())
        .join(tidebreak_core::EXEC_OUTPUT_DIRECTORY);
    std::fs::create_dir_all(&output_dir).unwrap();
    let published = b"def revenue():\n    return 'up'\n";
    std::fs::write(output_dir.join("analysis.py"), published).unwrap();
    let provider = crate::code_execution::ConfiguredExecProvider::new(
        store.clone(),
        Arc::new(MemSecrets::default()),
        &scratch_root,
    );
    let scan = provider
        .collect_agent_run_outputs(&workspace, chat.id, CallId::new(), run_id)
        .await
        .unwrap();
    assert_eq!(scan.entries.len(), 1, "{:?}", scan.notes);

    // The catalog names it, badges the run that produced it, and hides host
    // state entirely.
    let catalog: serde_json::Value =
        json_body(get(&router, &bearer, &format!("/chats/{}/outputs", chat.id)).await).await;
    assert_eq!(catalog["truncated"], false);
    let summary = &catalog["deliverables"][0];
    assert_eq!(summary["filename"], "analysis.py");
    assert_eq!(summary["mediaType"], "text/plain");
    assert_eq!(summary["revisionCount"], 1);
    assert_eq!(summary["producingRunId"], run_id.to_string());
    assert_eq!(summary["sizeBytes"], published.len());
    let output_id: OutputId = summary["outputId"].as_str().unwrap().parse().unwrap();
    for forbidden in ["scratch", "agent-run", dir.path().to_str().unwrap()] {
        assert!(!catalog.to_string().contains(forbidden));
    }

    // The preview reads the current revision, and the content route serves its
    // exact bytes — the two halves an export needs.
    let outputs = format!("/chats/{}/outputs/{output_id}", chat.id);
    let preview: serde_json::Value = json_body(get(&router, &bearer, &outputs).await).await;
    assert_eq!(preview["content"], std::str::from_utf8(published).unwrap());
    assert_eq!(preview["truncated"], false);
    let first_revision: OutputRevisionId = preview["revisionId"].as_str().unwrap().parse().unwrap();
    let exported = raw_body(get(&router, &bearer, &format!("{outputs}/content")).await).await;
    assert_eq!(exported, published);

    // A user edit publishes a new head revision against the one it opened on.
    let saved: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("{outputs}/revisions"),
            serde_json::json!({
                "expectedRevisionId": first_revision,
                "content": "def revenue():\n    return 'up and to the right'\n",
            }),
        )
        .await,
    )
    .await;
    assert_eq!(saved["status"], "saved");
    let edited_revision: OutputRevisionId = saved["revisionId"].as_str().unwrap().parse().unwrap();
    assert_ne!(edited_revision, first_revision);

    // An edit against a superseded revision is a conflict naming what to
    // reload, not a failure and not a silent overwrite.
    let conflict: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("{outputs}/revisions"),
            serde_json::json!({
                "expectedRevisionId": first_revision,
                "content": "written blind",
            }),
        )
        .await,
    )
    .await;
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(
        conflict["currentRevisionId"].as_str().unwrap(),
        edited_revision.to_string()
    );

    // History shows both, attributed, with the newest current.
    let history: serde_json::Value =
        json_body(get(&router, &bearer, &format!("{outputs}/revisions")).await).await;
    let rows = history["revisions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let by_id = |id: OutputRevisionId| {
        rows.iter()
            .find(|row| row["revisionId"].as_str().unwrap() == id.to_string())
            .unwrap_or_else(|| panic!("revision {id} should still be in the history"))
    };
    assert_eq!(by_id(first_revision)["producedBy"], "backgroundAgent");
    assert_eq!(by_id(first_revision)["sources"], serde_json::json!([]));
    assert_eq!(by_id(first_revision)["ordinal"], 1);
    assert_eq!(by_id(first_revision)["isCurrent"], false);
    assert_eq!(by_id(edited_revision)["producedBy"], "user");
    assert_eq!(by_id(edited_revision)["sources"], serde_json::json!([]));
    assert_eq!(by_id(edited_revision)["isCurrent"], true);

    // Restoring is append-only: the earlier revision keeps its own bytes, and
    // the restore arrives as a third revision rather than rewinding to the
    // first.
    let restored: serde_json::Value = json_body(
        post_json(
            &router,
            &bearer,
            &format!("{outputs}/revisions/{first_revision}/restore"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    assert_eq!(restored["revisionCount"], 3);
    let history: serde_json::Value =
        json_body(get(&router, &bearer, &format!("{outputs}/revisions")).await).await;
    assert_eq!(history["revisions"].as_array().unwrap().len(), 3);
    assert_eq!(
        raw_body(
            get(
                &router,
                &bearer,
                &format!("{outputs}/content?revision_id={edited_revision}")
            )
            .await
        )
        .await,
        b"def revenue():\n    return 'up and to the right'\n"
    );
    // …and the current bytes are the restored ones.
    assert_eq!(
        raw_body(get(&router, &bearer, &format!("{outputs}/content")).await).await,
        published
    );

    // Delete is a soft retraction with an explicit undo.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&outputs)
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value =
        json_body(get(&router, &bearer, &format!("/chats/{}/outputs", chat.id)).await).await;
    assert!(catalog["deliverables"].as_array().unwrap().is_empty());
    assert_eq!(
        get(&router, &bearer, &outputs).await.status(),
        StatusCode::NOT_FOUND
    );
    let response = post_json(
        &router,
        &bearer,
        &format!("{outputs}/restore"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value =
        json_body(get(&router, &bearer, &format!("/chats/{}/outputs", chat.id)).await).await;
    assert_eq!(catalog["deliverables"].as_array().unwrap().len(), 1);

    // Another conversation cannot name this output.
    let other = make_chat(&router, &bearer).await;
    assert_eq!(
        get(
            &router,
            &bearer,
            &format!("/chats/{}/outputs/{output_id}", other.id)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_foreground_revision_projects_only_its_turns_durable_evidence() {
    let (router, token, store, dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "write the sourced brief")
        .await
        .unwrap();

    let now = chrono::Utc::now();
    let document_id = DocumentId::new();
    store
        .upsert_document(&DocumentUpsert {
            id: document_id,
            project_id: None,
            chat_id: Some(chat.id),
            origin_uri: None,
            media_type: "text/plain".into(),
            title: Some("Quarterly filing".into()),
            canonical_text: "Revenue increased in the quarter.".into(),
            updated_at: now,
        })
        .await
        .unwrap();
    let citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 1 },
    };
    store
        .append_assistant_message_with_citations(
            &Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id,
                role: Role::Assistant,
                content: tidebreak_core::format_citation_directive(
                    "Revenue increased",
                    document_id,
                    &citation.locator,
                ),
                llm_content: None,
                reasoning: Default::default(),
                created_at: now,
            },
            std::slice::from_ref(&citation),
        )
        .await
        .unwrap();

    let web_call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: web_call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: "search-1".into(),
            name: tidebreak_core::WEB_SEARCH_TOOL.into(),
            arguments: serde_json::json!({"query": "quarterly revenue"}),
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
    let web_preview = ToolResultPreview::Entries {
        entries: vec![ResultEntry::new(ResultEntryKind::Link, "SEC filing")
            .with_detail("sec.gov")
            .with_web_url("https://www.sec.gov/example")],
        failures: Vec::new(),
        elided: 0,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            web_call_id,
            &ToolCallResolution::Completed {
                result: "search complete".into(),
            },
            now,
            Some(&web_preview),
        )
        .await
        .unwrap();

    // A completed non-search tool can retain a link-shaped row, but it is not
    // retrieval evidence for the output and must not be projected as a source.
    let unrelated_call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: unrelated_call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: "read-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "notes.md"}),
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
    let unrelated_preview = ToolResultPreview::Entries {
        entries: vec![
            ResultEntry::new(ResultEntryKind::Link, "Not a search source")
                .with_web_url("https://example.com/not-evidence"),
        ],
        failures: Vec::new(),
        elided: 0,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            unrelated_call_id,
            &ToolCallResolution::Completed {
                result: "read complete".into(),
            },
            now,
            Some(&unrelated_preview),
        )
        .await
        .unwrap();

    let output_id = OutputId::new();
    let revision_id = OutputRevisionId::new();
    let bytes = b"# Revenue\n\nRevenue increased.\n";
    store
        .create_output(&CreateOutput {
            id: output_id,
            chat_id: chat.id,
            filename: "Revenue brief.md".into(),
            kind: DeliverableKind::Text,
            revision: NewOutputRevision {
                id: revision_id,
                byte_len: bytes.len() as u64,
                sha256: [0; 32],
                turn_id: Some(turn_id),
                producing_run_id: None,
                created_at: now,
            },
        })
        .await
        .unwrap();
    let revision_path = dir.path().join("scratch").join(chat.id.to_string()).join(
        tidebreak_core::output_revision_relative_path(output_id, revision_id),
    );
    std::fs::create_dir_all(revision_path.parent().unwrap()).unwrap();
    std::fs::write(revision_path, bytes).unwrap();

    let history: serde_json::Value = json_body(
        get(
            &router,
            &bearer,
            &format!("/chats/{}/outputs/{output_id}/revisions", chat.id),
        )
        .await,
    )
    .await;
    let sources = history["revisions"][0]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["kind"], "document");
    assert_eq!(sources[0]["documentId"], document_id.to_string());
    assert_eq!(
        sources[0]["locator"],
        serde_json::json!({
            "kind": "lines",
            "start": 1,
            "end": 1,
        })
    );
    assert_eq!(
        sources[1],
        serde_json::json!({
            "kind": "web",
            "url": "https://www.sec.gov/example",
            "label": "SEC filing",
            "domain": "sec.gov",
        })
    );
    assert!(!history.to_string().contains("not-evidence"));
}
