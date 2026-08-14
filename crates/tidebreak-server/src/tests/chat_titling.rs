use super::*;

use std::collections::VecDeque;

use tidebreak_core::UtilityModel;

use crate::chat_titling::ChatTitler;

/// What the conversation's own turn answers with. Distinctive, so a titling call
/// that read assistant content would be caught by its own request.
const ASSISTANT_ANSWER: &str = "Reconciled: the variance is a timing difference.";

/// The model an OpenAI-only install resolves the `utility` role to.
const UTILITY_MODEL: &str = "gpt-5.4-nano";

/// The model the conversation itself runs on, so the two are never confused.
const CHAT_MODEL: &str = "gpt-5.6-sol";

/// A stub OpenAI Responses endpoint recording everything asked of it.
///
/// One endpoint serves both calls a titled conversation makes, because that is
/// how the real thing works: the turn and the titling call reach the same
/// credentialed provider and differ only in what they ask for.
struct TitlingStub {
    requests: Mutex<Vec<serde_json::Value>>,
    title_answers: Mutex<VecDeque<String>>,
    fallback_title_answer: String,
    pause_next_title: AtomicBool,
    title_entered: Notify,
    release_title: Notify,
}

impl TitlingStub {
    /// Requests that carried an output constraint — the titling calls.
    fn titling_requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.get("text").is_some())
            .cloned()
            .collect()
    }
}

async fn answer_as_stub(
    axum::extract::State(stub): axum::extract::State<Arc<TitlingStub>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let is_title = request.get("text").is_some();
    if is_title && stub.pause_next_title.swap(false, Ordering::SeqCst) {
        stub.title_entered.notify_one();
        stub.release_title.notified().await;
    }
    let text = if is_title {
        stub.title_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| stub.fallback_title_answer.clone())
    } else {
        ASSISTANT_ANSWER.to_owned()
    };
    stub.requests.lock().unwrap().push(request);
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"type": "response.output_text.delta", "delta": text}),
        serde_json::json!({"type": "response.completed", "response": {}}),
    );
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

/// An app whose only credentialed provider is a stub OpenAI endpoint answering
/// `title_answer` to any constrained call.
///
/// The registry-enforcing resolver is the point: it is what makes the `utility`
/// role resolve to a real model, so the turn worker actually starts a titling
/// call instead of skipping the work.
async fn titling_app(
    title_answer: &str,
) -> (
    Router,
    String,
    Arc<dyn Store>,
    Arc<TitlingStub>,
    tempfile::TempDir,
) {
    titling_app_with_answers(&[title_answer]).await
}

/// The same app with a sequence of constrained answers, followed by the last
/// answer forever. This makes transient failures observable without changing
/// how the conversation's own turn is answered.
async fn titling_app_with_answers(
    title_answers: &[&str],
) -> (
    Router,
    String,
    Arc<dyn Store>,
    Arc<TitlingStub>,
    tempfile::TempDir,
) {
    let fallback_title_answer = title_answers
        .last()
        .expect("a titling stub needs at least one answer")
        .to_string();
    let stub = Arc::new(TitlingStub {
        requests: Mutex::new(Vec::new()),
        title_answers: Mutex::new(
            title_answers
                .iter()
                .map(|answer| answer.to_string())
                .collect(),
        ),
        fallback_title_answer,
        pause_next_title: AtomicBool::new(false),
        title_entered: Notify::new(),
        release_title: Notify::new(),
    });
    let endpoint = axum::Router::new()
        .route("/v1/responses", axum::routing::post(answer_as_stub))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, endpoint).await;
    });
    let provider_base_url = format!("http://{address}/v1");
    providers::allow_test_loopback_provider_base_url(&provider_base_url);

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("titling.db").display()
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
            model: format!("openai::{CHAT_MODEL}"),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    spawn_turn_worker(&state);
    (app(state), bearer, store, stub, dir)
}

