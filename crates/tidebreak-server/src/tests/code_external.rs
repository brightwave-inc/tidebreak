//! The channel-adapter route surface end to end (docs/slack-sessions.md,
//! stage 2): adapter-token authentication, grant scoping, idempotent
//! messages, the snapshot-prefixed event stream, revocation severing it,
//! and token rotation with reuse detection.

use super::*;

use super::code::serve;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};

use axum::Router;
use futures::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::code::remote::service::RemoteSessions;
use crate::code::remote::wire::{
    EventCursor, MessageReceipt, SandboxEvent, SandboxEvents, SandboxLease, SandboxMessage,
    SandboxState, SandboxStatus, SpawnArguments,
};
use crate::code::remote::{RemoteSandboxError, SandboxProvisioner};
use crate::code::CodeRuntime;
use tidebreak_core::db::code::insert_repo;
use tidebreak_core::{CodeRepo, OwnerId, RepoId};

const ADAPTER_BOOTSTRAP_TOKEN: &str = "adapter-bootstrap-token-padded-to-forty-eight-characters";

#[derive(Default)]
struct FakeProvisioner {
    spawns: StdMutex<Vec<SpawnArguments>>,
    sends: StdMutex<Vec<SandboxMessage>>,
    event_reads: StdMutex<VecDeque<SandboxEvents>>,
}

#[async_trait::async_trait]
impl SandboxProvisioner for FakeProvisioner {
    async fn spawn(
        &self,
        _owner: &OwnerId,
        arguments: &SpawnArguments,
    ) -> Result<SandboxLease, RemoteSandboxError> {
        self.spawns.lock().unwrap().push(arguments.clone());
        Ok(SandboxLease {
            sandbox_id: "sb-ext".to_owned(),
            state: SandboxState::Pending,
            latest_event_seq: 0,
            expires_in_seconds: 7200,
        })
    }

    async fn status(
        &self,
        _owner: &OwnerId,
        sandbox_id: &str,
    ) -> Result<SandboxStatus, RemoteSandboxError> {
        Ok(SandboxStatus {
            sandbox_id: sandbox_id.to_owned(),
            state: SandboxState::Running,
            failure_reason: None,
            termination_reason: None,
            latest_event_seq: 0,
            pending_messages: 0,
            spend_microusd: None,
            spend_ceiling_microusd: None,
            possibly_stalled: false,
            repository_url: None,
            completed_at: None,
        })
    }

    async fn events(
        &self,
        _owner: &OwnerId,
        _sandbox_id: &str,
        _cursor: EventCursor,
    ) -> Result<SandboxEvents, RemoteSandboxError> {
        self.event_reads
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(RemoteSandboxError::Unavailable {
                operation: "events",
                detail: "no scripted read".to_owned(),
            })
    }

    async fn send(
        &self,
        _owner: &OwnerId,
        _sandbox_id: &str,
        message: &SandboxMessage,
    ) -> Result<MessageReceipt, RemoteSandboxError> {
        self.sends.lock().unwrap().push(message.clone());
        Ok(MessageReceipt {
            seq: 1,
            interrupt: message.interrupt,
            pending_messages: 0,
        })
    }

    async fn cancel(&self, _owner: &OwnerId, _sandbox_id: &str) -> Result<(), RemoteSandboxError> {
        Ok(())
    }
}

fn remote_settings() -> crate::code::remote::driver::RemoteSpawnSettings {
    crate::code::remote::driver::RemoteSpawnSettings {
        profile: "tidebreak-remote".to_owned(),
        incarnation_cap: 2,
        spend_ceiling_microusd: None,
        session_spend_ceiling_microusd: None,
    }
}

/// An app whose code runtime carries a fake sandbox provisioner and one
/// origin-bearing repository, ready for the adapter surface.
async fn external_app() -> (
    Router,
    Arc<FakeProvisioner>,
    Arc<CodeRuntime>,
    RepoId,
    tempfile::TempDir,
) {
    let (router, fake, runtime, repo_id, _token, dir) = external_app_with_token().await;
    (router, fake, runtime, repo_id, dir)
}

