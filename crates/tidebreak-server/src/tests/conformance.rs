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

use tidebreak_core::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// A self-host app over one shared store, authenticating the two named
/// principals above (and nothing else). Alice administers the deployment;
/// Bob is an ordinary member, which is also what makes this fixture a valid
/// token file — one that names no admin refuses to load.
async fn self_host_app() -> (Router, AppState, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("conformance.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
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

#[tokio::test]
async fn sticky_chat_defaults_are_isolated_between_self_host_users() {
    let (router, _state, _store, _dir) = self_host_app().await;
    let alice = format!("Bearer {ALICE_TOKEN}");
    let bob = format!("Bearer {BOB_TOKEN}");

    let alice_chat = make_chat(&router, &alice).await;
    let patched = patch_chat(
        &router,
        &alice,
        alice_chat.id,
        serde_json::json!({
            "permission_mode": "allow",
            "network_policy": {"mode": "off"},
        }),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);

    let bob_chat = make_chat(&router, &bob).await;
    assert_eq!(bob_chat.permission_mode, None);
    assert_eq!(
        bob_chat.network_policy,
        tidebreak_core::NetworkPolicy::default()
    );

    let bob_explicit: Chat = json_body(
        request(
            &router,
            "POST",
            "/chats",
            &bob,
            Some(serde_json::json!({
                "permission_mode": "plan",
                "network_policy": {"mode": "package_managers"},
            })),
        )
        .await,
    )
    .await;
    assert_eq!(
        bob_explicit.permission_mode,
        Some(tidebreak_core::PermissionMode::Plan)
    );

    let alice_next = make_chat(&router, &alice).await;
    assert_eq!(
        alice_next.permission_mode,
        Some(tidebreak_core::PermissionMode::Allow)
    );
    assert_eq!(
        alice_next.network_policy,
        tidebreak_core::NetworkPolicy::Off
    );

    let alice_settings: serde_json::Value =
        json_body(request(&router, "GET", "/settings", &alice, None).await).await;
    let bob_settings: serde_json::Value =
        json_body(request(&router, "GET", "/settings", &bob, None).await).await;
    assert_eq!(alice_settings["chat_defaults"]["permission_mode"], "allow");
    assert_eq!(
        alice_settings["chat_defaults"]["network_policy"]["mode"],
        "off"
    );
    assert_eq!(bob_settings["chat_defaults"]["permission_mode"], "plan");
    assert_eq!(
        bob_settings["chat_defaults"]["network_policy"]["mode"],
        "package_managers"
    );
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
        kind: tidebreak_core::ToolApprovalKind::for_tool_name("search"),
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

                provider_replay: None,
                error_code: None,
                error_detail: None,
                client_executor_id: None,
                client_lease_expires_at: None,
                created_at: chrono::Utc::now(),
                resolved_at: None,
            })
            .await
            .unwrap(),
        tidebreak_core::AcceptToolCallOutcome::Accepted(_)
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
        refusal.contains("TIDEBREAK_AUTH_TOKENS_FILE"),
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

    // Nor can it open behind an authenticator nobody can configure through.
    std::fs::write(
        config.auth_tokens_file.as_deref().unwrap(),
        format!("alice {ALICE_TOKEN}\n"),
    )
    .unwrap();
    let refusal = match crate::connect_store(&config).await {
        Err(refusal) => refusal.to_string(),
        Ok(_) => panic!("a self-host store must not open with no administrator"),
    };
    assert!(
        refusal.contains("names no administrator"),
        "a deployment nobody can configure must refuse boot: {refusal}"
    );
}

/// The deployment plane, enumerated. This list is the canonical statement of
/// which routes reconfigure the deployment or touch its shared secrets: a
/// member is refused every one of them, and an admin passes the gate on every
/// one of them.
///
/// Enumerating the assembled router is the point. A gate that matched path
/// prefixes in middleware would survive spot checks on the routes someone
/// remembered, and silently exempt the next config route whose path does not
/// fit the pattern.
fn deployment_plane_routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("PUT", "/settings"),
        ("PUT", "/settings/api-key"),
        ("DELETE", "/settings/api-key"),
        ("PUT", "/models/roles/default"),
        ("PUT", "/web-search"),
        ("GET", "/web-search/credentials"),
        ("PUT", "/web-search/credentials/brave"),
        ("DELETE", "/web-search/credentials/brave"),
        ("PUT", "/code-execution"),
        ("GET", "/code-execution/credentials"),
        ("PUT", "/code-execution/credentials/e2b"),
        ("DELETE", "/code-execution/credentials/e2b"),
        ("PUT", "/mcp/servers"),
        ("POST", "/mcp/servers/example/reconnect"),
        ("POST", "/plugins/install"),
        ("PUT", "/plugins/enabled"),
        ("PUT", "/connected-apps/rest/example"),
        ("DELETE", "/connected-apps/rest/example"),
        ("POST", "/connected-apps/rest/spec-preview"),
        ("POST", "/connected-apps/rest/spec-discovery"),
        ("POST", "/gateway/sign-in"),
        ("POST", "/gateway/sign-out"),
        ("POST", "/gateway/models/sync"),
        ("PUT", "/providers/anthropic"),
        ("DELETE", "/providers/anthropic/credential"),
        ("POST", "/providers/openai/chatgpt/sign-in"),
        ("POST", "/providers/openai/chatgpt/sign-out"),
        ("PUT", "/voice-transcription"),
        ("POST", "/voice-transcription/install"),
        ("GET", "/diagnostics/snapshot"),
        ("GET", "/diagnostics/metrics"),
        ("GET", "/diagnostics/export"),
    ]
}

