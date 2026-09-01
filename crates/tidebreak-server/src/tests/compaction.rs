use super::*;

use configuration::put_json;

/// A provider that answers the compaction call with a real checkpoint payload
/// and everything else with one short line.
///
/// Compaction sends the conversation's own request with one instruction
/// message appended, so the trailing message is what tells the two apart. The
/// wording of that instruction is core's business; this matches the one phrase
/// it is built around.
struct CheckpointProvider;

#[async_trait]
impl ModelProvider for CheckpointProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("checkpoint")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let maintenance = request.messages.last().is_some_and(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text } if text.contains("inert semantic checkpoint"))
            })
        });
        let text = if maintenance {
            r#"{"version":2,"original_requests":[],"confirmed_decisions":["Ship the migration."],"unresolved_questions":[],"task_state":[],"source_identities":[],"output_identities":[],"conclusions":[]}"#
        } else {
            "hi"
        };
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta { text: text.into() },
            ProviderEvent::Usage(Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Default::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// Give the install a credentialed provider, so the chat's model resolves.
async fn credential_a_provider(router: &Router, bearer: &str) {
    let response = put_json(
        router,
        bearer,
        "/providers/anthropic",
        serde_json::json!({"enabled": true, "credential": {"type": "api_key", "key": "sk-test"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Compaction only pays for itself once the target is smaller than the history,
/// which no short test conversation reaches at the shipped fractions.
async fn compact_everything_but_the_last_message(router: &Router, bearer: &str) {
    let response = put_json(
        router,
        bearer,
        "/settings",
        serde_json::json!({"compaction": {
            "threshold_fraction": 0.75,
            "target_fraction": 0.01,
            "min_threshold_tokens": 1000,
            "protect_recent_messages": 1
        }}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Wait until every turn this chat has accepted is terminal.
///
/// `wait_for_turn` scans the journal from the start, so on a chat that has
/// already finished one turn it returns on that turn's terminal event rather
/// than the one just sent.
async fn wait_until_idle(store: &Arc<dyn Store>, chat: ChatId) {
    for _ in 0..500 {
        let runs = store.list_turn_runs(chat).await.unwrap();
        if !runs.is_empty() && runs.iter().all(|turn| turn.status.is_terminal()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("chat {chat} still has a running turn");
}

#[tokio::test]
async fn managed_compaction_and_message_admission_share_the_frozen_gateway_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("managed-compaction.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let resolver_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    let resolver_os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let resolver = Arc::new(resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            resolver_policy.clone(),
            resolver_os_policy.clone(),
        ),
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        resolver_policy,
        resolver_os_policy,
    ));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        resolver,
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "gpt-5.6-sol".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    let router = app(state.clone());

    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(dir.path()),
        "https://corp.gateway",
    )
    .unwrap();
    let credentials: crate::connectors::GatewayCredentials =
        serde_json::from_value(serde_json::json!({
            "base_url": "https://corp.gateway/",
            "installation_id": "install-compaction",
            "user_id": "user-1",
            "refresh_token": "mg_rt_compaction",
            "access_tokens": {}
        }))
        .unwrap();
    crate::connectors::CredentialVault::new(secrets)
        .save(&credentials)
        .await
        .unwrap();
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: "https://corp.gateway/".into(),
            installation_id: Some("install-compaction".into()),
            models: vec![providers::CustomModelConfig {
                id: "gateway-opus".into(),
                upstream_id: Some("claude-opus-5".into()),
                display_name: Some("Gateway Opus".into()),
                // Deliberately unlike the curated Anthropic row: resolving
                // the frozen key below proves compaction sees deployment
                // policy rather than merely preserving the canonical label.
                context_window: 123_456,
                max_output_tokens: 12_345,
                ..Default::default()
            }],
            model_protocols: Default::default(),
            model_reasoning_efforts: Default::default(),
            member_catalog: None,
            catalog_etag: None,
        },
    )
    .await
    .unwrap();

    let admission_chat = make_chat(&router, &bearer).await;
    let compaction_chat = make_chat(&router, &bearer).await;
    for chat_id in [admission_chat.id, compaction_chat.id] {
        store
            .set_chat_model(chat_id, Some("anthropic::claude-opus-5".into()))
            .await
            .unwrap();
    }
    let compaction_chat = store.get_chat(compaction_chat.id).await.unwrap().unwrap();
    let compaction_model = crate::routes::resolve_executable_chat_model(&state, &compaction_chat)
        .await
        .unwrap();
    assert!(
        compaction_model.starts_with("model_gateway::__tidebreak_gateway_v1."),
        "managed execution should freeze the gateway route: {compaction_model}"
    );
    let compaction_policy = providers::resolve_model_policy(&*store, &compaction_model, true, None)
        .await
        .unwrap()
        .expect("the frozen gateway selector should resolve against its snapshot");
    assert_eq!(
        compaction_policy.provider,
        providers::ProviderKind::ModelGateway
    );
    assert_eq!(compaction_policy.id, "gateway-opus");
    assert_eq!(compaction_policy.context_window, 123_456);

    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(
            &router,
            &bearer,
            admission_chat.id,
            turn_id,
            "ordinary admission"
        )
        .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().model,
        compaction_model,
        "ordinary admission and on-demand compaction must resolve one executable identity"
    );

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", compaction_chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: serde_json::Value = json_body(response).await;
    assert_eq!(run["compacted"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn compacting_on_request_checkpoints_the_chat_and_journals_it() {
    let (router, token, store, _dir) = test_app_with(Arc::new(CheckpointProvider)).await;
    let bearer = format!("Bearer {token}");
    credential_a_provider(&router, &bearer).await;
    compact_everything_but_the_last_message(&router, &bearer).await;
    let chat = make_chat(&router, &bearer).await;
    // Long enough that the tiny target cannot keep the whole conversation:
    // compaction declines when there is no prefix worth standing a summary in
    // for, which is the other test.
    for message in [
        format!(
            "decide the storage engine. {}",
            "background detail ".repeat(200)
        ),
        format!("now write the migration. {}", "further detail ".repeat(200)),
    ] {
        assert_eq!(
            send_message(&router, &bearer, chat.id, &message).await,
            StatusCode::ACCEPTED
        );
        wait_for_turn(&store, chat.id).await;
    }
    wait_until_idle(&store, chat.id).await;
    let before = store.list_events(chat.id, 0).await.unwrap().len() as i64;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", chat.id),
        serde_json::json!({"focus": "the storage engine decision"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: serde_json::Value = json_body(response).await;
    assert_eq!(run["compacted"], true);

    assert!(
        store
            .get_context_checkpoint(chat.id)
            .await
            .unwrap()
            .is_some(),
        "the pass wrote a durable checkpoint"
    );
    // The renderer learns about this the way it learns about compaction inside a
    // turn: from the journal, in order.
    let journaled: Vec<AgentEvent> = store
        .list_events(chat.id, before)
        .await
        .unwrap()
        .into_iter()
        .map(|framed| framed.event)
        .collect();
    assert!(
        matches!(
            journaled.as_slice(),
            [
                AgentEvent::CompactionStarted,
                AgentEvent::CompactionFinished { compacted: true }
            ]
        ),
        "the journal carries the pair the renderer already handles: {journaled:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn compacting_a_chat_with_nothing_to_give_up_says_so() {
    let (router, token, store, _dir) = test_app_with(Arc::new(CheckpointProvider)).await;
    let bearer = format!("Bearer {token}");
    credential_a_provider(&router, &bearer).await;
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    let before = store.list_events(chat.id, 0).await.unwrap().len() as i64;

    // Shipped fractions, one short exchange: there is no prefix worth standing
    // a summary in for.
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: serde_json::Value = json_body(response).await;
    assert_eq!(
        run["compacted"], false,
        "the caller is told nothing happened rather than left to guess"
    );
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(
        store.list_events(chat.id, before).await.unwrap().is_empty(),
        "a pass that never started reports no compaction status"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_is_refused_while_a_turn_runs() {
    let (router, token, _store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "start something long").await,
        StatusCode::ACCEPTED
    );

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "compaction_chat_busy");
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_focus_is_bounded_and_the_chat_must_exist() {
    let (router, token, _store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    for focus in [
        serde_json::json!("x".repeat(crate::routes::MAX_COMPACTION_FOCUS_CHARS + 1)),
        serde_json::json!("keep\0this"),
    ] {
        let response = post_json(
            &router,
            &bearer,
            &format!("/chats/{}/compact", chat.id),
            serde_json::json!({ "focus": focus }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", ChatId::new()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Compaction resolves the chat's own model and writes its summary there, so
/// the route requires no separately configured maintenance model: an install
/// that has none still gets an ordinary answer.
#[tokio::test(flavor = "multi_thread")]
async fn compaction_needs_no_separately_configured_maintenance_model() {
    let (router, token, _store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/compact", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: serde_json::Value = json_body(response).await;
    assert_eq!(run["compacted"], false);
}
