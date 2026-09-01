//! The in-process engine behind the adapter: a session with no workspace.
//!
//! Decision 0048 step 5's tripwire. A conversation with no workspace runs on
//! the internal engine through exactly the routes, journal, and worker an
//! external engine uses — and the conversation the engine keeps for itself
//! never surfaces as a chat of the owner's.

use super::code::*;
use super::*;

use std::sync::Arc;

use crate::code::CodeRuntime;
use crate::engine::internal::InternalAdapter;
use tidebreak_core::{CodeEvent, Store};
use tidebreak_harness::AdapterRegistry;

/// A code app whose registry holds the in-process engine and whose chat turn
/// lane runs, so an internal-engine turn executes end to end against the
/// fake provider.
async fn internal_engine_app() -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut runtime =
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), AdapterRegistry::new());
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store_trait,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    // Registered the way `bind` does it: after the state exists, with a
    // copy that carries no code runtime.
    runtime
        .adapters
        .register(Arc::new(InternalAdapter::new(state.clone())));
    let runtime = Arc::new(runtime);
    state.code = Some(runtime.clone());
    spawn_turn_worker(&state);
    let token = state.token.clone();
    (app(state), token, runtime, dir)
}

#[tokio::test]
async fn a_session_without_a_workspace_runs_on_the_internal_engine() {
    let (router, token, runtime, _dir) = internal_engine_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let session = client
        .post(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "allow" }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();
    assert_eq!(session["harness_kind"], "internal");
    assert!(session["workspace_id"].is_null(), "{session}");
    assert_eq!(session["permission_mode"], "allow");

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = turn.json().await.unwrap();
    assert_eq!(turn["status"], "completed", "{turn}");

    // The chat lane's answer reached the code journal through the adapter.
    let events = journaled_events(&runtime.db, json_id(&session).parse().unwrap()).await;
    assert!(
        events.iter().any(|framed| matches!(
            &framed.event,
            CodeEvent::AssistantMessage { text, .. } if text == "hi"
        )),
        "{events:?}"
    );

    // The engine's own conversation is not one of the owner's chats.
    let chats: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/chats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(chats.is_empty(), "{chats:?}");
    let private = client
        .get(format!("http://{addr}/chats/{}", json_id(&session)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(private.status(), reqwest::StatusCode::NOT_FOUND);

    // The session row settles like any other, with no workspace to link.
    let stored = runtime
        .get_session(
            &tidebreak_core::OwnerId::local(),
            json_id(&session).parse().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stored.lifecycle, tidebreak_core::CodeSessionLifecycle::Idle);
    assert!(stored.workspace_id.is_none());
}

#[tokio::test]
async fn the_internal_engine_is_not_a_workspace_engine() {
    let (router, token, _runtime, dir) = internal_engine_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "internal",
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "harness_unavailable");

    // The doctor lists the engines a workspace can pick.
    let doctor: serde_json::Value = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!doctor.to_string().contains("\"internal\""), "{doctor}");
}
