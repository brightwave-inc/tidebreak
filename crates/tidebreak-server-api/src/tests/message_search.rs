//! `GET /search/messages` through the real router: what a hit names, what
//! reruns, branches, archiving, incognito, and deletes do to it, and who can
//! see it.

use super::conversations::patch_chat;
use super::*;

/// Answers `answer 1`, `answer 2`, … so each answer is its own search term.
#[derive(Clone, Default)]
struct NumberedAnswers {
    requests: Arc<AtomicUsize>,
    /// Fail this many first requests with an error the worker does not retry.
    fail_requests: usize,
}

#[async_trait]
impl ModelProvider for NumberedAnswers {
    fn id(&self) -> ProviderId {
        ProviderId::new("numbered")
    }

    async fn stream(
        &self,
        _req: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        if number <= self.fail_requests {
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

/// Search for `words`, expecting a page.
async fn search(router: &Router, bearer: &str, words: &str) -> tidebreak_core::MessageSearchPage {
    let uri = format!(
        "/search/messages?q={}",
        url::form_urlencoded::byte_serialize(words.as_bytes()).collect::<String>()
    );
    let response = get(router, bearer, &uri).await;
    assert_eq!(response.status(), StatusCode::OK, "{words}");
    json_body(response).await
}

/// `(source, session, turn)` of every hit, newest first.
fn hits(
    page: &tidebreak_core::MessageSearchPage,
) -> Vec<(
    tidebreak_core::MessageSearchSource,
    SessionId,
    Option<TurnId>,
)> {
    page.hits
        .iter()
        .map(|hit| (hit.source, hit.session_id, hit.turn_id))
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

/// Two people on one self-hosted machine: each finds what they wrote, and
/// nothing the other did, however the words overlap.
#[tokio::test]
async fn a_member_never_finds_another_persons_messages() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tokens.path(),
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let (router, _dir) = standalone_app(|config| {
        config.auth_tokens_file = Some(tokens.path().to_owned());
    })
    .await;
    let alice = format!("Bearer {ALICE_TOKEN}");
    let bob = format!("Bearer {BOB_TOKEN}");
    let alice_chat = make_chat(&router, &alice).await;
    let bob_chat = make_chat(&router, &bob).await;
    assert_eq!(
        send_message(&router, &alice, alice_chat.id, "harbour payroll figures").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send_message(&router, &bob, bob_chat.id, "harbour lunch plans").await,
        StatusCode::ACCEPTED
    );

    let mine = search(&router, &alice, "harbour").await;
    assert_eq!(
        mine.hits
            .iter()
            .map(|hit| hit.session_id)
            .collect::<Vec<_>>(),
        [alice_chat.id]
    );
    assert!(search(&router, &bob, "payroll").await.hits.is_empty());
    let theirs = search(&router, &bob, "harbour").await;
    assert_eq!(
        theirs
            .hits
            .iter()
            .map(|hit| hit.session_id)
            .collect::<Vec<_>>(),
        [bob_chat.id]
    );
    assert!(theirs.indexing.complete);
}

#[tokio::test]
async fn a_hit_names_the_chat_message_and_turn_to_open() {
    let (router, token, store, _dir) = test_app_with(Arc::new(NumberedAnswers::default())).await;
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
    assert_eq!(
        send_message(&router, &bearer, chat.id, "When is <b>high</b> tide?").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let turn = latest_turn(&store, chat.id).await;
    let messages = store.list_messages(chat.id).await.unwrap();

    let page = search(&router, &bearer, "tide").await;
    assert_eq!(page.hits.len(), 1);
    let hit = &page.hits[0];
    assert_eq!(hit.kind, tidebreak_core::MessageSearchKind::Chat);
    assert_eq!(hit.session_id, chat.id);
    assert_eq!(hit.title.as_deref(), Some("Tide tables"));
    assert_eq!(hit.turn_id, Some(turn));
    assert_eq!(hit.message_id, Some(messages[0].id));
    assert_eq!(hit.event_seq, None);
    assert_eq!(hit.source, tidebreak_core::MessageSearchSource::User);
    // The snippet is the text as written, never markup of the server's own.
    assert_eq!(hit.snippet, "When is <b>high</b> tide?");
    assert_eq!(
        hit.ranges,
        [tidebreak_core::MessageSearchRange { start: 20, end: 24 }]
    );

    let answer = search(&router, &bearer, "answer").await;
    assert_eq!(
        hits(&answer),
        [(
            tidebreak_core::MessageSearchSource::Assistant,
            chat.id,
            Some(turn)
        )]
    );
}

/// A regenerated answer is an earlier version and an edited question no
/// longer exists: neither is a hit, and the question stays one hit under
/// the turn that answers it now.
#[tokio::test]
async fn reruns_leave_only_the_current_version_in_search() {
    let (router, token, store, _dir) = test_app_with(Arc::new(NumberedAnswers::default())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "what is a tide").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let first = latest_turn(&store, chat.id).await;

    let regenerated = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{first}/regenerate", chat.id),
        serde_json::json!({ "new_turn_id": regenerated }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;

    use tidebreak_core::MessageSearchSource::{Assistant, User};
    assert!(search(&router, &bearer, "answer 1").await.hits.is_empty());
    assert_eq!(
        hits(&search(&router, &bearer, "answer 2").await),
        [(Assistant, chat.id, Some(regenerated))]
    );
    assert_eq!(
        hits(&search(&router, &bearer, "tide").await),
        [(User, chat.id, Some(regenerated))]
    );

    let edited = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{regenerated}/edit", chat.id),
        serde_json::json!({ "new_turn_id": edited, "content": "what is a wave" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 3).await;

    assert!(search(&router, &bearer, "tide").await.hits.is_empty());
    assert!(search(&router, &bearer, "answer 2").await.hits.is_empty());
    assert_eq!(
        hits(&search(&router, &bearer, "wave").await),
        [(User, chat.id, Some(edited))]
    );
    assert_eq!(
        hits(&search(&router, &bearer, "answer 3").await),
        [(Assistant, chat.id, Some(edited))]
    );
}

/// A retry sends the question again, and the transcript shows it once under
/// the retry. Search does the same.
#[tokio::test]
async fn a_retried_question_is_one_hit_under_the_retry() {
    let provider = NumberedAnswers {
        fail_requests: 1,
        ..NumberedAnswers::default()
    };
    let (router, token, store, _dir) = test_app_with(Arc::new(provider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "try the harbour").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let failed = latest_turn(&store, chat.id).await;

    let retry = TurnId::new();
    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{failed}/retry", chat.id),
        serde_json::json!({ "new_turn_id": retry }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_for_turns(&store, chat.id, 2).await;

    assert_eq!(
        hits(&search(&router, &bearer, "harbour").await),
        [(
            tidebreak_core::MessageSearchSource::User,
            chat.id,
            Some(retry)
        )]
    );
}

#[tokio::test]
async fn a_branch_is_searchable_as_soon_as_it_exists() {
    let (router, token, store, _dir) = test_app_with(Arc::new(NumberedAnswers::default())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "copy these tide notes").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let turn = latest_turn(&store, chat.id).await;

    let response = post_json(
        &router,
        &bearer,
        &format!("/chats/{}/turns/{turn}/branch", chat.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let branch: ChatListing = json_body(response).await;

    let mut sessions: Vec<SessionId> = search(&router, &bearer, "tide notes")
        .await
        .hits
        .iter()
        .map(|hit| hit.session_id)
        .collect();
    sessions.sort_by_key(|id| id.0);
    let mut expected = vec![chat.id, branch.chat.id];
    expected.sort_by_key(|id| id.0);
    assert_eq!(sessions, expected);
}

#[tokio::test]
async fn archiving_incognito_and_deleting_through_the_routes_keep_search_current() {
    let (router, token, store, _dir) = test_app_with(Arc::new(NumberedAnswers::default())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "lighthouse keeper").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;

    let archived = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({ "archived": true }),
    )
    .await;
    assert_eq!(archived.status(), StatusCode::OK);
    let page = search(&router, &bearer, "lighthouse").await;
    assert_eq!(page.hits.len(), 1);
    assert!(
        page.hits[0].archived,
        "an archived chat is found and marked"
    );
    let restored = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({ "archived": false }),
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert!(!search(&router, &bearer, "lighthouse").await.hits[0].archived);

    let incognito = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({ "memory_incognito": true }),
    )
    .await;
    assert_eq!(incognito.status(), StatusCode::OK);
    assert!(search(&router, &bearer, "lighthouse").await.hits.is_empty());
    let back = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({ "memory_incognito": false }),
    )
    .await;
    assert_eq!(back.status(), StatusCode::OK);
    assert_eq!(search(&router, &bearer, "lighthouse").await.hits.len(), 1);

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
    assert!(search(&router, &bearer, "lighthouse").await.hits.is_empty());
}

#[tokio::test]
async fn a_malformed_search_is_refused_and_punctuation_alone_matches_nothing() {
    let (router, token, _store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let long = "a".repeat(501);
    for uri in [
        "/search/messages".to_owned(),
        "/search/messages?q=".to_owned(),
        "/search/messages?q=%20%20".to_owned(),
        format!("/search/messages?q={long}"),
        "/search/messages?q=tide&limit=0".to_owned(),
        "/search/messages?q=tide&limit=51".to_owned(),
        "/search/messages?q=tide&cursor=nope".to_owned(),
        "/search/messages?q=tide&sort=relevance".to_owned(),
    ] {
        let response = get(&router, &bearer, &uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    for words in ["*", "\"", "()", "- :"] {
        let page = search(&router, &bearer, words).await;
        assert!(page.hits.is_empty(), "{words}");
        assert_eq!(page.next_cursor, None);
    }
}