/// The chat's title once the background titling call has stored one.
async fn wait_for_title(store: &Arc<dyn Store>, chat: ChatId) -> Option<String> {
    for _ in 0..300 {
        let title = store.get_chat(chat).await.unwrap().unwrap().title;
        if title.is_some() {
            return title;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// The titling requests the stub has seen, once at least `count` have arrived.
async fn wait_for_titling_requests(
    stub: &Arc<TitlingStub>,
    count: usize,
) -> Vec<serde_json::Value> {
    for _ in 0..300 {
        let requests = stub.titling_requests();
        if requests.len() >= count {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stub.titling_requests()
}

/// The whole feature over the wire: a turn on a chat nobody named leaves it named
/// after what the user asked for, on the cheap model, without the conversation
/// waiting for any of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_names_the_chat_it_runs_in() {
    let (router, bearer, store, stub, _dir) =
        titling_app(r#"{"title":"Q3 revenue reconciliation"}"#).await;
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(chat.title, None);

    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;
    let events = wait_for_turn(&store, chat.id).await;
    assert!(
        matches!(
            events.last().map(|event| &event.event),
            Some(AgentEvent::TurnCompleted { .. })
        ),
        "titling rides beside the turn and never decides its outcome",
    );
    assert_eq!(
        wait_for_title(&store, chat.id).await.as_deref(),
        Some("Q3 revenue reconciliation"),
    );

    let requests = stub.titling_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request["model"], UTILITY_MODEL,
        "background work runs on the utility role, not the conversation's model",
    );
    assert!(
        request.get("tools").is_none(),
        "titling advertises no tools; the model has nothing to do but answer",
    );
    assert_eq!(
        request["text"]["format"]["name"], "chat_title",
        "a provider that enforces the schema is what makes \"no title\" answerable",
    );
    let material = request["input"].to_string();
    assert!(material.contains("Reconcile Q3 revenue for me"));
    assert!(
        !material.contains(ASSISTANT_ANSWER),
        "the name describes what the user asked for, not what the model answered",
    );
}

/// The reason the payload's title is nullable. A permanent name is a bad trade
/// for an exchange that has not established what it is about.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_with_nothing_to_name_stays_untitled() {
    let (router, bearer, store, stub, _dir) = titling_app(r#"{"title":null}"#).await;
    let chat = make_chat(&router, &bearer).await;

    send_message(&router, &bearer, chat.id, "hi").await;
    wait_for_turn(&store, chat.id).await;
    assert_eq!(wait_for_titling_requests(&stub, 1).await.len(), 1);
    assert_eq!(
        store.get_chat(chat.id).await.unwrap().unwrap().title,
        None,
        "declining is a valid answer, and it leaves the chat named by nobody",
    );
}

/// A broken provider stream is background-maintenance noise, not a reason to
/// leave a real conversation unnamed when the next call would have worked.
#[tokio::test(flavor = "multi_thread")]
async fn titling_retries_transient_failures_before_giving_up() {
    let (router, bearer, store, stub, _dir) = titling_app_with_answers(&[
        "not json",
        r#"{"wrong":"shape"}"#,
        r#"{"title":"Q3 revenue reconciliation"}"#,
    ])
    .await;
    let chat = make_chat(&router, &bearer).await;

    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;

    assert_eq!(
        wait_for_title(&store, chat.id).await.as_deref(),
        Some("Q3 revenue reconciliation"),
    );
    assert_eq!(
        wait_for_titling_requests(&stub, 3).await.len(),
        3,
        "one background run gets the initial call plus two retries",
    );
}

/// Exhausting one run must release the in-flight claim. The next accepted user
/// message reaches the same front-of-turn hook and starts a fresh set of tries.
#[tokio::test(flavor = "multi_thread")]
async fn an_untitled_chat_tries_again_on_the_next_user_message() {
    let (router, bearer, store, stub, _dir) = titling_app("not json").await;
    let chat = make_chat(&router, &bearer).await;

    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;
    wait_for_turn(&store, chat.id).await;
    assert_eq!(wait_for_titling_requests(&stub, 3).await.len(), 3);
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().title, None);

    send_message(&router, &bearer, chat.id, "Use the ledger export too").await;
    assert_eq!(
        wait_for_titling_requests(&stub, 6).await.len(),
        6,
        "the next user submission starts another three-attempt run",
    );
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().title, None);
}

/// The next turn may start after the foreground answer finishes but before the
/// background titler does. Its trigger must queue behind the in-flight run,
/// rather than disappearing because the per-chat claim was already held.
#[tokio::test(flavor = "multi_thread")]
async fn a_next_message_queued_while_titling_runs_gets_its_own_attempt() {
    let (router, bearer, store, stub, _dir) = titling_app_with_answers(&[
        "not json",
        "not json",
        "not json",
        r#"{"title":"Q3 revenue reconciliation"}"#,
    ])
    .await;
    let chat = make_chat(&router, &bearer).await;
    stub.pause_next_title.store(true, Ordering::SeqCst);

    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;
    tokio::time::timeout(Duration::from_secs(5), stub.title_entered.notified())
        .await
        .expect("the first titling request did not start");
    wait_for_turn(&store, chat.id).await;

    send_message(&router, &bearer, chat.id, "Use the ledger export too").await;
    stub.release_title.notify_one();

    assert_eq!(
        wait_for_title(&store, chat.id).await.as_deref(),
        Some("Q3 revenue reconciliation"),
    );
    assert_eq!(
        wait_for_titling_requests(&stub, 4).await.len(),
        4,
        "the queued user turn runs after the first three-attempt batch fails",
    );
}

/// A name the user typed outranks a derived one however the two are ordered: the
/// turn does not start a call for a chat that has one, and the write itself
/// refuses to replace a name that lands mid-call.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_the_user_typed_is_never_replaced_by_a_derived_one() {
    let (router, bearer, store, stub, _dir) = titling_app(r#"{"title":"Derived name"}"#).await;
    let chat = make_chat(&router, &bearer).await;
    let renamed = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"title": "Ledger work"}),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);

    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;
    wait_for_turn(&store, chat.id).await;
    assert!(
        stub.titling_requests().is_empty(),
        "an already-named chat costs nothing to skip",
    );

    let applied = store
        .set_chat_title_if_unset(chat.id, "Derived name")
        .await
        .unwrap();
    assert!(
        !applied,
        "a rename that commits mid-call still wins, which the check above cannot cover",
    );
    assert_eq!(
        store.get_chat(chat.id).await.unwrap().unwrap().title,
        Some("Ledger work".into()),
    );
}

