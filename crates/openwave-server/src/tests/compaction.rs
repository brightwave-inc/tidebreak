use super::*;

use configuration::put_json;

/// A provider that answers the compaction call with a real checkpoint payload
/// and everything else with one short line.
///
/// The two are told apart by the shape of the request rather than by its
/// prompt: maintenance is the only call that carries a response format and no
/// tools, and the prompt itself is core's business.
struct CheckpointProvider;

#[async_trait]
impl ModelProvider for CheckpointProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("checkpoint")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let maintenance = request.response_format.is_some() && request.tools.is_empty();
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

/// Give the install a credentialed provider, so a utility model resolves.
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

#[tokio::test(flavor = "multi_thread")]
async fn compaction_without_a_utility_model_is_refused_rather_than_reported_empty() {
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
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "compaction_utility_model_unavailable");
}
