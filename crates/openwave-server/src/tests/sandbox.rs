use super::*;

#[tokio::test]
async fn create_then_get_and_list() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let created: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"title": "hi"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    };
    assert_eq!(created.title.as_deref(), Some("hi"));
    assert_eq!(created.attachment_revision, 0);
    assert!(created.root_attachments.is_empty());
    assert!(serde_json::to_value(&created)
        .unwrap()
        .get("workspace_dir")
        .is_none());

    let fetched: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", created.id))
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

    let agent_runs: Vec<serde_json::Value> = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}/agent-runs", created.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    };
    assert_eq!(agent_runs.len(), 1);
    assert_eq!(
        agent_runs[0].get("id"),
        Some(&serde_json::Value::String(
            openwave_core::AgentRunId::foreground_for_chat(created.id).to_string()
        ))
    );
    let snapshot = agent_runs[0].as_object().unwrap();
    assert!(snapshot.get("lease_token").is_none());
    assert!(snapshot.get("lease_expires_at").is_none());
    assert!(snapshot.get("input").is_none());
    assert!(snapshot.get("chat_id").is_none());

    let listed: Vec<Chat> = {
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/chats")
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

#[tokio::test]
async fn agent_run_snapshots_expose_only_safe_live_sandbox_activity() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat: Chat = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    };

    let run = admit_sandbox_for_test(&store, chat.id, "research").await;
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let checkpoint = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "provider-call-identity".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({
            "query": "private query that must not reach the renderer",
            "api_key": "secret-value"
        }),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(
                run.id,
                worker_lease,
                &crate::tests::dispatchable(&checkpoint)
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("activity"),
        Some(&serde_json::json!({"kind": "web_search", "status": "waiting"}))
    );
    assert_eq!(
        snapshot.get("spawn_call_id"),
        Some(&serde_json::json!(run.spawn_call_id))
    );

    // The projection is intentionally independent of the durable checkpoint's
    // sensitive executor data.
    let encoded = serde_json::to_string(snapshot).unwrap();
    for forbidden in [
        "private query that must not reach the renderer",
        "secret-value",
        "provider-call-identity",
        "arguments",
        "lease_token",
        "result",
    ] {
        assert!(!encoded.contains(forbidden), "snapshot leaked {forbidden}");
    }

    let executor_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(checkpoint.id, executor_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        openwave_core::ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("activity"),
        Some(&serde_json::json!({"kind": "web_search", "status": "running"}))
    );
}