/// A titling call outlives the turn that started it, so the next turn can begin
/// while it is still running. Without the in-flight guard that second turn starts
/// a second call, and a chat that is never named pays for one on every message.
#[tokio::test]
async fn one_titling_call_runs_per_chat_at_a_time() {
    struct CountingProvider(AtomicUsize);

    #[async_trait]
    impl ModelProvider for CountingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("counting")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: r#"{"title":"Q3 revenue reconciliation"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let provider = Arc::new(CountingProvider(AtomicUsize::new(0)));
    let (router, token, store, _dir) = test_app_with(provider.clone()).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let titler = Arc::new(ChatTitler::new(
        store.clone(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(crate::bus::EventBus::default()),
    ));
    let utility = UtilityModel {
        provider: None,
        model: "utility-model".into(),
        reasoning_model: false,
        reasoning_effort: None,
    };
    send_message(&router, &bearer, chat.id, "Reconcile Q3 revenue for me").await;
    wait_for_turn(&store, chat.id).await;
    let turn_calls = provider.0.load(Ordering::SeqCst);

    titler.spawn(chat.id, utility.clone());
    titler.spawn(chat.id, utility);

    assert_eq!(
        wait_for_title(&store, chat.id).await.as_deref(),
        Some("Q3 revenue reconciliation"),
    );
    assert_eq!(provider.0.load(Ordering::SeqCst) - turn_calls, 1);
}

/// The name reaches an open client over the socket, on the frame shape the
/// renderer discriminates on, without disturbing the sequenced stream beside it.
///
/// This is the delivery half of the feature, and it is the half a client cannot
/// work around: a name written durably but never announced leaves every open
/// window showing "New chat" until it is reloaded.
#[tokio::test(flavor = "multi_thread")]
async fn the_derived_name_arrives_on_the_open_socket() {
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (router, bearer, _store, _stub, _dir) =
        titling_app(r#"{"title":"Q3 revenue reconciliation"}"#).await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let token = bearer.trim_start_matches("Bearer ").to_owned();
    let http = reqwest::Client::new();
    let chat: Chat = http
        .post(format!("http://{address}/chats"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let mut request = format!("ws://{address}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", bearer.parse().unwrap());
    let (mut socket, _response) = connect_async(request).await.unwrap();

    assert_eq!(
        http.post(format!("http://{address}/chats/{}/messages", chat.id))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "turn_id": TurnId::new(),
                "content": "Reconcile Q3 revenue for me",
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED,
    );

    let mut titled = None;
    let mut sequences = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let frame: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            if frame.get("metadata").is_some() {
                titled = Some(frame);
                break;
            }
            sequences.push(
                frame["seq"]
                    .as_i64()
                    .expect("an event frame carries its seq"),
            );
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("no title arrived on the socket");

    assert_eq!(
        titled.unwrap(),
        serde_json::json!({"metadata": "titled", "title": "Q3 revenue reconciliation"}),
        "the metadata frame is what the renderer discriminates on",
    );
    assert!(
        sequences.windows(2).all(|pair| pair[1] > pair[0]),
        "the metadata frame must not disturb the sequenced stream: {sequences:?}",
    );

    // Nothing retains a notice, and a new chat's first turn can name it before
    // the renderer finishes connecting. Opening a socket therefore has to state
    // the name the chat already has, or that client never learns it.
    let mut request = format!("ws://{address}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", bearer.parse().unwrap());
    let (mut reconnected, _response) = connect_async(request).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), reconnected.next())
        .await
        .expect("a reconnecting client heard nothing")
        .expect("the socket stayed open")
        .unwrap();
    let WsMessage::Text(text) = first else {
        panic!("the first frame is text");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text.as_str()).unwrap(),
        serde_json::json!({"metadata": "titled", "title": "Q3 revenue reconciliation"}),
    );
}
