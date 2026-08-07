use super::*;

use std::net::SocketAddr;

use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::event_projection::{RendererAgentEvent, RendererSequencedEvent};

struct SensitiveEventProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SensitiveEventProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("provider-secret-id")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "provider-secret-call-id".into(),
                    name: "provider_secret_tool".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"/Users/private/file.txt","secret":"hunter2"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "safe assistant response".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Serve a router (with the given provider) over a real loopback socket.
async fn serve_app_with(
    provider: Arc<dyn ModelProvider>,
) -> (SocketAddr, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (router, token, store, dir) = test_app_with(provider).await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, token, store, dir)
}

async fn make_chat_http(client: &reqwest::Client, addr: SocketAddr, token: &str) -> Chat {
    client
        .post(format!("http://{addr}/chats"))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn send_message_http(client: &reqwest::Client, addr: SocketAddr, token: &str, chat: ChatId) {
    let response = client
        .post(format!("http://{addr}/chats/{chat}/messages"))
        .bearer_auth(token)
        .json(&serde_json::json!({"turn_id": TurnId::new(), "content": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

/// Connect to a chat's event socket (authenticated) and read frames until
/// `want` turns have ended (or a timeout), returning the decoded events in
/// arrival order.
async fn read_until_turns_end(
    addr: SocketAddr,
    token: &str,
    chat: ChatId,
    after: i64,
    want: usize,
) -> Vec<RendererSequencedEvent> {
    let mut request = format!("ws://{addr}/chats/{chat}/events?after={after}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _response) = connect_async(request).await.unwrap();

    let mut events = Vec::new();
    let mut completed = 0;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: RendererSequencedEvent = serde_json::from_str(text.as_str()).unwrap();
            if matches!(
                event.event,
                RendererAgentEvent::TurnCompleted { .. } | RendererAgentEvent::TurnFailed { .. }
            ) {
                completed += 1;
            }
            events.push(event);
            if completed >= want {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turns did not complete over the socket");
    events
}

/// Read one turn's worth of events over a fresh connection.
async fn read_until_turn_end(
    addr: SocketAddr,
    token: &str,
    chat: ChatId,
    after: i64,
) -> Vec<RendererSequencedEvent> {
    read_until_turns_end(addr, token, chat, after, 1).await
}

fn decode_ws_event(message: WsMessage) -> RendererSequencedEvent {
    let WsMessage::Text(text) = message else {
        panic!("expected a JSON text event frame");
    };
    serde_json::from_str(text.as_str()).unwrap()
}

async fn read_raw_until_terminal<S>(socket: &mut S) -> Vec<serde_json::Value>
where
    S: futures::Stream<
            Item = std::result::Result<WsMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let mut events = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            let terminal = matches!(
                event["event"]["type"].as_str(),
                Some("turn_completed" | "turn_failed" | "turn_cancelled")
            );
            events.push(event);
            if terminal {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not terminate over the event socket");
    events
}

fn assert_renderer_event_frames_are_redacted(events: &[serde_json::Value]) {
    let serialized = serde_json::to_string(events).unwrap();
    // `usage` is deliberately absent from this list: the terminal events carry
    // the turn's four token counts so the desktop can show context usage.
    // `stop_reason`, which sits beside it in the journal, still does not cross.
    //
    // Internal *keys* are written with their quotes and colon rather than bare.
    // A bare `output` also matches the `output_tokens` count that now crosses
    // legitimately, and a check that cannot distinguish the two is not a check.
    for forbidden in [
        "provider-secret-id",
        "provider-secret-call-id",
        "provider_secret_tool",
        "/Users/private",
        "file.txt",
        "hunter2",
        "\"fragment\":",
        "\"output\":",
        "\"content\":",
        "\"data\":",
        "\"summary\":",
        "\"stop_reason\":",
        "\"diagnostic\":",
        "\"lease\":",
        "\"checkpoint\":",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "event stream leaked {forbidden}"
        );
    }
    assert!(events.iter().any(|event| event["event"]["name"] == "other"));
    assert!(events.iter().any(|event| {
        event["event"]["type"] == "tool_call_completed" && event["event"]["status"] == "failed"
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_live_and_replay_frames_use_the_renderer_safe_projection() {
    let provider = Arc::new(SensitiveEventProvider {
        calls: AtomicUsize::new(0),
    });
    let (addr, token, store, _dir) = serve_app_with(provider).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    let mut request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut live_socket, _) = connect_async(request).await.unwrap();

    send_message_http(&client, addr, &token, chat.id).await;
    let live = read_raw_until_terminal(&mut live_socket).await;
    assert_renderer_event_frames_are_redacted(&live);
    assert!(
        live.iter().all(|event| event.get("replayed").is_none()),
        "live frames retain their established wire shape"
    );

    wait_for_turn(&store, chat.id).await;
    let mut replay_request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    replay_request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut replay_socket, _) = connect_async(replay_request).await.unwrap();
    let replay = read_raw_until_terminal(&mut replay_socket).await;
    assert_renderer_event_frames_are_redacted(&replay);
    assert!(
        replay
            .iter()
            .all(|event| event.get("replayed") == Some(&serde_json::Value::Bool(true))),
        "durable catch-up frames identify themselves to the renderer"
    );

    let live_sequences = live
        .iter()
        .map(|event| event["seq"].clone())
        .collect::<Vec<_>>();
    let replay_sequences = replay
        .iter()
        .map(|event| event["seq"].clone())
        .collect::<Vec<_>>();
    assert_eq!(replay_sequences, live_sequences);
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_a_journal_gap_before_accepting_a_later_live_event() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    let token = state.token.clone();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    let first = AgentEvent::TextDelta { text: "one".into() };
    assert_eq!(store.append_event(chat.id, &first).await.unwrap(), 1);
    let mut request = format!("ws://{addr}/chats/{}/events", chat.id)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let replayed_first = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("initial journal replay timed out")
        .expect("event socket closed")
        .unwrap();
    assert_eq!(decode_ws_event(replayed_first).seq, 1);

    let second = AgentEvent::TextDelta { text: "two".into() };
    let third = AgentEvent::TextDelta {
        text: "three".into(),
    };
    assert_eq!(store.append_event(chat.id, &second).await.unwrap(), 2);
    assert_eq!(store.append_event(chat.id, &third).await.unwrap(), 3);
    let _ = state.events.sender(chat.id).send(SequencedEvent {
        seq: 3,
        event: third,
    });
    let _ = state.events.sender(chat.id).send(SequencedEvent {
        seq: 2,
        event: second.clone(),
    });

    let mut recovered = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("gap recovery timed out")
            .expect("event socket closed")
            .unwrap();
        recovered.push(decode_ws_event(frame));
    }
    assert_eq!(
        recovered.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(
        recovered[0].event,
        RendererAgentEvent::TextDelta { text: "two".into() }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err(),
        "the late live seq 2 must be deduplicated after journal replay"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_a_finished_turn_from_the_journal() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    // Run the turn to completion, then connect — everything comes from replay.
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    let events = read_until_turn_end(addr, &token, chat.id, 0).await;
    assert_eq!(events.first().unwrap().seq, 1, "replay starts at seq 1");
    assert!(matches!(
        events[0].event,
        RendererAgentEvent::TurnStarted { .. }
    ));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, RendererAgentEvent::TextDelta { text } if text == "hi")));
    assert!(matches!(
        events.last().unwrap().event,
        RendererAgentEvent::TurnCompleted { .. }
    ));
    // Sequence numbers are strictly increasing.
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_after_cursor_replays_only_newer_events() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Resume after seq 1: the first replayed event must be seq 2, and seq 1 is
    // not re-sent.
    let events = read_until_turn_end(addr, &token, chat.id, 1).await;
    assert_eq!(events.first().unwrap().seq, 2);
    assert!(events.iter().all(|e| e.seq > 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_one_turn_then_streams_the_next_live() {
    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;

    // Turn 1 runs to completion and is journaled.
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Connect (replays turn 1) and keep reading; then run turn 2, whose events
    // arrive live on the same connection. Assert both turns come through in
    // one gap-free, duplicate-free, strictly-increasing stream.
    let reader = {
        let token = token.clone();
        tokio::spawn(async move { read_until_turns_end(addr, &token, chat.id, 0, 2).await })
    };
    // Let the reader connect, subscribe, and drain the replay before turn 2.
    tokio::time::sleep(Duration::from_millis(100)).await;
    send_message_http(&client, addr, &token, chat.id).await;

    let events = reader.await.unwrap();
    assert!(matches!(
        events[0].event,
        RendererAgentEvent::TurnStarted { .. }
    ));
    assert_eq!(events[0].seq, 1);
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.event, RendererAgentEvent::TurnCompleted { .. }))
            .count(),
        2,
        "both turns completed over one connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_bad_after_cursor_is_a_json_400() {
    let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    // A non-integer `after` fails extraction; it must answer the API-wide
    // `{ kind, message }` JSON, not axum's plain-text rejection.
    let response = client
        .get(format!("http://{addr}/chats/{}/events?after=abc", chat.id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = response.json().await.unwrap();
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_without_a_token_is_rejected() {
    let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let chat = ChatId::new();
    let request = format!("ws://{addr}/chats/{chat}/events")
        .into_client_request()
        .unwrap();
    // No Authorization header: the handshake must fail (auth runs before upgrade).
    assert!(connect_async(request).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_subprotocol_auth_succeeds() {
    use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

    let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let client = reqwest::Client::new();
    let chat = make_chat_http(&client, addr, &token).await;
    send_message_http(&client, addr, &token, chat.id).await;
    wait_for_turn(&store, chat.id).await;

    // Authenticate with Sec-WebSocket-Protocol only — no Authorization header.
    let mut request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}{token}")
            .parse()
            .unwrap(),
    );
    let (mut socket, response) = connect_async(request).await.unwrap();
    // Server must select the handshake subprotocol.
    let selected = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok());
    assert_eq!(selected, Some(WS_HANDSHAKE_SUBPROTOCOL));

    let mut saw_completed = false;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let event: RendererSequencedEvent = serde_json::from_str(text.as_str()).unwrap();
            if matches!(event.event, RendererAgentEvent::TurnCompleted { .. }) {
                saw_completed = true;
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over subprotocol-authed socket");
    assert!(saw_completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_subprotocol_wrong_token_is_rejected() {
    use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

    let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
    let chat = ChatId::new();
    let mut request = format!("ws://{addr}/chats/{chat}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}not-the-token")
            .parse()
            .unwrap(),
    );
    assert!(connect_async(request).await.is_err());
}