/// A representative slice of the member plane: reads that only reveal what the
/// deployment can do, plus the owner-scoped data surface. None of these may
/// become a `403` when the role gate lands.
fn member_plane_routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/settings"),
        ("GET", "/models"),
        ("GET", "/web-search"),
        ("GET", "/code-execution"),
        ("GET", "/mcp/servers"),
        ("GET", "/plugins"),
        ("GET", "/connected-apps"),
        ("GET", "/apps"),
        ("GET", "/policy"),
        ("GET", "/gateway/status"),
        ("GET", "/gateway/apps"),
        ("GET", "/gateway/machine"),
        ("GET", "/providers"),
        ("GET", "/providers/openai/chatgpt/status"),
        ("GET", "/voice-transcription"),
        ("GET", "/chats"),
        ("GET", "/projects"),
        ("GET", "/documents"),
        ("GET", "/inbox"),
        ("GET", "/notifications"),
        ("GET", "/notifications/unread-count"),
        ("POST", "/notifications/read"),
        ("POST", "/notifications/read-all"),
        ("GET", "/grants"),
        ("GET", "/consent/statements"),
    ]
}

/// The plane split, driven across the real router with a real admin and a real
/// member. `403` is the only status under test: what a deployment-plane
/// handler does once the gate lets it through is its own concern, so any other
/// status counts as having passed.
#[tokio::test]
async fn the_deployment_plane_admits_admins_and_refuses_members() {
    let (router, _state, _store, _dir) = self_host_app().await;
    let admin = format!("Bearer {ALICE_TOKEN}");
    let member = format!("Bearer {BOB_TOKEN}");
    let body = Some(serde_json::json!({}));

    for (method, uri) in deployment_plane_routes() {
        let response = request(&router, method, uri, &member, body.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} reconfigures the deployment and must refuse a member"
        );
        let status = request(&router, method, uri, &admin, body.clone())
            .await
            .status();
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{method} {uri} must let an admin past the gate, got {status}"
        );
    }

    for (method, uri) in member_plane_routes() {
        let status = request(&router, method, uri, &member, None).await.status();
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{method} {uri} reveals only capability or the caller's own data, \
             but answered {status} to a member"
        );
    }
}

/// The plausible wrong implementation: a role gate that defaults an identity
/// when none was attached would pass every authenticated test above. It must
/// fail closed instead, exactly like the `AuthContext` extractor.
#[tokio::test]
async fn the_role_gate_rejects_a_route_no_auth_middleware_covers() {
    let app = Router::new()
        .route("/settings", axum::routing::put(|| async { "leaked" }))
        .route_layer(axum::middleware::from_fn(crate::auth::require_admin));

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A desktop server must never open a routable socket. The refusal runs on the
/// real boot path, before the instance lock or the store, so a misconfigured
/// launch fails immediately and leaves nothing behind — and it is a refusal
/// rather than a silent fallback, because an operator who set the variable
/// would otherwise believe they had published a port.
#[tokio::test]
async fn desktop_boot_refuses_a_configured_listen_address() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::desktop(dir.path());
    config.listen_addr = Some("0.0.0.0:8080".parse().unwrap());

    let refusal = match crate::bind(config).await {
        Err(refusal) => refusal.to_string(),
        Ok(_) => panic!("the desktop profile must not bind a routable address"),
    };
    assert!(
        refusal.contains("loopback-only") && refusal.contains("self_host"),
        "the refusal must name the remedy: {refusal}"
    );
    assert!(
        !dir.path().join("tidebreak.lock").exists(),
        "the refusal must come before the boot takes the instance lock"
    );
}

/// The container-entrypoint contract: a self-host deployment binds the address
/// it was given, and the liveness probe the entrypoint waits on answers there
/// without a credential — it sits outside both auth layers, so a packaging
/// healthcheck never needs a token it has no way to hold.
///
/// The address is taken from a configured `Config` rather than the process
/// environment, which a parallel test binary cannot mutate safely, and bound
/// on port 0 so the test cannot collide with anything. What it pins is that a
/// self-host config's `bind_addr` is what the socket ends up on; that
/// `bind_inner` is the caller of `bind_addr` is pinned by the desktop refusal
/// above, which travels the real boot path.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_host_deployment_serves_liveness_on_its_configured_address() {
    let (router, state, _store, _dir) = self_host_app().await;
    let mut config = (*state.config).clone();
    assert_eq!(config.profile, Profile::SelfHost);
    config.listen_addr = Some("127.0.0.1:0".parse().unwrap());

    let listener = TcpListener::bind(config.bind_addr().unwrap())
        .await
        .unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    assert_eq!(
        addr.ip(),
        "127.0.0.1".parse::<std::net::IpAddr>().unwrap(),
        "the configured interface is the one bound"
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let response = reqwest::get(format!("http://{addr}/healthz"))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
}
