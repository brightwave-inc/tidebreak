//! Cross-principal conformance over the self-host route surface (#853).
//!
//! Two named users share one store, and everything one of them owns —
//! chats, transcripts, projects, documents, standing grants, the WebSocket
//! event stream — must be invisible and immutable to the other:
//! indistinguishable from data that does not exist. The suite drives the
//! real router with the self-host profile's named bearer tokens, so it
//! covers the token-to-principal resolution, the `ScopedStore` seam, and
//! the owner-scoped queries as one path.

use super::*;

use std::net::SocketAddr;

use openwave_core::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const ALICE_TOKEN: &str = "alice-token-0123456789abcdef";
const BOB_TOKEN: &str = "bob-token-9876543210fedcba";

/// A self-host app over one shared store, authenticating the two named
/// principals above (and nothing else).
async fn self_host_app() -> (Router, AppState, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("conformance.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN}\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let state = AppState::new(
        config,
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    spawn_turn_worker(&state);
    (app(state.clone()), state, store, dir)
}

async fn request(
    router: &Router,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, bearer);
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

async fn ingest_document(router: &Router, bearer: &str, uri: &str) -> String {
    let accepted: serde_json::Value = json_body(
        post_raw(
            router,
            bearer,
            uri,
            Some("text/plain"),
            b"the text".to_vec(),
        )
        .await,
    )
    .await;
    accepted["document_id"].as_str().unwrap().to_owned()
}

/// The REST matrix: everything B can ask about A's data answers as if the
/// data did not exist, and none of B's rejected mutations leave a mark.
#[tokio::test]
async fn cross_principal_rest_surface_is_disjoint() {
    let (router, state, store, _dir) = self_host_app().await;
    let alice = format!("Bearer {ALICE_TOKEN}");
    let bob = format!("Bearer {BOB_TOKEN}");

    // On the shared profile only the named tokens authenticate: the
    // per-launch capability token names nobody, so it admits nobody.
    for token in [format!("Bearer {}", state.token), "Bearer bogus".into()] {
        let response = request(&router, "GET", "/chats", &token, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token}");
    }

    // Alice's world: a project, a chat with a finished turn, and documents
    // at both the standalone and chat scope.
    let project: Project = json_body(
        request(
            &router,
            "POST",
            "/projects",
            &alice,
            Some(serde_json::json!({"title": "Filings"})),
        )
        .await,
    )
    .await;
    let chat = make_chat(&router, &alice).await;
    assert_eq!(
        send_message(&router, &alice, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    let document_id = ingest_document(&router, &alice, "/documents/raw?title=a.txt").await;
    let chat_document_id = ingest_document(
        &router,
        &alice,
        &format!("/chats/{}/documents/raw?title=b.txt", chat.id),
    )
    .await;

    // Positive control: the owner sees all of it.
    let chats: Vec<Chat> = json_body(request(&router, "GET", "/chats", &alice, None).await).await;
    assert_eq!(chats.len(), 1);
    let transcript: serde_json::Value = json_body(
        request(
            &router,
            "GET",
            &format!("/chats/{}/messages", chat.id),
            &alice,
            None,
        )
        .await,
    )
    .await;
    assert!(!transcript["messages"].as_array().unwrap().is_empty());

    // Bob's lists are empty — not filtered views that leak counts, but the
    // same responses an empty account gets.
    for uri in ["/chats", "/projects"] {
        let listed: Vec<serde_json::Value> =
            json_body(request(&router, "GET", uri, &bob, None).await).await;
        assert_eq!(listed, Vec::<serde_json::Value>::new(), "{uri}");
    }
    let documents: serde_json::Value =
        json_body(request(&router, "GET", "/documents", &bob, None).await).await;
    assert_eq!(documents["documents"].as_array().unwrap().len(), 0);

    // Every direct route over Alice's ids answers 404 for Bob: reads,
    // transcript, turn mutations, and the document surface alike.
    let patch_body = serde_json::json!({"title": "stolen"});
    let message_body = serde_json::json!({"turn_id": TurnId::new(), "content": "mine now"});
    let matrix: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        ("GET", format!("/chats/{}", chat.id), None),
        (
            "PATCH",
            format!("/chats/{}", chat.id),
            Some(patch_body.clone()),
        ),
        ("DELETE", format!("/chats/{}", chat.id), None),
        ("GET", format!("/chats/{}/messages", chat.id), None),
        (
            "POST",
            format!("/chats/{}/messages", chat.id),
            Some(message_body),
        ),
        (
            "POST",
            format!("/chats/{}/cancel", chat.id),
            Some(serde_json::json!({"turn_id": TurnId::new()})),
        ),
        ("GET", format!("/chats/{}/approvals", chat.id), None),
        ("GET", format!("/chats/{}/agent-runs", chat.id), None),
        (
            "POST",
            format!("/chats/{}/documents/raw?title=c.txt", chat.id),
            None,
        ),
        (
            "GET",
            format!("/chats/{}/documents/{chat_document_id}", chat.id),
            None,
        ),
        ("GET", format!("/projects/{}", project.id), None),
        (
            "PATCH",
            format!("/projects/{}", project.id),
            Some(patch_body),
        ),
        ("DELETE", format!("/projects/{}", project.id), None),
        ("GET", format!("/documents/{document_id}"), None),
        (
            "GET",
            format!("/documents/{document_id}/file-content"),
            None,
        ),
    ];
    for (method, uri, body) in matrix {
        let response = request(&router, method, &uri, &bob, body).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} must not reveal another owner's data"
        );
    }

    // Standalone document deletion is idempotent (deleting an absent id also
    // answers 202), so the line to hold is subtler: Bob's delete of Alice's
    // document must behave exactly like deleting nothing.
    let response = request(
        &router,
        "DELETE",
        &format!("/documents/{document_id}"),
        &bob,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // Alice's world is untouched by any of it.
    let response = request(&router, "GET", &format!("/chats/{}", chat.id), &alice, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let chat_after: Chat = json_body(response).await;
    assert_eq!(chat_after.title, chat.title);
    let response = request(
        &router,
        "GET",
        &format!("/documents/{document_id}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Standing grants belong to the owner of the chat or project they cover:
/// invisible in the other principal's list and immune to their revocation.
#[tokio::test]
async fn standing_grants_are_owner_scoped_across_the_grant_surface() {
    let (router, state, store, _dir) = self_host_app().await;
    let alice = format!("Bearer {ALICE_TOKEN}");
    let bob = format!("Bearer {BOB_TOKEN}");
    let chat = make_chat(&router, &alice).await;

    // Park a Sensitive call in Alice's chat and approve it with a standing
    // grant, through the same durable approval path the route uses.
    let call_id = CallId::new();
    let approval = ApprovalRequest {
        auto_judge: false,
        call_id,
        chat_id: chat.id,
        turn_id: TurnId::new(),
        tool_name: "search".into(),
        class: ApprovalClass::Sensitive,
        kind: openwave_core::ToolApprovalKind::for_tool_name("search"),
        preview: None,
    };
    assert!(matches!(
        store
            .accept_tool_call(&ToolCallRecord {
                id: call_id,
                chat_id: chat.id,
                turn_id: approval.turn_id,
                provider_id: format!("provider-{call_id}"),
                name: "search".into(),
                arguments: serde_json::json!({ "query": "quarterly filings" }),
                raw_arguments: None,
                execution: ToolCallExecution::Server,
                status: ToolCallStatus::Pending,
                result: None,
                result_preview: None,
                error_code: None,
                error_detail: None,
                client_executor_id: None,
                client_lease_expires_at: None,
                created_at: chrono::Utc::now(),
                resolved_at: None,
            })
            .await
            .unwrap(),
        openwave_core::AcceptToolCallOutcome::Accepted(_)
    ));
    let pending = state.approvals.register(approval, None).await;
    drop(pending.decision);
    assert_eq!(
        state
            .approvals
            .resolve_with_grant(
                chat.id,
                call_id,
                ApprovalDecision::Approve,
                Some(crate::routes::ApprovalGrantRung::WholeTool),
            )
            .await
            .unwrap(),
        crate::approvals::ResolveApprovalOutcome::Resolved
    );

    // The owner finds the grant on both consent surfaces; Bob finds nothing.
    let grants: Vec<serde_json::Value> =
        json_body(request(&router, "GET", "/grants", &alice, None).await).await;
    assert_eq!(grants.len(), 1);
    for uri in ["/grants", "/consent/statements"] {
        let listed: Vec<serde_json::Value> =
            json_body(request(&router, "GET", uri, &bob, None).await).await;
        assert_eq!(listed, Vec::<serde_json::Value>::new(), "{uri}");
    }

    // Bob cannot withdraw it — and cannot learn it exists by trying.
    let response = request(&router, "DELETE", &format!("/grants/{call_id}"), &bob, None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(store.list_standing_tool_grants().await.unwrap().len(), 1);

    // The owner can.
    let response = request(
        &router,
        "DELETE",
        &format!("/grants/{call_id}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(store.list_standing_tool_grants().await.unwrap().is_empty());
}

/// The WebSocket event surface holds the same line: no upgrade onto another
/// owner's chat, and no event delivery across owners on your own socket.
#[tokio::test(flavor = "multi_thread")]
async fn ws_event_surface_is_owner_gated() {
    let (router, _state, store, _dir) = self_host_app().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router.clone()).await;
    });

    let client = reqwest::Client::new();
    let alice_chat: Chat = client
        .post(format!("http://{addr}/chats"))
        .bearer_auth(ALICE_TOKEN)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_chat: Chat = client
        .post(format!("http://{addr}/chats"))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Bob cannot upgrade onto Alice's chat: the handshake is refused before
    // any socket exists, indistinguishable from a chat that isn't there.
    let mut upgrade = format!("ws://{addr}/chats/{}/events", alice_chat.id)
        .into_client_request()
        .unwrap();
    upgrade.headers_mut().insert(
        "Authorization",
        format!("Bearer {BOB_TOKEN}").parse().unwrap(),
    );
    match connect_async(upgrade).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 404)
        }
        other => panic!("expected a refused handshake, got {other:?}"),
    }

    // Bob's socket on his own chat stays silent while Alice's turn runs.
    let mut bob_request = format!("ws://{addr}/chats/{}/events", bob_chat.id)
        .into_client_request()
        .unwrap();
    bob_request.headers_mut().insert(
        "Authorization",
        format!("Bearer {BOB_TOKEN}").parse().unwrap(),
    );
    let (mut bob_socket, _) = connect_async(bob_request).await.unwrap();

    let response = client
        .post(format!("http://{addr}/chats/{}/messages", alice_chat.id))
        .bearer_auth(ALICE_TOKEN)
        .json(&serde_json::json!({"turn_id": TurnId::new(), "content": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let events = wait_for_turn(&store, alice_chat.id).await;
    assert!(!events.is_empty(), "alice's turn journaled its events");

    // Alice's completed turn delivered nothing to Bob's live stream.
    let leaked = tokio::time::timeout(Duration::from_millis(300), bob_socket.next()).await;
    assert!(
        leaked.is_err(),
        "bob's socket must not observe alice's events: {leaked:?}"
    );
}

/// The boot side of the contract: a self-host store cannot open without a
/// principal-naming authenticator, on any path that opens the store.
#[tokio::test]
async fn self_host_boot_fails_closed_without_a_principal_naming_authenticator() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = Profile::SelfHost;

    // No authenticator configured: refused before any store exists.
    let refusal = match crate::connect_store(&config).await {
        Err(refusal) => refusal.to_string(),
        Ok(_) => panic!("a self-host store must not open without an authenticator"),
    };
    assert!(
        refusal.contains("OPENWAVE_AUTH_TOKENS_FILE"),
        "the refusal names the missing authenticator: {refusal}"
    );

    // An authenticator that names nobody is refused the same way.
    let tokens_file = dir.path().join("tokens");
    std::fs::write(&tokens_file, "# nobody\n").unwrap();
    config.auth_tokens_file = Some(tokens_file);
    let refusal = match crate::connect_store(&config).await {
        Err(refusal) => refusal.to_string(),
        Ok(_) => panic!("a self-host store must not open behind an empty authenticator"),
    };
    assert!(
        refusal.contains("names no principals"),
        "an empty authenticator must refuse boot: {refusal}"
    );
}