#[tokio::test]
async fn agent_run_activity_history_is_ordered_typed_and_names_submitted_files() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;

    let run = admit_sandbox_for_test(&store, chat.id, "research").await;
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 4, 4)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );

    let exec = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "private-exec-provider-identity".into(),
        name: openwave_core::SANDBOX_EXEC_TOOL.into(),
        arguments: serde_json::json!({
            "command": "python3",
            "args": ["report.py", "--format", "md"]
        }),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(
                run.id,
                worker_lease,
                &crate::tests::dispatchable(&exec)
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    let exec_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(exec.id, exec_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        openwave_core::ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    assert!(matches!(
        store
            .resolve_sandbox_tool_call(
                exec.id,
                exec_lease,
                &ToolCallResolution::Failed {
                    result: "exit: 17\nduration_ms: 42\n\nstderr:\nthe command's own stderr".into(),
                    error_code: "exec_command_failed".into(),
                    error_detail: Some("private executor detail".into()),
                },
            )
            .await
            .unwrap(),
        openwave_core::ResolveSandboxToolCallOutcome::Resolved
    ));

    // Continue through a second checkpoint so the endpoint also proves durable
    // ordering and the query-only search projection.
    let search_worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(search_worker_lease, chrono::Duration::minutes(5), 4, 4)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let search = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "private-search-provider-identity".into(),
        name: openwave_core::SANDBOX_WEB_SEARCH_TOOL.into(),
        arguments: serde_json::json!({
            "query": "OpenWave release notes",
            "api_key": "secret-value"
        }),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(
                run.id,
                search_worker_lease,
                &crate::tests::dispatchable(&search)
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    let search_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(search.id, search_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        openwave_core::ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    assert!(matches!(
        store
            .resolve_sandbox_tool_call(
                search.id,
                search_lease,
                &ToolCallResolution::Completed {
                    result: "private search result".into(),
                },
            )
            .await
            .unwrap(),
        openwave_core::ResolveSandboxToolCallOutcome::Resolved
    ));

    // Resolving the second checkpoint hands the run back for completion; submit
    // a file, which remains the run's durable produced result.
    let completion_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(completion_lease, chrono::Duration::minutes(5), 4, 4)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let submitted = openwave_core::OutputId::new();
    store
        .create_output(&openwave_core::CreateOutput {
            id: submitted,
            chat_id: chat.id,
            filename: "Q3 revenue.md".into(),
            kind: openwave_core::DeliverableKind::Text,
            revision: openwave_core::NewOutputRevision {
                id: openwave_core::OutputRevisionId::new(),
                byte_len: 4,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: Some(run.id),
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    store
        .submit_agent_run_submission(
            run.id,
            completion_lease,
            &[openwave_core::AgentRunSubmittedOutput {
                output_id: submitted,
                filename: "Q3 revenue.md".into(),
            }],
            "wrote the quarterly revenue summary",
        )
        .await
        .unwrap()
        .expect("completion commits");

    // The run snapshot names the submitted files without carrying their bytes.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("submitted_outputs"),
        Some(&serde_json::json!([
            {"output_id": submitted, "filename": "Q3 revenue.md"}
        ]))
    );
    assert_eq!(snapshot.get("task"), Some(&serde_json::json!("research")));
    // The names live in the structured field; the prose is the summary alone.
    assert_eq!(
        snapshot.get("terminal_text"),
        Some(&serde_json::json!("wrote the quarterly revenue summary"))
    );
    assert_eq!(snapshot.get("activity"), Some(&serde_json::json!(null)));

    // The activity history is the ordered, terminal-outcome projection.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs/{}/activity", chat.id, run.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let history: Vec<serde_json::Value> = json_body(response).await;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].get("kind"), Some(&serde_json::json!("exec")));
    assert_eq!(
        history[0].get("outcome"),
        Some(&serde_json::json!("failed"))
    );
    assert_eq!(
        history[0].get("detail"),
        Some(&serde_json::json!({
            "kind": "exec",
            "command": "python3",
            "args": ["report.py", "--format", "md"],
            "exit_code": 17,
            "output": "exit: 17\nduration_ms: 42\n\nstderr:\nthe command's own stderr",
        }))
    );
    assert_eq!(
        history[1].get("kind"),
        Some(&serde_json::json!("web_search"))
    );
    assert_eq!(
        history[1].get("outcome"),
        Some(&serde_json::json!("completed"))
    );
    assert_eq!(
        history[1].get("detail"),
        Some(&serde_json::json!({
            "kind": "search",
            "query": "OpenWave release notes",
        }))
    );
    assert!(history
        .iter()
        .all(|entry| entry.get("at").is_some_and(|at| at.is_string())));

    // The field is additive: an entry serialized before typed detail existed
    // still deserializes with no detail.
    let mut old_entry = history[0].clone();
    old_entry.as_object_mut().unwrap().remove("detail");
    let old_entry: crate::routes::AgentActivityHistoryItem =
        serde_json::from_value(old_entry).expect("the old history shape still deserializes");
    assert_eq!(old_entry.detail, None);

    // A settled exec's receipt tail is the one stored result the projection
    // carries, asserted exactly above. Nothing else crosses: not the search
    // result, not the executor's own failure detail, not the arguments a
    // search was called with, and not the durable row's other columns.
    let encoded = serde_json::to_string(&history).unwrap();
    for forbidden in [
        "private executor detail",
        "private search result",
        "secret-value",
        "private-exec-provider-identity",
        "private-search-provider-identity",
        "arguments",
        "lease_token",
    ] {
        assert!(!encoded.contains(forbidden), "history leaked {forbidden}");
    }

    // Binding to the wrong chat must not reveal that the run exists.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/chats/{}/agent-runs/{}/activity",
                    other_chat.id, run.id
                ))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agent_run_progress_is_ordered_resumable_and_bound_to_its_chat() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "research").await;

    let read = |after: Option<i64>| {
        let router = router.clone();
        let bearer = bearer.clone();
        let chat_id = chat.id;
        let run_id = run.id;
        async move {
            let query = after.map_or_else(String::new, |after| format!("?after_sequence={after}"));
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/chats/{chat_id}/agent-runs/{run_id}/progress{query}"
                        ))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body::<serde_json::Value>(response).await
        }
    };

    // A run that has published nothing reads as an empty page whose cursor is
    // the one the caller asked for, so a poller does not skip ahead.
    let empty = read(None).await;
    assert_eq!(empty["entries"], serde_json::json!([]));
    assert_eq!(empty["next_sequence"], serde_json::json!(0));

    store
        .append_agent_run_progress(run.id, "call:one", "Reading the filings.")
        .await
        .unwrap();
    store
        .append_agent_run_progress(run.id, "call:two", "Writing the summary.")
        .await
        .unwrap();
    // The same producer identity republished is the reattach case: one line,
    // not two.
    store
        .append_agent_run_progress(run.id, "call:one", "Reading the filings.")
        .await
        .unwrap();

    let page = read(None).await;
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["sequence"], serde_json::json!(1));
    assert_eq!(
        entries[0]["text"],
        serde_json::json!("Reading the filings.")
    );
    assert_eq!(entries[1]["sequence"], serde_json::json!(2));
    assert_eq!(page["next_sequence"], serde_json::json!(2));
    assert!(entries[0]["at"].is_string());

    // Resuming from the cursor returns only what arrived after it.
    let resumed = read(Some(1)).await;
    let resumed_entries = resumed["entries"].as_array().unwrap();
    assert_eq!(resumed_entries.len(), 1);
    assert_eq!(
        resumed_entries[0]["text"],
        serde_json::json!("Writing the summary.")
    );
    assert_eq!(resumed["next_sequence"], serde_json::json!(2));
    assert_eq!(read(Some(2)).await["entries"], serde_json::json!([]));

    // Binding to the wrong chat must not reveal that the run exists.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/chats/{}/agent-runs/{}/progress",
                    other_chat.id, run.id
                ))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delegated_file_routes_are_native_only_and_expose_only_exact_broker_authority() {
    let (router, token, _state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let root_id = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let resource = openwave_core::SandboxAgentFileResource {
        root_id,
        relative_path: "reports/private-summary.md".into(),
    };
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("delegated read".into()),
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 1,
        root_attachments: vec![ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::Conversation,
        }],
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    store
        .accept_turn(
            TurnId::new(),
            chat.id,
            "test-model",
            "private delegated task",
        )
        .await
        .unwrap();
    let turn_lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let turn = store
        .claim_turn_run(turn_lease, now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .append_turn_event(
            chat.id,
            turn.id,
            turn_lease,
            1,
            chrono::Utc::now(),
            &AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
    let spawn_call_id = CallId::new();
    let child_id = openwave_core::AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let child = match store
        .checkpoint_sandbox_spawn(
            &openwave_core::SandboxSpawnCheckpointRequest {
                origin_turn_id: turn.id,
                lease_token: turn_lease,
                expected_steer_revision: turn.steer_revision,
                call_id: spawn_call_id,
                provider_id: "private-spawn-provider-id".into(),
                arguments: serde_json::json!({
                    "task": "private delegated task",
                    "resource": resource,
                }),
                approval_gated: false,
                result: serde_json::to_string(&openwave_core::SpawnSandboxAgentResult {
                    agent_id: child_id,
                })
                .unwrap(),
                event_ordinal: 2,
                progress: TurnCheckpointProgress {
                    model_steps: 1,
                    usage: Usage::default(),
                },
                max_active_background_agents:
                    openwave_core::AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS,
                execution_location: openwave_core::AgentRunExecutionLocation::InProcess,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        openwave_core::CheckpointSandboxSpawnOutcome::Checkpointed { child, .. } => child,
        outcome => panic!("unexpected spawn checkpoint: {outcome:?}"),
    };
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        child.id
    );
    let call = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: child.id,
        chat_id: chat.id,
        provider_id: "private-read-provider-id".into(),
        name: openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        arguments: serde_json::json!({}),
    };
    store
        .park_agent_run_for_sandbox_tool_call(
            child.id,
            worker_lease,
            &crate::tests::dispatchable(&call),
        )
        .await
        .unwrap();

    let renderer_pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sandbox-file-reads/pending")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer_pending.status(), StatusCode::UNAUTHORIZED);
    let native_pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sandbox-file-reads/pending")
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(native_pending.status(), StatusCode::OK);
    let pending: serde_json::Value = json_body(native_pending).await;
    assert_eq!(
        pending,
        serde_json::json!([{"call_id": call.id, "claimed": false}])
    );

    let activity = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let snapshots: Vec<serde_json::Value> = json_body(activity).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot["id"] == serde_json::json!(child.id))
        .unwrap();
    assert_eq!(
        snapshot["activity"],
        serde_json::json!({"kind": "read_delegated_file", "status": "waiting"})
    );
    let lease = uuid::Uuid::new_v4();
    let claim_uri = format!("/sandbox-file-reads/{}/claim", call.id);
    let claim_body = serde_json::json!({"lease_token": lease});
    let claimed = post_native_json(&router, &bearer, &claim_uri, claim_body.clone()).await;
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed: serde_json::Value = json_body(claimed).await;
    assert_eq!(
        claimed,
        serde_json::json!({
            "disposition": "claimed",
            "call_id": call.id,
            "chat_id": chat.id,
            "root_id": root_id,
            "relative_path": resource.relative_path,
        })
    );
    let encoded = claimed.to_string();
    for forbidden in [
        "private delegated task",
        "private-spawn-provider-id",
        "private-read-provider-id",
        &lease.to_string(),
        "agent_run_id",
    ] {
        assert!(!encoded.contains(forbidden), "claim leaked {forbidden}");
    }
    let retry = post_native_json(&router, &bearer, &claim_uri, claim_body).await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(retry).await["disposition"],
        "existing"
    );
    let heartbeat_uri = format!("/sandbox-file-reads/{}/heartbeat", call.id);
    assert_eq!(
        post_native_json(
            &router,
            &bearer,
            &heartbeat_uri,
            serde_json::json!({"lease_token": uuid::Uuid::new_v4()}),
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        post_native_json(
            &router,
            &bearer,
            &heartbeat_uri,
            serde_json::json!({"lease_token": lease}),
        )
        .await
        .status(),
        StatusCode::OK
    );
    let resolve_uri = format!("/sandbox-file-reads/{}/resolve", call.id);
    let resolution = serde_json::json!({
        "lease_token": lease,
        "resolution": {"status": "failed", "reason": "not_found"},
    });
    let resolved = post_native_json(&router, &bearer, &resolve_uri, resolution.clone()).await;
    assert_eq!(resolved.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(resolved).await,
        serde_json::json!({"disposition": "resolved"})
    );
    let retried = post_native_json(&router, &bearer, &resolve_uri, resolution).await;
    assert_eq!(retried.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(retried).await["disposition"],
        "existing"
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(call.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("delegated_file_not_found")
    );
    assert_eq!(receipt.error_detail, None);
    let encoded = serde_json::to_string(&receipt).unwrap();
    for forbidden in [
        resource.relative_path.as_str(),
        &root_id.to_string(),
        "private-read-provider-id",
        "private delegated task",
    ] {
        assert!(!encoded.contains(forbidden), "receipt leaked {forbidden}");
    }

    // Activity names only the delegated file's bounded leaf name. The broker
    // root and parent path remain native-only even though the durable admission
    // retains both for authorization.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/chats/{}/agent-runs/{}/activity",
                    chat.id, child.id
                ))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let history: Vec<serde_json::Value> = json_body(response).await;
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0]["detail"],
        serde_json::json!({"kind": "file", "name": "private-summary.md"})
    );
    let encoded = serde_json::to_string(&history).unwrap();
    for forbidden in [
        resource.relative_path.as_str(),
        &root_id.to_string(),
        "private-read-provider-id",
        "private delegated task",
    ] {
        assert!(!encoded.contains(forbidden), "activity leaked {forbidden}");
    }
}