/// Like [`external_app`], also handing back the API bearer token for the
/// authenticated (person- and adapter-principal) routes.
async fn external_app_with_token() -> (
    Router,
    Arc<FakeProvisioner>,
    Arc<CodeRuntime>,
    RepoId,
    Arc<str>,
    tempfile::TempDir,
) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let fake = Arc::new(FakeProvisioner::default());
    let runtime = Arc::new(
        CodeRuntime::new(
            db,
            dir.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_remote_sessions(RemoteSessions::new(fake.clone(), remote_settings())),
    );
    let owner = OwnerId::local();
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: dir.path().join("repo").display().to_string(),
        display_name: "tools".into(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: Some("github.com".into()),
        origin_owner: Some("acme".into()),
        origin_name: Some("tools".into()),
    };
    insert_repo(&runtime.db, &repo).await.unwrap();
    let repo_id = repo.id;
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
    state.code = Some(runtime.clone());
    state.adapter_bootstrap_tokens = Some(Arc::new(crate::auth::AdapterBootstrapTokens::for_test(
        ADAPTER_BOOTSTRAP_TOKEN,
    )));
    let token = state.token.clone();
    (app(state), fake, runtime, repo_id, token, dir)
}

async fn bound_session_id(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    external_key: &str,
) -> tidebreak_core::CodeSessionId {
    tidebreak_core::db::code::get_external_binding(&runtime.db, owner, "slack", external_key)
        .await
        .unwrap()
        .expect("the external session request committed its binding")
        .session_id
}

/// The whole adapter surface over HTTP: bad tokens refuse, get-or-create
/// is idempotent, messages are idempotent on the event id, a foreign grant
/// sees "not found", interrupt reaches the sandbox, and rotation with a
/// replayed refresh kills the grant.
#[tokio::test]
async fn external_routes_scope_by_grant_and_replay_idempotently() {
    let (router, fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();

    // No token and a bogus token both refuse before any handler runs.
    let refused = client
        .post(format!("http://{addr}/external/code/sessions"))
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    let refused = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth("tbg_not_a_token")
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Get-or-create: created once, existing on the retry.
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.unwrap();
    assert_eq!(created["status"], "created");
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/1.1").await;
    let session_id_text = session_id.to_string();
    assert_eq!(
        created["session_id"].as_str(),
        Some(session_id_text.as_str())
    );
    let again: serde_json::Value = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["status"], "existing");
    assert_eq!(again["session_id"].as_str(), Some(session_id_text.as_str()));

    // Messages: the idle session runs the message; the replay answers the
    // same turn without a second spawn.
    let first: serde_json::Value = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "start", "event_id": "Ev1", "channel_ts": "1700000001.000100"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["outcome"], "new_turn");
    assert_eq!(fake.spawns.lock().unwrap().len(), 1);
    let replay: serde_json::Value = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "start", "event_id": "Ev1", "channel_ts": "1700000001.000100"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["outcome"], "new_turn");
    assert_eq!(replay["turn_id"], first["turn_id"]);
    assert_eq!(fake.spawns.lock().unwrap().len(), 1);

    // A foreign grant sees the session as not found, and its get-or-create
    // on the same conversation refuses the same way.
    let (_foreign, foreign_pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U2", "T1")
        .await
        .unwrap();
    let hidden = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&foreign_pair.token)
        .json(&serde_json::json!({
            "text": "mine now", "event_id": "EvX", "channel_ts": "1700000002.000100"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(hidden.status(), reqwest::StatusCode::NOT_FOUND);
    let mismatch = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&foreign_pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), reqwest::StatusCode::NOT_FOUND);

    // Interrupt reaches the sandbox as its desktop equivalent does.
    let interrupted = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&pair.token)
        .send()
        .await
        .unwrap();
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    assert!(fake
        .sends
        .lock()
        .unwrap()
        .iter()
        .any(|message| message.interrupt));

    // Rotation trades the pair; the old access token stops working.
    let rotated: serde_json::Value = client
        .post(format!("http://{addr}/external/grants/rotate"))
        .json(&serde_json::json!({ "refresh": pair.refresh }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_token = rotated["token"].as_str().unwrap().to_owned();
    let stale = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&pair.token)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A replayed rotated refresh kills the grant: the rotation refuses and
    // the new access token stops working too.
    let theft = client
        .post(format!("http://{addr}/external/grants/rotate"))
        .json(&serde_json::json!({ "refresh": pair.refresh }))
        .send()
        .await
        .unwrap();
    assert_eq!(theft.status(), reqwest::StatusCode::UNAUTHORIZED);
    let dead = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(dead.status(), reqwest::StatusCode::UNAUTHORIZED);
    let revoked = tidebreak_core::db::code::get_external_grant(&runtime.db, &owner, grant.id)
        .await
        .unwrap()
        .unwrap();
    assert!(revoked.revoked_at.is_some());
}

