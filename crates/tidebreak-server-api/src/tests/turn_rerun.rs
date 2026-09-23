//! Regenerate, edit, and branch a Work chat through its routes.

use super::*;

/// The `(role, text)` of every message one request carried.
type SeenRequest = Vec<(Role, String)>;

/// Answers `answer 1`, `answer 2`, … and records the text of every message
/// each request carried, so a test can see what the model was shown.
#[derive(Clone, Default)]
struct NumberedProvider {
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    /// Fail the first request with an error the worker does not retry.
    fail_first: bool,
}

impl NumberedProvider {
    fn request(&self, index: usize) -> SeenRequest {
        self.requests.lock().unwrap()[index].clone()
    }
}

#[async_trait]
impl ModelProvider for NumberedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("numbered")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let seen = req
            .messages
            .iter()
            .map(|message| {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (message.role, text)
            })
            .collect::<Vec<_>>();
        let number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(seen);
            requests.len()
        };
        if self.fail_first && number == 1 {
            return Err(AgentError::MissingCredential("no key yet".into()));
        }
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: format!("answer {number}"),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

async fn transcript(router: &Router, bearer: &str, chat: SessionId) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{chat}/messages"))
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

/// `(role, content, turn_id)` for every message the transcript shows.
fn shown(transcript: &serde_json::Value) -> Vec<(String, String, String)> {
    transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| {
            (
                message["role"].as_str().unwrap().to_owned(),
                message["content"].as_str().unwrap().to_owned(),
                message["turn_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Wait until `count` turns of `chat` have finished, however they ended.
async fn wait_for_turns(store: &Arc<dyn Store>, chat: SessionId, count: usize) {
    for _ in 0..500 {
        let finished = store
            .list_turns(chat)
            .await
            .unwrap()
            .iter()
            .filter(|turn| turn.status.is_terminal())
            .count();
        if finished >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{count} turns did not finish in time");
}

async fn latest_turn(store: &Arc<dyn Store>, chat: SessionId) -> TurnId {
    store.list_turns(chat).await.unwrap().pop().unwrap().id
}

/// The user and assistant texts a request carried that mention `needle`.
fn mentions(request: &[(Role, String)], role: Role, needle: &str) -> usize {
    request
        .iter()
        .filter(|(seen_role, text)| *seen_role == role && text.contains(needle))
        .count()
}

/// Record a settled tool call in `turn`, as the agent loop would have.
async fn record_call(store: &Arc<dyn Store>, chat: SessionId, turn: TurnId, name: &str) {
    let started_at = chrono::Utc::now();
    let call_id = CallId::new();
    store
        .accept_tool_call(&ToolCallRecord {
            id: call_id,
            chat_id: chat,
            turn_id: turn,
            provider_id: format!("provider-{call_id}"),
            name: name.into(),
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
            created_at: started_at,
            resolved_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_server_tool_call(
                call_id,
                &ToolCallResolution::Completed {
                    result: "done".into(),
                },
                started_at + chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap(),
        tidebreak_core::ResolveToolCallOutcome::Resolved
    );
}

#[tokio::test]
async fn regenerating_keeps_the_earlier_answer_as_a_version_and_one_question() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "what is a tide").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;

    let second = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": second }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let started: serde_json::Value = json_body(response).await;
    assert_eq!(started["chat_id"], chat.id.to_string());
    assert_eq!(started["turn_id"], second.to_string());
    assert_eq!(started["branched"], false);
    wait_for_turns(&store, chat.id, 2).await;

    // The model saw the question once, and not the answer it replaced.
    let rerun = provider.request(1);
    assert_eq!(mentions(&rerun, Role::User, "what is a tide"), 1);
    assert_eq!(mentions(&rerun, Role::Assistant, "answer 1"), 0);

    let transcript = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        shown(&transcript),
        [
            (
                "user".to_owned(),
                "what is a tide".to_owned(),
                second.to_string()
            ),
            (
                "assistant".to_owned(),
                "answer 2".to_owned(),
                second.to_string()
            ),
        ]
    );
    let versions = transcript["answer_versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["turn_id"], first.to_string());
    assert_eq!(versions[0]["current_turn_id"], second.to_string());
    assert_eq!(versions[0]["messages"][0]["content"], "answer 1");
    assert_eq!(versions[0]["terminal_turn"]["status"], "completed");
    assert_eq!(
        transcript["terminal_turns"].as_array().unwrap().len(),
        1,
        "only the current answer's turn is part of the conversation"
    );

    // The next message continues from the current answer.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "and why").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 3).await;
    let next = provider.request(2);
    assert_eq!(mentions(&next, Role::User, "what is a tide"), 1);
    assert_eq!(mentions(&next, Role::Assistant, "answer 2"), 1);
    assert_eq!(mentions(&next, Role::Assistant, "answer 1"), 0);
}

#[tokio::test]
async fn a_retry_after_a_failure_replaces_the_failure_without_a_version() {
    let provider = NumberedProvider {
        fail_first: true,
        ..NumberedProvider::default()
    };
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "try this").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let failed = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);

    let retry = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{}/regenerate", chat.id, failed.id),
        serde_json::json!({ "new_turn_id": retry }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;

    let transcript = transcript(&router, &bearer, chat.id).await;
    let shown = shown(&transcript);
    assert_eq!(
        shown.iter().filter(|(role, _, _)| role == "user").count(),
        1,
        "a retry never stacks a second copy of the question"
    );
    assert_eq!(shown.last().unwrap().1, "answer 2");
    assert!(transcript["answer_versions"].as_array().unwrap().is_empty());
    assert!(transcript["terminal_turns"]
        .as_array()
        .unwrap()
        .iter()
        .all(|turn| turn["status"] == "completed"));
}

#[tokio::test]
async fn only_the_latest_settled_turn_can_be_rerun() {
    let (router, token, store, _dir) = test_app_with(Arc::new(NumberedProvider::default())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    for (index, question) in ["first", "second"].into_iter().enumerate() {
        assert_eq!(
            send_message(&router, &bearer, chat.id, question).await,
            StatusCode::ACCEPTED
        );
        wait_for_turns(&store, chat.id, index + 1).await;
    }
    let turns = store.list_turns(chat.id).await.unwrap();
    let earlier = turns[0].id;

    for (action, body) in [
        (
            "regenerate",
            serde_json::json!({ "new_turn_id": TurnId::new() }),
        ),
        (
            "edit",
            serde_json::json!({ "new_turn_id": TurnId::new(), "content": "changed" }),
        ),
    ] {
        let response = post_json(
            &router,
            &bearer,
            &format!("/chats/{}/turns/{earlier}/{action}", chat.id),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{action}");
        let body: serde_json::Value = json_body(response).await;
        assert_eq!(body["kind"], "turn_not_latest", "{action}");
    }

    // A turn from another chat is not a turn of this one.
    let other = make_chat(&router, &bearer).await;
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{earlier}/regenerate", other.id),
        serde_json::json!({ "new_turn_id": TurnId::new() }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_edit_of_a_turn_that_only_talked_replaces_it_in_place() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "draft question").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;

    let transcript_before = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        transcript_before["terminal_turns"][0]["side_effects"],
        serde_json::json!([]),
        "the latest turn says it changed nothing outside the conversation"
    );

    let edited = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/edit", chat.id),
        serde_json::json!({ "new_turn_id": edited, "content": "better question" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let started: serde_json::Value = json_body(response).await;
    assert_eq!(started["chat_id"], chat.id.to_string());
    assert_eq!(started["branched"], false);
    wait_for_turns(&store, chat.id, 2).await;

    let rerun = provider.request(1);
    assert_eq!(mentions(&rerun, Role::User, "draft question"), 0);
    assert_eq!(mentions(&rerun, Role::User, "better question"), 1);
    assert_eq!(mentions(&rerun, Role::Assistant, "answer 1"), 0);

    let transcript = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        shown(&transcript),
        [
            (
                "user".to_owned(),
                "better question".to_owned(),
                edited.to_string()
            ),
            (
                "assistant".to_owned(),
                "answer 2".to_owned(),
                edited.to_string()
            ),
        ]
    );
    assert!(
        transcript["answer_versions"].as_array().unwrap().is_empty(),
        "an edited turn is not an earlier version of the new one"
    );
}

#[tokio::test]
async fn an_edit_of_a_turn_that_wrote_files_starts_a_new_chat_and_says_why() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    for (index, question) in ["set things up", "write the report"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            send_message(&router, &bearer, chat.id, question).await,
            StatusCode::ACCEPTED
        );
        wait_for_turns(&store, chat.id, index + 1).await;
    }
    let wrote = latest_turn(&store, chat.id).await;
    record_call(&store, chat.id, wrote, "write_file").await;
    record_call(&store, chat.id, wrote, "mcp__linear__create_issue").await;

    let transcript_before = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        transcript_before["terminal_turns"][1]["side_effects"],
        serde_json::json!(["files_written", "connected_apps_called"])
    );

    let edited = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{wrote}/edit", chat.id),
        serde_json::json!({ "new_turn_id": edited, "content": "write a shorter report" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let started: serde_json::Value = json_body(response).await;
    assert_eq!(started["branched"], true);
    assert_eq!(
        started["side_effects"],
        serde_json::json!(["files_written", "connected_apps_called"])
    );
    let branch: SessionId = serde_json::from_value(started["chat_id"].clone()).unwrap();
    assert_ne!(branch, chat.id);
    wait_for_turns(&store, branch, 2).await;

    // The branch has the history before the edited turn, then the edit.
    let branched = transcript(&router, &bearer, branch).await;
    let texts: Vec<String> = shown(&branched)
        .into_iter()
        .map(|(_, content, _)| content)
        .collect();
    assert_eq!(
        texts,
        [
            "set things up",
            "answer 1",
            "write a shorter report",
            "answer 3"
        ]
    );
    let rerun = provider.request(2);
    assert_eq!(mentions(&rerun, Role::User, "set things up"), 1);
    assert_eq!(mentions(&rerun, Role::User, "write the report"), 0);

    // The original is untouched.
    let original = transcript(&router, &bearer, chat.id).await;
    let texts: Vec<String> = shown(&original)
        .into_iter()
        .map(|(_, content, _)| content)
        .collect();
    assert_eq!(
        texts,
        ["set things up", "answer 1", "write the report", "answer 2"]
    );

    // A retry of the same edit answers the same way instead of branching again.
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{wrote}/edit", chat.id),
        serde_json::json!({ "new_turn_id": edited, "content": "write a shorter report" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried: serde_json::Value = json_body(response).await;
    assert_eq!(retried["chat_id"], branch.to_string());
    let listings: Vec<ChatListing> = json_body(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listings.len(), 2, "one original and one branch");
    let link = listings
        .iter()
        .find(|listing| listing.chat.id == branch)
        .unwrap()
        .branched_from
        .clone()
        .expect("the branch links back to its original");
    assert_eq!(link.chat_id, chat.id);
    assert_eq!(
        link.turn_id,
        Some(store.list_turns(chat.id).await.unwrap()[0].id)
    );
}

#[tokio::test]
async fn an_edit_starts_a_new_chat_when_an_earlier_answer_acted() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "write the report").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let wrote = latest_turn(&store, chat.id).await;
    record_call(&store, chat.id, wrote, "write_file").await;

    // The answer that replaces it only talks.
    let regenerated = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{wrote}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": regenerated }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;

    // An edit takes the earlier answer out of the conversation too, so the
    // transcript warns before sending and the edit starts a new chat.
    let before = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        before["terminal_turns"][0]["side_effects"],
        serde_json::json!(["files_written"])
    );
    let edited = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{regenerated}/edit", chat.id),
        serde_json::json!({ "new_turn_id": edited, "content": "write a shorter report" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let started: serde_json::Value = json_body(response).await;
    assert_eq!(started["branched"], true);
    assert_eq!(
        started["side_effects"],
        serde_json::json!(["files_written"])
    );
    let branch: SessionId = serde_json::from_value(started["chat_id"].clone()).unwrap();
    assert_ne!(branch, chat.id);
    wait_for_turns(&store, branch, 1).await;

    // The original still pages back to the answer that wrote the file.
    let original = transcript(&router, &bearer, chat.id).await;
    assert_eq!(original["answer_versions"][0]["turn_id"], wrote.to_string());
}

#[tokio::test]
async fn a_branch_copies_the_history_through_a_turn_and_links_back() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let renamed = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({ "title": "Tide tables" }),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    for (index, question) in ["first question", "second question"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            send_message(&router, &bearer, chat.id, question).await,
            StatusCode::ACCEPTED
        );
        wait_for_turns(&store, chat.id, index + 1).await;
    }
    let first = store.list_turns(chat.id).await.unwrap()[0].id;
    record_call(&store, chat.id, first, "web_search").await;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/branch", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let branch: ChatListing = json_body(response).await;
    assert_eq!(branch.chat.title.as_deref(), Some("Tide tables (branch)"));
    let link = branch.branched_from.clone().unwrap();
    assert_eq!(link.chat_id, chat.id);
    assert_eq!(link.turn_id, Some(first));
    assert_eq!(branch.turn_count, 1);

    let copied = transcript(&router, &bearer, branch.chat.id).await;
    let texts: Vec<String> = shown(&copied)
        .into_iter()
        .map(|(_, content, _)| content)
        .collect();
    assert_eq!(texts, ["first question", "answer 1"]);
    assert_eq!(copied["tool_activity"].as_array().unwrap().len(), 1);
    assert_ne!(
        copied["messages"][0]["turn_id"],
        serde_json::json!(first.to_string()),
        "a branch owns its copy under new ids"
    );

    // The model in the branch sees the copied history and nothing after it.
    assert_eq!(
        send_message(&router, &bearer, branch.chat.id, "go another way").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, branch.chat.id, 2).await;
    let request = provider.request(2);
    assert_eq!(mentions(&request, Role::User, "first question"), 1);
    assert_eq!(mentions(&request, Role::Assistant, "answer 1"), 1);
    assert_eq!(mentions(&request, Role::User, "second question"), 0);

    // Deleting the original leaves the branch whole.
    let response = router
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
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let after = transcript(&router, &bearer, branch.chat.id).await;
    assert_eq!(shown(&after).len(), 4);
}

#[tokio::test]
async fn a_branch_from_an_earlier_version_carries_that_version() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "name a boat").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": TurnId::new() }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/branch", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let branch: ChatListing = json_body(response).await;
    let copied = transcript(&router, &bearer, branch.chat.id).await;
    let texts: Vec<String> = shown(&copied)
        .into_iter()
        .map(|(_, content, _)| content)
        .collect();
    assert_eq!(texts, ["name a boat", "answer 1"]);
}

#[tokio::test]
async fn retry_with_model_answers_under_that_model_and_leaves_the_chat_alone() {
    let provider = RecordingProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "compare these").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;
    let chat_model = provider.models.lock().unwrap()[0].clone();

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": TurnId::new(), "model": "second-opinion" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;
    assert_eq!(provider.models.lock().unwrap()[1], "second-opinion");

    // The chat keeps its own model for the turn after the retry.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "and now").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 3).await;
    assert_eq!(provider.models.lock().unwrap()[2], chat_model);
}

#[tokio::test]
async fn a_rerun_and_a_branch_carry_the_message_files() {
    let provider = NumberedProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(provider.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let accepted: serde_json::Value = json_body(
        post_raw(
            &router,
            &bearer,
            &format!("/chats/{}/documents/raw?title=brief.txt", chat.id),
            Some("text/plain"),
            b"tide heights for the week".to_vec(),
        )
        .await,
    )
    .await;
    let document: tidebreak_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/messages", chat.id),
        serde_json::json!({
            "turn_id": TurnId::new(),
            "content": "Summarize the brief",
            "file_attachments": [document],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;

    // A regenerate sends the file again with the same message.
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": TurnId::new() }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;
    let rerun = transcript(&router, &bearer, chat.id).await;
    assert_eq!(
        rerun["messages"][0]["file_attachments"][0]["document_id"],
        document.to_string()
    );
    assert_eq!(
        mentions(&provider.request(1), Role::User, &document.to_string()),
        1
    );

    // A branch owns a copy of the file, and the model in the branch is told
    // the copy's id.
    let current = latest_turn(&store, chat.id).await;
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{current}/branch", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let branch: ChatListing = json_body(response).await;
    let copied = transcript(&router, &bearer, branch.chat.id).await;
    let copy: String = copied["messages"][0]["file_attachments"][0]["document_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(copy, document.to_string());
    assert_eq!(
        send_message(&router, &bearer, branch.chat.id, "shorter please").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, branch.chat.id, 2).await;
    let request = provider.request(2);
    assert_eq!(mentions(&request, Role::User, &copy), 1);
    assert_eq!(mentions(&request, Role::User, &document.to_string()), 0);
}