#[tokio::test]
async fn sandbox_cancel_route_is_authenticated_closed_and_idempotent() {
    let (router, token, _state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "cancel me").await;
    let uri = format!("/chats/{}/agent-runs/{}/cancel", chat.id, run.id);

    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let wrong_chat = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/agent-runs/{}/cancel", other_chat.id, run.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(wrong_chat.status(), StatusCode::CONFLICT);

    let foreground = post_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/agent-runs/{}/cancel",
            chat.id,
            openwave_core::AgentRunId::foreground_for_chat(chat.id)
        ),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(foreground.status(), StatusCode::CONFLICT);

    for expected_status in ["cancelled", "cancelled"] {
        let response = post_json(&router, &bearer, &uri, serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: serde_json::Value = json_body(response).await;
        assert_eq!(
            body,
            serde_json::json!({"id": run.id, "status": expected_status})
        );
        assert_eq!(body.as_object().unwrap().len(), 2);
        let encoded = body.to_string();
        for private in ["lease", "executor", "attempt", "claim", "reason"] {
            assert!(!encoded.contains(private), "response leaked {private}");
        }
    }
}

#[tokio::test]
async fn sandbox_cancel_route_rejects_completed_and_failed_runs() {
    let (router, token, _state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let completed = admit_sandbox_for_test(&store, chat.id, "complete").await;
    let completed_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(completed_lease, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        completed.id
    );
    store
        .submit_agent_run_result(completed.id, completed_lease, "done")
        .await
        .unwrap()
        .expect("completion commits");

    let failed_chat = make_chat(&router, &bearer).await;
    let failed = admit_sandbox_for_test(&store, failed_chat.id, "fail").await;
    for attempt in 1..=openwave_core::AgentRun::DEFAULT_MAX_ATTEMPTS {
        let lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(lease, chrono::Duration::minutes(5), 1, 1)
                .await
                .unwrap()
                .unwrap()
                .id,
            failed.id
        );
        store
            .fail_agent_run(
                failed.id,
                lease,
                "test_failure",
                "bounded detail",
                chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap()
            .expect("failure transition commits");
        if attempt < openwave_core::AgentRun::DEFAULT_MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    assert_eq!(
        store
            .get_agent_run(failed.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::Failed
    );

    for (chat_id, run_id) in [(chat.id, completed.id), (failed_chat.id, failed.id)] {
        let response = post_json(
            &router,
            &bearer,
            &format!("/chats/{chat_id}/agent-runs/{run_id}/cancel"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn sandbox_cancel_route_signals_only_the_durable_model_receipt() {
    let (router, token, state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "running").await;
    let lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(lease, chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let active = state
        .sandbox_attempts
        .register_model(run.id, lease)
        .expect("register exact model attempt");
    let unrelated_run = openwave_core::AgentRunId::sandbox_for_spawn_call(CallId::new());
    let unrelated = state
        .sandbox_attempts
        .register_model(unrelated_run, lease)
        .expect("register unrelated model attempt");

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/agent-runs/{}/cancel", chat.id, run.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        json_body::<serde_json::Value>(response).await,
        serde_json::json!({"id": run.id, "status": "cancelling"})
    );
    assert!(active.cancel_token().is_cancelled());
    assert!(!unrelated.cancel_token().is_cancelled());
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::Cancelling,
        "the durable transition commits before the local accelerator is observable"
    );

    let retry = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/agent-runs/{}/cancel", chat.id, run.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::ACCEPTED);
    assert_eq!(
        json_body::<serde_json::Value>(retry).await,
        serde_json::json!({"id": run.id, "status": "cancelling"})
    );
    assert!(!unrelated.cancel_token().is_cancelled());
}

#[tokio::test]
async fn expired_model_cancellation_still_signals_its_immutable_receipt() {
    let (router, token, state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "expired").await;
    let lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease, chrono::Duration::milliseconds(1), 1, 1)
        .await
        .unwrap()
        .expect("claim run");
    let active = state
        .sandbox_attempts
        .register_model(run.id, lease)
        .expect("register expired model attempt");
    tokio::time::sleep(Duration::from_millis(10)).await;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/agent-runs/{}/cancel", chat.id, run.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        json_body::<serde_json::Value>(response).await["status"],
        "cancelled"
    );
    assert!(active.cancel_token().is_cancelled());
}

#[tokio::test]
async fn parent_turn_cancellation_signals_its_exact_running_child_model() {
    let (router, token, state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "parent-owned model").await;
    let admission = store
        .get_sandbox_agent_admission(run.id)
        .await
        .unwrap()
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease, chrono::Duration::minutes(5), 1, 1)
        .await
        .unwrap()
        .expect("claim child");
    let active = state
        .sandbox_attempts
        .register_model(run.id, lease)
        .expect("register model");
    let unrelated_run = openwave_core::AgentRunId::sandbox_for_spawn_call(CallId::new());
    let unrelated = state
        .sandbox_attempts
        .register_model(unrelated_run, lease)
        .expect("register unrelated model");

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/cancel", chat.id),
        serde_json::json!({"turn_id": admission.origin_turn_id}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(active.cancel_token().is_cancelled());
    assert!(!unrelated.cancel_token().is_cancelled());
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::Cancelling
    );
}

#[tokio::test]
async fn parent_turn_cancellation_signals_its_exact_running_child_search() {
    let (router, token, state, store, _dir) = test_app_with_state().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let run = admit_sandbox_for_test(&store, chat.id, "parent-owned search").await;
    let admission = store
        .get_sandbox_agent_admission(run.id)
        .await
        .unwrap()
        .unwrap();
    let model_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(model_lease, chrono::Duration::minutes(5), 1, 1)
        .await
        .unwrap()
        .expect("claim child");
    let request = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "provider-call".into(),
        name: openwave_core::SANDBOX_WEB_SEARCH_TOOL.into(),
        arguments: serde_json::json!({"query":"parent cancellation"}),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(
                run.id,
                model_lease,
                &crate::tests::dispatchable(&request)
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    let search_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(request.id, search_lease, chrono::Duration::minutes(5))
            .await
            .unwrap(),
        openwave_core::ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    let active = state
        .sandbox_attempts
        .register_checkpoint(request.id, run.id, search_lease)
        .expect("register exact search");
    let stale = state
        .sandbox_attempts
        .register_checkpoint(request.id, run.id, uuid::Uuid::new_v4())
        .expect("register stale search identity");

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/cancel", chat.id),
        serde_json::json!({"turn_id": admission.origin_turn_id}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(active.cancel_token().is_cancelled());
    assert!(!stale.cancel_token().is_cancelled());
    assert_eq!(
        store
            .get_sandbox_tool_call_receipt(request.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        openwave_core::SandboxToolCallStatus::Cancelled
    );
}

#[tokio::test]
async fn agent_run_snapshots_expose_only_safe_live_foreground_folder_activity() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let historical_argument = "historical-private-argument".repeat(4_000);
    let historical_result = "historical-private-result".repeat(16_000);
    let historical = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "historical-provider-call-identity".into(),
        name: "read_connected_file".into(),
        arguments: serde_json::json!({"path": historical_argument}),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let historical = match store.accept_tool_call(&historical).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call) => call,
        outcome => panic!("unexpected historical-call admission: {outcome:?}"),
    };
    let historical_lease = uuid::Uuid::new_v4();
    let historical_now = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                historical.id,
                chat.id,
                uuid::Uuid::new_v4(),
                historical_lease,
                historical_now,
                historical_now + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    assert!(matches!(
        store
            .resolve_client_tool_call_and_append_event(
                historical.id,
                chat.id,
                historical_lease,
                chrono::Utc::now(),
                &ToolCallResolution::Completed {
                    result: historical_result.clone(),
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap()
            .outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    ));

    let root_id = "5b3e9987-5ebf-4bb0-bc6f-0c041b156027";
    let relative_path = "taxes/2026/private-return.txt";
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "provider-call-identity".into(),
        name: "read_connected_file".into(),
        arguments: serde_json::json!({
            "root_id": root_id,
            "path": relative_path,
            "grant": "private-grant"
        }),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let call = match store.accept_tool_call(&call).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call) => call,
        outcome => panic!("unexpected client-call admission: {outcome:?}"),
    };

    let snapshot = |router: Router| async {
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}/agent-runs", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let snapshots: Vec<serde_json::Value> = json_body(response).await;
        snapshots
            .into_iter()
            .find(|snapshot| snapshot.get("tier") == Some(&serde_json::json!("foreground")))
            .expect("foreground snapshot is returned")
    };

    let waiting = snapshot(router.clone()).await;
    assert_eq!(
        waiting.get("activity"),
        Some(&serde_json::json!({
            "kind": "read_connected_file",
            "status": "waiting"
        }))
    );
    let encoded = serde_json::to_string(&waiting).unwrap();
    for forbidden in [
        root_id,
        relative_path,
        "private-grant",
        "provider-call-identity",
        &historical_argument,
        &historical_result,
    ] {
        assert!(!encoded.contains(forbidden), "snapshot leaked {forbidden}");
    }

    let lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                lease,
                now,
                now + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    assert_eq!(
        snapshot(router.clone()).await.get("activity"),
        Some(&serde_json::json!({
            "kind": "read_connected_file",
            "status": "running"
        }))
    );

    assert!(matches!(
        store
            .resolve_client_tool_call_and_append_event(
                call.id,
                chat.id,
                lease,
                chrono::Utc::now(),
                &ToolCallResolution::Completed {
                    result: "private result".into(),
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap()
            .outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    ));
    assert_eq!(
        snapshot(router).await.get("activity"),
        Some(&serde_json::Value::Null)
    );
}

#[tokio::test]
async fn agent_run_snapshots_omit_persisted_raw_failure_detail() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(response).await;

    let run = admit_sandbox_for_test(&store, chat.id, "research").await;
    let raw_detail = "upstream request failed: Authorization: Bearer private-token";
    for attempt in 1..=openwave_core::AgentRun::DEFAULT_MAX_ATTEMPTS {
        let lease_token = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(lease_token, chrono::Duration::minutes(5), 1, 1)
                .await
                .unwrap()
                .unwrap()
                .id,
            run.id
        );
        assert!(store
            .fail_agent_run(
                run.id,
                lease_token,
                "sandbox_transport_failed",
                raw_detail,
                chrono::Duration::microseconds(1),
            )
            .await
            .unwrap()
            .is_some());
        if attempt < openwave_core::AgentRun::DEFAULT_MAX_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }
    assert_eq!(
        store
            .get_agent_run(run.id)
            .await
            .unwrap()
            .expect("failed run remains persisted")
            .last_error_detail
            .as_deref(),
        Some(raw_detail)
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/agent-runs", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshots: Vec<serde_json::Value> = json_body(response).await;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.get("id") == Some(&serde_json::json!(run.id)))
        .expect("sandbox snapshot is returned");
    assert_eq!(
        snapshot.get("last_error_code"),
        Some(&serde_json::json!("sandbox_transport_failed"))
    );
    assert_eq!(
        snapshot.get("terminal_text"),
        Some(&serde_json::json!(
            "Sandbox task failed (sandbox_transport_failed)"
        ))
    );
    assert!(snapshot.get("last_error_detail").is_none());
    assert!(!serde_json::to_string(snapshot)
        .unwrap()
        .contains(raw_detail));
}