/// The event stream opens with the session snapshot (lifecycle and
/// attention), replays the journal — the per-turn assistant record a
/// renderer needs — and drops the moment the grant is revoked.
#[tokio::test]
async fn external_events_snapshot_then_replay_then_sever_on_revoke() {
    let (router, fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C2/9.9", "repo_id": repo_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = bound_session_id(&runtime, &owner, "T1/C2/9.9").await;
    let session_id_text = session_id.to_string();
    assert_eq!(
        created["session_id"].as_str(),
        Some(session_id_text.as_str())
    );
    let first: serde_json::Value = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "start", "event_id": "Ev1", "channel_ts": "1700000001.000100"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["outcome"], "new_turn");

    // A token that holds no binding cannot even upgrade.
    let (_foreign, foreign_pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U2", "T1")
        .await
        .unwrap();
    let mut request = format!("ws://{addr}/external/code/sessions/{session_id}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", foreign_pair.token).parse().unwrap(),
    );
    assert!(connect_async(request).await.is_err());

    let mut request = format!("ws://{addr}/external/code/sessions/{session_id}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", pair.token).parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();

    // First frame: the session snapshot with its attention.
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert_eq!(
        value["snapshot"]["id"].as_str(),
        Some(session_id_text.as_str())
    );
    assert!(value["snapshot"]["attention"].is_object());
    assert!(value["snapshot"]["lifecycle"].is_string());

    // The sandbox streams the turn; ingest journals it, and the frames —
    // the per-turn assistant record a renderer needs — reach the socket.
    fake.event_reads.lock().unwrap().push_back(SandboxEvents {
        sandbox_id: "sb-ext".to_owned(),
        state: SandboxState::Running,
        latest_event_seq: 3,
        events: vec![
            SandboxEvent {
                seq: 1,
                kind: "turn_started".to_owned(),
                payload: serde_json::json!({ "turn": 1 }),
                created_at: String::new(),
            },
            SandboxEvent {
                seq: 2,
                kind: "assistant_record".to_owned(),
                payload: serde_json::json!({ "turn": 1, "body": "done: shipped" }),
                created_at: String::new(),
            },
            SandboxEvent {
                seq: 3,
                kind: "turn_completed".to_owned(),
                payload: serde_json::json!({ "turn": 1, "exit_code": 0 }),
                created_at: String::new(),
            },
        ],
    });
    let mut live = runtime.get_session(&owner, session_id).await.unwrap();
    runtime
        .remote_sessions()
        .unwrap()
        .driver(&runtime.db, runtime.bus.as_ref())
        .pump(&mut live, 0)
        .await
        .unwrap();
    let mut saw_assistant_record = false;
    for _ in 0..20 {
        let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), socket.next()).await
        else {
            break;
        };
        let Ok(text) = frame.to_text() else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        if value["event"]["type"] == "assistant_message" || text.contains("done: shipped") {
            saw_assistant_record = true;
            break;
        }
    }
    assert!(
        saw_assistant_record,
        "the per-turn assistant record must reach the socket"
    );

    // The adapter is a renderer, not a viewer: its reconnect — the resync
    // path — must not clear `DoneUnreviewed`, which the desktop inbox and
    // the channel's own success render both read.
    let before = runtime
        .get_session(&owner, session_id)
        .await
        .unwrap()
        .attention;
    let mut request = format!("ws://{addr}/external/code/sessions/{session_id}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", pair.token).parse().unwrap(),
    );
    let (mut resync, _) = connect_async(request).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), resync.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(frame.to_text().unwrap().contains("snapshot"));
    let after = runtime
        .get_session(&owner, session_id)
        .await
        .unwrap()
        .attention;
    assert_eq!(
        after.state, before.state,
        "an adapter connect must not count as the owner viewing the session"
    );
    drop(resync);

    // Revocation severs the live stream promptly.
    runtime
        .revoke_adapter_grant(&owner, grant.id, "owner unlinked the workspace")
        .await
        .unwrap();
    let severed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                None => break,
                Some(Err(_)) => break,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(
        severed.is_ok(),
        "the revoked grant's stream must drop immediately"
    );
}

/// The human halves of the grant lifecycle: the connect handshake mints a
/// grant bound to the shown identity, but only at the adapter's closing
/// confirm — a forwarded link that is merely approved binds nothing — and
/// the desktop lists and revokes grants, whole workspaces included.
/// The operator's pairing probe answers only to the bootstrap token and
/// writes nothing: the setup page learns the pairing is wrong at setup
/// time, not at a user's first connect card.
#[tokio::test]
async fn the_pairing_probe_answers_only_to_the_bootstrap_token() {
    let (router, _fake, _runtime, _repo_id, _token, _dir) = external_app_with_token().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let missing = client
        .get(format!("http://{addr}/external/connect/probe"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);
    let wrong = client
        .get(format!("http://{addr}/external/connect/probe"))
        .bearer_auth("wrong-bootstrap-token-padded-to-thirty-two")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
    let ok = client
        .get(format!("http://{addr}/external/connect/probe"))
        .bearer_auth(ADAPTER_BOOTSTRAP_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), reqwest::StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_connect_handshake_mints_only_at_the_closing_confirm() {
    let (router, _fake, runtime, _repo_id, token, _dir) = external_app_with_token().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();

    let missing_bootstrap = client
        .post(format!("http://{addr}/external/connect"))
        .json(&serde_json::json!({
            "channel_kind": "slack",
            "external_identity": "U1",
            "workspace_identity": "T1",
            "display_name": "Casey",
            "workspace_name": "Acme Corp",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing_bootstrap.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let wrong_bootstrap = client
        .post(format!("http://{addr}/external/connect"))
        .bearer_auth("wrong-bootstrap-token-padded-to-thirty-two")
        .json(&serde_json::json!({
            "channel_kind": "slack",
            "external_identity": "U1",
            "workspace_identity": "T1",
            "display_name": "Casey",
            "workspace_name": "Acme Corp",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_bootstrap.status(), reqwest::StatusCode::UNAUTHORIZED);

    let started: serde_json::Value = client
        .post(format!("http://{addr}/external/connect"))
        .bearer_auth(ADAPTER_BOOTSTRAP_TOKEN)
        .json(&serde_json::json!({
            "channel_kind": "slack",
            "external_identity": "U1",
            "workspace_identity": "T1",
            "display_name": "Casey",
            "workspace_name": "Acme Corp",
            "avatar_url": "https://example.com/a.png",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce = started["nonce"].as_str().unwrap().to_owned();
    let confirmation_token = started["confirmation_token"].as_str().unwrap().to_owned();

    // The approval link alone cannot observe or complete the adapter half.
    let unconfirmed_status = client
        .get(format!("http://{addr}/external/connect/{nonce}/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(unconfirmed_status.status(), reqwest::StatusCode::NOT_FOUND);
    let pending: serde_json::Value = client
        .get(format!("http://{addr}/external/connect/{nonce}/status"))
        .bearer_auth(&confirmation_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending["state"], "pending");

    // The approval page shows the identity being linked.
    let page: serde_json::Value = client
        .get(format!("http://{addr}/external/connect/{nonce}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["display_name"], "Casey");
    assert_eq!(page["workspace_name"], "Acme Corp");
    assert_eq!(page["state"], "pending");
    let csrf = page["csrf"].as_str().unwrap().to_owned();

    // A forwarded link binds nothing: the link lacks the adapter's separate
    // confirmation capability, completion before approval refuses even with
    // that capability, and a wrong CSRF token cannot approve.
    let link_only = client
        .post(format!("http://{addr}/external/connect/{nonce}/complete"))
        .send()
        .await
        .unwrap();
    assert_eq!(link_only.status(), reqwest::StatusCode::NOT_FOUND);
    let early = client
        .post(format!("http://{addr}/external/connect/{nonce}/complete"))
        .bearer_auth(&confirmation_token)
        .send()
        .await
        .unwrap();
    assert_eq!(early.status(), reqwest::StatusCode::NOT_FOUND);
    let forged = client
        .post(format!("http://{addr}/external/connect/{nonce}/approve"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "csrf": "not-the-token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), reqwest::StatusCode::NOT_FOUND);

    // The owner approves. Nothing is minted yet.
    let approved = client
        .post(format!("http://{addr}/external/connect/{nonce}/approve"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "csrf": csrf }))
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), reqwest::StatusCode::NO_CONTENT);
    let approved_status: serde_json::Value = client
        .get(format!("http://{addr}/external/connect/{nonce}/status"))
        .bearer_auth(&confirmation_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(approved_status["state"], "approved");
    let grants: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/grants"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(grants.is_empty(), "approval alone must mint nothing");

    // The closing confirm mints the grant bound to the shown identity,
    // exactly once.
    let completed: serde_json::Value = client
        .post(format!("http://{addr}/external/connect/{nonce}/complete"))
        .bearer_auth(&confirmation_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["grant"]["external_identity"], "U1");
    assert_eq!(completed["grant"]["workspace_identity"], "T1");
    assert!(completed["token"].as_str().unwrap().starts_with("tbg_"));
    let replayed = client
        .post(format!("http://{addr}/external/connect/{nonce}/complete"))
        .bearer_auth(&confirmation_token)
        .send()
        .await
        .unwrap();
    assert_eq!(replayed.status(), reqwest::StatusCode::NOT_FOUND);
    let stale_view = client
        .get(format!("http://{addr}/external/connect/{nonce}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(stale_view.status(), reqwest::StatusCode::NOT_FOUND);

    // The desktop lists the grant and revokes it.
    let grants: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/grants"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["display_name"], "Casey");
    assert_eq!(grants[0]["workspace_name"], "Acme Corp");
    assert_eq!(grants[0]["avatar_url"], "https://example.com/a.png");
    let grant_id = grants[0]["id"].as_str().unwrap().to_owned();
    let revoked: serde_json::Value = client
        .post(format!("http://{addr}/code/grants/{grant_id}/revoke"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "reason": "done with this workspace" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(revoked["revoked_at"].is_string());
    assert_eq!(revoked["revoked_reason"], "done with this workspace");

    // Revoking a whole workspace cuts every live grant it holds.
    runtime
        .mint_adapter_grant(&owner, "slack", "U5", "T9")
        .await
        .unwrap();
    runtime
        .mint_adapter_grant(&owner, "slack", "U6", "T9")
        .await
        .unwrap();
    let swept: Vec<serde_json::Value> = client
        .post(format!("http://{addr}/code/grants/revoke-workspace"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "channel_kind": "slack", "workspace_identity": "T9" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(swept.len(), 2);
    assert!(swept.iter().all(|grant| grant["revoked_at"].is_string()));
}

/// The approval page never receives an avatar that can target a local host or
/// carry credentials. Invalid avatar input is a request error, not a stored
/// surprise that the browser later loads.
#[tokio::test]
async fn connect_rejects_unsafe_avatar_urls() {
    let (router, _fake, _runtime, _repo_id, _token, _dir) = external_app_with_token().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    for avatar_url in [
        "http://example.com/a.png",
        "https://user:secret@example.com/a.png",
        "https://127.0.0.1/a.png",
        "https://avatars.local/a.png",
    ] {
        let response = client
            .post(format!("http://{addr}/external/connect"))
            .bearer_auth(ADAPTER_BOOTSTRAP_TOKEN)
            .json(&serde_json::json!({
                "channel_kind": "slack",
                "external_identity": "U1",
                "workspace_identity": "T1",
                "display_name": "Casey",
                "workspace_name": "Acme Corp",
                "avatar_url": avatar_url,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}

/// The public bootstrap route never buffers an unbounded identity document.
#[tokio::test]
async fn connect_start_rejects_an_oversized_body() {
    let (router, _fake, _runtime, _repo_id, _token, _dir) = external_app_with_token().await;
    let addr = serve(router).await;
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/external/connect"))
        .bearer_auth(ADAPTER_BOOTSTRAP_TOKEN)
        .json(&serde_json::json!({
            "channel_kind": "slack",
            "external_identity": "U1",
            "workspace_identity": "T1",
            "display_name": "x".repeat(20 * 1024),
            "workspace_name": "Acme Corp",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
}
