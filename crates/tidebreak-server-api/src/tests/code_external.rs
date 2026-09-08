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
        _session: tidebreak_core::SessionId,
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
        _session: tidebreak_core::SessionId,
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
        _session: tidebreak_core::SessionId,
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
        _session: tidebreak_core::SessionId,
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

    async fn cancel(
        &self,
        _owner: &OwnerId,
        _session: tidebreak_core::SessionId,
        _sandbox_id: &str,
    ) -> Result<(), RemoteSandboxError> {
        Ok(())
    }
}

fn remote_settings() -> crate::code::remote::driver::RemoteSpawnSettings {
    crate::code::remote::driver::RemoteSpawnSettings {
        profile: "tidebreak-remote".to_owned(),
        engine: None,
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
    external_app_built(|runtime| runtime).await
}

/// [`external_app_with_token`] with the runtime shaped by the caller before
/// the app is built: a lender or a relay a test needs wired in.
async fn external_app_built(
    customize: impl FnOnce(CodeRuntime) -> CodeRuntime,
) -> (
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
    let runtime = Arc::new(customize(
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
    ));
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
) -> tidebreak_core::SessionId {
    tidebreak_core::db::code::get_external_binding(&runtime.db, owner, "slack", external_key)
        .await
        .unwrap()
        .expect("the external session request committed its binding")
        .session_id
}

/// A channel names its repository the way a forge does. `repository:
/// owner/name` resolves to the owner's registered checkout regardless of
/// case, a name nobody registered is a typed conflict the adapter can word,
/// and a request naming no repository at all is a bad request.
#[tokio::test]
async fn an_external_session_names_its_repository_by_origin() {
    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();

    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C9/1.1", "repository": "ACME/Tools" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C9/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    let workspace = runtime
        .get_workspace(
            &owner,
            session
                .workspace_id
                .expect("a repository session has a workspace"),
        )
        .await
        .unwrap();
    assert_eq!(
        workspace.repo_id, repo_id,
        "the origin resolved to the registered checkout"
    );

    let unknown = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C9/2.1", "repository": "acme/other" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(body["kind"], "repo_unknown");

    let nameless = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C9/3.1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(nameless.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// A fake forge that lends one fixed credential for whichever repository is
/// asked, recording the ask.
struct LendingFake {
    minted: StdMutex<Vec<String>>,
    asked: StdMutex<Vec<crate::obo_gateway::GitForgeAttributionRequest>>,
}

impl LendingFake {
    fn new() -> Self {
        Self {
            minted: StdMutex::new(Vec::new()),
            asked: StdMutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl crate::obo_gateway::GitCredentialLender for LendingFake {
    async fn git_forge_identity(
        &self,
        _owner: &OwnerId,
        attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitForgeIdentity, crate::obo_gateway::GitForgeError> {
        self.asked.lock().unwrap().push(attribution);
        Ok(crate::obo_gateway::GitForgeIdentity {
            app_name: "Acme Forge".to_owned(),
            attribution: match attribution {
                crate::obo_gateway::GitForgeAttributionRequest::Person => {
                    crate::obo_gateway::GitForgeAttribution::Person {
                        login: "mira".to_owned(),
                        display_name: None,
                        commit_email: None,
                    }
                }
                crate::obo_gateway::GitForgeAttributionRequest::Installation => {
                    crate::obo_gateway::GitForgeAttribution::Bot {
                        bot_login: Some("acme-bot".to_owned()),
                    }
                }
            },
        })
    }

    async fn git_credential(
        &self,
        _owner: &OwnerId,
        repository: &str,
        attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitCredential, crate::obo_gateway::GitForgeError> {
        self.asked.lock().unwrap().push(attribution);
        self.minted.lock().unwrap().push(repository.to_owned());
        Ok(crate::obo_gateway::GitCredential {
            username: "x-access-token".to_owned(),
            secret: "lent-secret".to_owned(),
        })
    }

    async fn list_repositories(
        &self,
        _owner: &OwnerId,
        attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<Vec<crate::obo_gateway::GitHubRepository>, crate::obo_gateway::GitForgeError> {
        self.asked.lock().unwrap().push(attribution);
        Ok(Vec::new())
    }
}

/// A machine session's own git borrows the person's forge credential from
/// the loopback route under the session's relay key: `https` against the
/// repository's origin host answers a credential minted for that
/// repository, any other host or protocol answers nothing, and a missing or
/// unknown key is refused before anything is looked up.
#[tokio::test]
async fn a_sessions_git_borrows_the_persons_credential_from_the_loopback_route() {
    let lender = Arc::new(LendingFake::new());
    let gateway = Arc::new(
        crate::obo_gateway::OboGateway::new(
            "https://gateway.example",
            "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed".to_owned(),
        )
        .unwrap(),
    );
    let relay = Arc::new(crate::code::harness_llm::HarnessLlmRelay::new(gateway));
    let (router, _fake, runtime, repo_id, _token, _dir) = external_app_built({
        let lender = lender.clone();
        let relay = relay.clone();
        move |runtime| runtime.with_git_credentials(lender).with_harness_llm(relay)
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C7/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C7/1.1").await;
    let key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
        owner: owner.clone(),
        session: session_id,
    });
    let route = format!(
        "http://{addr}{}",
        crate::code::harness_llm::GIT_CREDENTIAL_PATH
    );
    let ask = |body: &'static str, key: Option<&str>| {
        let mut request = client.post(&route).body(body);
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        request.send()
    };

    let lent = ask(
        "protocol=https\nhost=github.com\npath=acme/tools.git\n",
        Some(&key),
    )
    .await
    .unwrap();
    assert_eq!(lent.status(), reqwest::StatusCode::OK);
    assert_eq!(
        lent.text().await.unwrap(),
        "username=x-access-token\npassword=lent-secret\n"
    );
    assert_eq!(
        lender.minted.lock().unwrap().as_slice(),
        ["acme/tools"],
        "minted for the workspace's own repository"
    );
    assert_eq!(
        lender.asked.lock().unwrap().as_slice(),
        [
            crate::obo_gateway::GitForgeAttributionRequest::Person,
            crate::obo_gateway::GitForgeAttributionRequest::Person,
        ],
        "get-or-create probes the person, then the session's git borrow uses that identity"
    );

    let person = runtime.get_session(&owner, session_id).await.unwrap();
    let mut bot_session = person.clone();
    bot_session.id = tidebreak_core::SessionId::new();
    bot_session.acts_as = Some(tidebreak_core::ActsAs::Bot);
    tidebreak_core::db::code::insert_session(&runtime.db, &bot_session)
        .await
        .unwrap();
    let bot_key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
        owner: owner.clone(),
        session: bot_session.id,
    });
    let bot_lent = ask(
        "protocol=https\nhost=github.com\npath=acme/tools.git\n",
        Some(&bot_key),
    )
    .await
    .unwrap();
    assert_eq!(bot_lent.status(), reqwest::StatusCode::OK);
    assert_eq!(
        lender.asked.lock().unwrap().as_slice(),
        [
            crate::obo_gateway::GitForgeAttributionRequest::Person,
            crate::obo_gateway::GitForgeAttributionRequest::Person,
            crate::obo_gateway::GitForgeAttributionRequest::Installation,
        ],
        "a bot session asks to act as the installation"
    );

    for other in [
        "protocol=https\nhost=evil.example\n",
        "protocol=http\nhost=github.com\n",
        "protocol=ssh\nhost=github.com\n",
    ] {
        let nothing = ask(other, Some(&key)).await.unwrap();
        assert_eq!(nothing.status(), reqwest::StatusCode::OK, "{other:?}");
        assert_eq!(
            nothing.text().await.unwrap(),
            "",
            "{other:?} gets no credential"
        );
    }
    assert_eq!(
        lender.minted.lock().unwrap().len(),
        2,
        "no mint for a host that is not the origin"
    );

    let unkeyed = ask("protocol=https\nhost=github.com\n", None)
        .await
        .unwrap();
    assert_eq!(unkeyed.status(), reqwest::StatusCode::UNAUTHORIZED);
    let wrong = ask("protocol=https\nhost=github.com\n", Some("not-a-key"))
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// A standalone machine's static lender answers the deployment token for
/// `https` against github.com, nothing for another host, and refuses a
/// Person request because the machine acts as one account.
#[tokio::test]
async fn a_sessions_git_borrows_the_standalone_deployment_token_from_the_loopback_route() {
    let lender = Arc::new(crate::obo_gateway::StaticGitCredentialLender::new(
        "deployment-token".to_owned(),
        Some("ship-bot".to_owned()),
    ));
    let relay = Arc::new(crate::code::harness_llm::HarnessLlmRelay::keys_only());
    let (router, _fake, runtime, repo_id, _token, _dir) = external_app_built({
        let lender = lender.clone();
        let relay = relay.clone();
        move |runtime| runtime.with_git_credentials(lender).with_harness_llm(relay)
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C8/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C8/1.1").await;
    let bot = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(bot.acts_as(), tidebreak_core::ActsAs::Bot);
    let mut person_session = bot.clone();
    person_session.id = tidebreak_core::SessionId::new();
    person_session.acts_as = Some(tidebreak_core::ActsAs::Person);
    tidebreak_core::db::code::insert_session(&runtime.db, &person_session)
        .await
        .unwrap();
    let bot_key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
        owner: owner.clone(),
        session: session_id,
    });
    let person_key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
        owner: owner.clone(),
        session: person_session.id,
    });
    let route = format!(
        "http://{addr}{}",
        crate::code::harness_llm::GIT_CREDENTIAL_PATH
    );
    let ask =
        |body: &'static str, key: &str| client.post(&route).bearer_auth(key).body(body).send();

    let lent = ask(
        "protocol=https\nhost=github.com\npath=acme/tools.git\n",
        &bot_key,
    )
    .await
    .unwrap();
    assert_eq!(lent.status(), reqwest::StatusCode::OK);
    assert_eq!(
        lent.text().await.unwrap(),
        "username=x-access-token\npassword=deployment-token\n"
    );

    let other = ask("protocol=https\nhost=evil.example\n", &bot_key)
        .await
        .unwrap();
    assert_eq!(other.status(), reqwest::StatusCode::OK);
    assert_eq!(other.text().await.unwrap(), "");

    let person = ask(
        "protocol=https\nhost=github.com\npath=acme/tools.git\n",
        &person_key,
    )
    .await
    .unwrap();
    assert_eq!(person.status(), reqwest::StatusCode::BAD_GATEWAY);
    let reason = person.text().await.unwrap();
    assert!(
        reason.contains("standalone machine acts as one"),
        "the helper names why person is refused: {reason}"
    );
}

/// A Slack sandbox stays remote when a browser follows the link, queues a
/// turn, and the hosted process recovers its session rows.
#[tokio::test]
async fn web_follow_ups_and_recovery_keep_a_slack_session_in_its_sandbox() {
    let (router, fake, runtime, repo_id, token, _dir) = external_app_built(|mut runtime| {
        runtime
            .adapters
            .register(Arc::new(crate::scripted_harness::ScriptedAdapter::new(
                crate::scripted_harness::plain_text_script(),
            )));
        runtime
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C-web/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C-web/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.execution_location,
        tidebreak_core::ExecutionLocation::Sandbox
    );
    assert!(!runtime.has_worker(session_id));
    let error = runtime
        .attach_and_spawn_worker(session.clone())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "session_remote");

    // The session location remains authoritative if old workspace metadata
    // still names a local path after recovery or an upgrade.
    let mut workspace = runtime
        .get_workspace(&owner, session.workspace_id.unwrap())
        .await
        .unwrap();
    workspace.worktree_path = _dir
        .path()
        .join("obsolete-local-path")
        .display()
        .to_string();
    tidebreak_core::db::code::save_workspace(&runtime.db, &workspace)
        .await
        .unwrap();

    let first = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "start in Slack", "event_id": "Ev-web", "channel_ts": "1700000001.000100"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(fake.spawns.lock().unwrap().len(), 1);

    let queued = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "continue from the browser" }))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), reqwest::StatusCode::ACCEPTED);
    let queued: serde_json::Value = queued.json().await.unwrap();
    assert_eq!(queued["message"], "continue from the browser");
    runtime.recover().await.unwrap();
    assert!(!runtime.has_worker(session_id));
    assert_eq!(
        runtime
            .get_session(&owner, session_id)
            .await
            .unwrap()
            .lifecycle,
        tidebreak_core::SessionLifecycle::Running
    );

    fake.event_reads.lock().unwrap().push_back(SandboxEvents {
        sandbox_id: "sb-ext".into(),
        state: SandboxState::Running,
        latest_event_seq: 2,
        events: vec![
            SandboxEvent {
                seq: 1,
                kind: "turn_started".into(),
                payload: serde_json::json!({ "turn": 1 }),
                created_at: String::new(),
            },
            SandboxEvent {
                seq: 2,
                kind: "turn_completed".into(),
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
    runtime.start(format!("http://{addr}")).await.unwrap();
    super::code::wait_until(|| {
        fake.sends
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.body == "continue from the browser")
    })
    .await;
    assert_eq!(fake.spawns.lock().unwrap().len(), 1);
    assert!(!runtime.has_worker(session_id));
    // The queued turn drains after the send lands, so wait for it rather
    // than reading the queue in the same instant.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !tidebreak_core::db::code::list_queued_turns(&runtime.db, &owner, session_id)
            .await
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued turn drains after the send lands");
    let interrupted = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&token)
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
}

/// A configured runtime places an external session in the sandbox and leaves
/// a desktop session on the same app on the machine, with a host worktree.
#[tokio::test]
async fn a_runtime_places_only_external_sessions_in_the_sandbox() {
    let (router, _fake, runtime, repo_id, token, dir) = external_app_built(|mut runtime| {
        runtime
            .adapters
            .register(Arc::new(crate::scripted_harness::ScriptedAdapter::new(
                crate::scripted_harness::plain_text_script(),
            )));
        runtime
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C-place/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C-place/1.1").await;
    let snapshot = client
        .get(format!("http://{addr}/code/sessions/{session_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(snapshot.status(), reqwest::StatusCode::OK);
    let snapshot: serde_json::Value = snapshot.json().await.unwrap();
    assert_eq!(snapshot["execution_location"], "sandbox");
    assert_eq!(
        runtime
            .get_session(&owner, session_id)
            .await
            .unwrap()
            .execution_location,
        tidebreak_core::ExecutionLocation::Sandbox
    );

    let repo = super::code::init_git_repo(dir.path());
    let (_repo, workspace) =
        super::code::register_and_workspace(&client, addr, &token, &repo).await;
    let desktop = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            super::code::json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "harness": "claude_code", "permission_mode": "plan" }))
        .send()
        .await
        .unwrap();
    let desktop_status = desktop.status();
    let desktop_body = desktop.text().await.unwrap();
    assert_eq!(
        desktop_status,
        reqwest::StatusCode::CREATED,
        "{desktop_body}"
    );
    let desktop: serde_json::Value = serde_json::from_str(&desktop_body).unwrap();
    assert_eq!(desktop["execution_location"], "machine");
    let desktop_id: tidebreak_core::SessionId =
        desktop["id"].as_str().unwrap().parse().expect("session id");
    assert_eq!(
        runtime
            .get_session(&owner, desktop_id)
            .await
            .unwrap()
            .execution_location,
        tidebreak_core::ExecutionLocation::Machine
    );
    assert!(runtime.has_worker(desktop_id));
    let files = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/files",
            super::code::json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(files.status(), reqwest::StatusCode::OK);
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
    assert_eq!(
        value["snapshot"]["execution_location"], "sandbox",
        "a runtime is configured, so the session runs in a sandbox"
    );

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

/// A message from a channel is attributed to the person who sent it there,
/// not to the shared principal that owns the session (decision 0086). The
/// channel identity comes from the grant behind the call and the display name
/// from the adapter; a call that sends no name leaves it unset rather than
/// inventing one.
#[tokio::test(flavor = "multi_thread")]
async fn an_external_message_carries_the_senders_channel_identity() {
    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U7", "T1")
        .await
        .unwrap();

    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C9/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C9/1.1").await;

    let named: serde_json::Value = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "take a look at this",
            "event_id": "Ev9",
            "channel_ts": "1700000009.000100",
            "display": "Ines Okafor",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(named["outcome"], "new_turn");
    let turn_id: tidebreak_core::TurnId = serde_json::from_value(named["turn_id"].clone()).unwrap();
    let turn = tidebreak_core::db::code::get_turn(&runtime.db, &owner, turn_id)
        .await
        .unwrap()
        .expect("the delivered message became a turn");
    let actor = turn.actor.expect("an adapter message names its sender");
    assert_eq!(actor.channel_kind.as_deref(), Some("slack"));
    assert_eq!(actor.external_identity.as_deref(), Some("U7"));
    assert_eq!(actor.display.as_deref(), Some("Ines Okafor"));
    assert_eq!(
        actor.principal, None,
        "a person who never connected holds no principal here"
    );

    // A second message while the first is in flight parks as a queue row, and
    // the row carries the same attribution so promotion keeps it.
    let queued: serde_json::Value = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "and this one too",
            "event_id": "Ev10",
            "channel_ts": "1700000010.000100",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queued["outcome"], "queued");
    let (rows, _paused) =
        tidebreak_core::db::code::list_queued_turns(&runtime.db, &owner, session_id)
            .await
            .map(|rows| (rows, false))
            .unwrap();
    let row = rows.first().expect("the second message parked");
    let actor = row
        .actor
        .clone()
        .expect("a parked message names its sender");
    assert_eq!(actor.channel_kind.as_deref(), Some("slack"));
    assert_eq!(actor.external_identity.as_deref(), Some("U7"));
    assert_eq!(
        actor.display, None,
        "no display name is not the same as a made-up one"
    );
}

/// An app with no sandbox runtime and the scripted engine registered as
/// Claude Code, plus one local git repository: the machine the adapter's
/// end-to-end lane drives.
async fn machine_app() -> (Router, Arc<CodeRuntime>, RepoId, tempfile::TempDir) {
    machine_app_built(|runtime| runtime).await
}

/// [`machine_app`] with the runtime shaped by the caller before the app is
/// built: the operator's permission policy, say.
async fn machine_app_built(
    customize: impl FnOnce(CodeRuntime) -> CodeRuntime,
) -> (Router, Arc<CodeRuntime>, RepoId, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = tidebreak_harness::AdapterRegistry::new();
    // The machine location takes the deployment's default mode, `ask`, so
    // the engine must offer structured approvals the way Claude Code does;
    // the operator's policy tests name the other modes, so the engine
    // offers those too.
    registry.register(Arc::new(
        crate::scripted_harness::ScriptedAdapter::new(crate::scripted_harness::plain_text_script())
            .with_approvals(tidebreak_core::CapLevel::Supported)
            .with_plan_mode(tidebreak_core::CapLevel::Supported)
            .with_auto_mode(tidebreak_core::CapLevel::Supported)
            .with_allow_mode(tidebreak_core::CapLevel::Supported),
    ));
    let runtime = Arc::new(customize(CodeRuntime::with_registry_and_browser_runtime(
        db,
        dir.path().to_path_buf(),
        registry,
        None,
        None,
    )));
    let owner = OwnerId::local();
    let root = super::code::init_git_repo(dir.path());
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: root.display().to_string(),
        display_name: "tools".into(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    insert_repo(&runtime.db, &repo).await.unwrap();
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
    (app(state), runtime, repo.id, dir)
}

/// Decision 0088: a machine with no sandbox runtime runs an external
/// session on its own engine. The conversation gets a worktree under the
/// machine's root and an ordinary session at the deployment's default
/// permission mode; a second get-or-create answers the same session; a
/// message becomes a turn at once and its reply reaches the external
/// stream, whose snapshot names the location; a replayed delivery answers
/// from the first row.
#[tokio::test]
async fn a_machine_without_a_runtime_runs_an_external_session_on_its_own_engine() {
    let (router, runtime, repo_id, _dir) = machine_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();

    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    let status = created.status();
    let body = created.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CREATED, "{body}");
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.execution_location,
        tidebreak_core::ExecutionLocation::Machine
    );
    assert_eq!(
        session.permission_mode,
        tidebreak_core::PermissionMode::default(),
        "the channel's Allow is a sandbox posture; the machine takes the deployment default"
    );
    let workspace = runtime
        .get_workspace(
            &owner,
            session
                .workspace_id
                .expect("a repository session has a workspace"),
        )
        .await
        .unwrap();
    assert!(
        !workspace.is_remote(),
        "the workspace is a worktree on the machine"
    );
    assert!(
        std::path::Path::new(&workspace.worktree_path)
            .join("README.md")
            .is_file(),
        "the worktree is checked out at {}",
        workspace.worktree_path
    );

    let again = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C1/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::OK);
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again["status"], "existing");
    assert_eq!(again["session_id"], session_id.to_string());

    let delivered = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "text": "say hello", "event_id": "Ev-1", "channel_ts": "1.1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(delivered.status(), reqwest::StatusCode::OK);
    let delivered: serde_json::Value = delivered.json().await.unwrap();
    assert_eq!(
        delivered["outcome"], "new_turn",
        "an idle machine session promotes the message at once: {delivered}"
    );
    let turn_id = delivered["turn_id"].as_str().unwrap().to_owned();

    let mut request = format!("ws://{addr}/external/code/sessions/{session_id}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", pair.token).parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut saw_snapshot = false;
    let mut saw_reply = false;
    while !(saw_snapshot && saw_reply) {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the stream carries the snapshot and the scripted reply in time")
            .expect("the stream stays open")
            .unwrap();
        let Ok(text) = frame.to_text() else { continue };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if value["snapshot"]["execution_location"] == "machine" {
                saw_snapshot = true;
            }
        }
        if text.contains("hello from the scripted engine") {
            saw_reply = true;
        }
    }

    let replay = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "text": "say hello", "event_id": "Ev-1", "channel_ts": "1.1" }))
        .send()
        .await
        .unwrap();
    let replay: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(
        replay["turn_id"], turn_id,
        "a replayed delivery answers from the first row"
    );
}

/// Decision 88's amendment: on the machine's engine a channel session takes
/// the operator's default mode when it names none, the mode it names up to
/// the operator's ceiling, and a refusal by name above it. The refusal is a
/// conflict the adapter can show, not a silently clamped session.
#[tokio::test]
async fn a_machine_session_takes_the_operators_mode_and_refuses_above_the_ceiling() {
    let (router, runtime, repo_id, _dir) = machine_app_built(|runtime| {
        runtime.with_external_permission_policy(
            tidebreak_core::PermissionMode::Auto,
            tidebreak_core::PermissionMode::Allow,
        )
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let start = |key: &'static str, mode: Option<&'static str>| {
        let mut body = serde_json::json!({ "external_key": key, "repo_id": repo_id });
        if let Some(mode) = mode {
            body["permission_mode"] = serde_json::Value::String(mode.to_owned());
        }
        client
            .post(format!("http://{addr}/external/code/sessions"))
            .bearer_auth(&pair.token)
            .json(&body)
            .send()
    };

    let defaulted = start("T1/C1/1.1", None).await.unwrap();
    assert_eq!(defaulted.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.permission_mode,
        tidebreak_core::PermissionMode::Auto,
        "the channel named no mode, so the operator's default applies"
    );

    let named = start("T1/C1/2.2", Some("allow")).await.unwrap();
    assert_eq!(named.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/2.2").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.permission_mode,
        tidebreak_core::PermissionMode::Allow,
        "a mode at the ceiling is honored"
    );

    let lower = start("T1/C1/3.3", Some("plan")).await.unwrap();
    assert_eq!(lower.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/3.3").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.permission_mode,
        tidebreak_core::PermissionMode::Plan
    );

    let unknown = start("T1/C1/4.4", Some("bypass")).await.unwrap();
    assert_eq!(
        unknown.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a mode this machine does not know is a malformed request"
    );
}

/// The ceiling is the operator's line: a channel that names a mode above
/// it is refused by name, with the ceiling and the setting to raise it in
/// the message, and no session or workspace is created.
#[tokio::test]
async fn a_machine_session_above_the_ceiling_is_refused_by_name() {
    let (router, runtime, repo_id, _dir) = machine_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let refused = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C1/9.9",
            "repo_id": repo_id,
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_above_ceiling", "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(message.contains("up to ask"), "{message}");
    assert!(
        message.contains("TIDEBREAK_EXTERNAL_PERMISSION_CEILING"),
        "{message}"
    );
    assert!(
        tidebreak_core::db::code::get_external_binding(&runtime.db, &owner, "slack", "T1/C1/9.9")
            .await
            .unwrap()
            .is_none(),
        "a refused start binds nothing"
    );
}

struct ProbeFake {
    person:
        StdMutex<Result<crate::obo_gateway::GitForgeIdentity, crate::obo_gateway::GitForgeError>>,
    installation:
        StdMutex<Result<crate::obo_gateway::GitForgeIdentity, crate::obo_gateway::GitForgeError>>,
    minted: StdMutex<Vec<crate::obo_gateway::GitForgeAttributionRequest>>,
    asked: StdMutex<Vec<crate::obo_gateway::GitForgeAttributionRequest>>,
}

impl ProbeFake {
    fn person_connected(login: &str) -> Arc<Self> {
        Arc::new(Self {
            person: StdMutex::new(Ok(crate::obo_gateway::GitForgeIdentity {
                app_name: "Acme Forge".to_owned(),
                attribution: crate::obo_gateway::GitForgeAttribution::Person {
                    login: login.to_owned(),
                    display_name: Some("Mira".to_owned()),
                    commit_email: None,
                },
            })),
            installation: StdMutex::new(Ok(crate::obo_gateway::GitForgeIdentity {
                app_name: "Acme Forge".to_owned(),
                attribution: crate::obo_gateway::GitForgeAttribution::Bot {
                    bot_login: Some("acme-bot".to_owned()),
                },
            })),
            minted: StdMutex::new(Vec::new()),
            asked: StdMutex::new(Vec::new()),
        })
    }

    fn not_connected(connect_url: &str) -> Arc<Self> {
        let fake = Self::person_connected("mira");
        *fake.person.lock().unwrap() = Err(crate::obo_gateway::GitForgeError::NotConnected {
            connect_url: Some(connect_url.to_owned()),
        });
        fake
    }

    fn unavailable() -> Arc<Self> {
        let fake = Self::person_connected("mira");
        *fake.person.lock().unwrap() = Err(crate::obo_gateway::GitForgeError::Unavailable(
            "forge down".into(),
        ));
        fake
    }
}

#[async_trait::async_trait]
impl crate::obo_gateway::GitCredentialLender for ProbeFake {
    async fn git_forge_identity(
        &self,
        _owner: &OwnerId,
        attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitForgeIdentity, crate::obo_gateway::GitForgeError> {
        self.asked.lock().unwrap().push(attribution);
        match attribution {
            crate::obo_gateway::GitForgeAttributionRequest::Person => {
                self.person.lock().unwrap().clone()
            }
            crate::obo_gateway::GitForgeAttributionRequest::Installation => {
                self.installation.lock().unwrap().clone()
            }
        }
    }

    async fn git_credential(
        &self,
        _owner: &OwnerId,
        _repository: &str,
        attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitCredential, crate::obo_gateway::GitForgeError> {
        self.minted.lock().unwrap().push(attribution);
        Ok(crate::obo_gateway::GitCredential {
            username: "x-access-token".to_owned(),
            secret: "lent-secret".to_owned(),
        })
    }

    async fn list_repositories(
        &self,
        _owner: &OwnerId,
        _attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<Vec<crate::obo_gateway::GitHubRepository>, crate::obo_gateway::GitForgeError> {
        Ok(Vec::new())
    }
}

async fn machine_with_lender(
    lender: Arc<ProbeFake>,
) -> (Router, Arc<CodeRuntime>, RepoId, tempfile::TempDir) {
    let cloned = lender.clone();
    machine_app_built(move |runtime| runtime.with_git_credentials(cloned)).await
}

/// A DM with no `acts_as` acts as the person when the gateway offers them.
#[tokio::test]
async fn an_external_machine_session_acts_as_the_connected_person() {
    let lender = ProbeFake::person_connected("mira");
    let (router, runtime, repo_id, _dir) = machine_with_lender(lender.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C-id/1.1", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["acts_as"], "person");
    assert_eq!(body["acting_login"], "mira");
    assert_eq!(body["app_name"], "Acme Forge");
    assert!(body.get("connect_url").is_none());
    let session_id = bound_session_id(&runtime, &owner, "T1/C-id/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(session.acts_as(), tidebreak_core::ActsAs::Person);
    assert!(
        lender
            .asked
            .lock()
            .unwrap()
            .contains(&crate::obo_gateway::GitForgeAttributionRequest::Person),
        "the clone path probes the person before checkout"
    );
}

/// When the person is not connected, the session runs as the bot and names
/// the connect URL.
#[tokio::test]
async fn an_external_machine_session_falls_back_to_the_bot_when_not_connected() {
    let lender = ProbeFake::not_connected("https://gateway.example/connect");
    let (router, runtime, repo_id, _dir) = machine_with_lender(lender.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C-id/2.2", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["acts_as"], "bot");
    assert_eq!(body["connect_url"], "https://gateway.example/connect");
    assert_eq!(body["acting_login"], "acme-bot");
    let session_id = bound_session_id(&runtime, &owner, "T1/C-id/2.2").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(session.acts_as(), tidebreak_core::ActsAs::Bot);
    assert!(
        lender
            .asked
            .lock()
            .unwrap()
            .contains(&crate::obo_gateway::GitForgeAttributionRequest::Installation),
        "the clone borrows the installation after the person probe fails"
    );
}

/// A person may ask for the bot even when their identity is offered.
#[tokio::test]
async fn an_explicit_bot_request_acts_as_the_bot_while_connected() {
    let lender = ProbeFake::person_connected("mira");
    let (router, runtime, repo_id, _dir) = machine_with_lender(lender.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C-id/3.3",
            "repo_id": repo_id,
            "acts_as": "bot",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["acts_as"], "bot");
    assert_eq!(body["acting_login"], "acme-bot");
    assert!(
        !lender
            .asked
            .lock()
            .unwrap()
            .contains(&crate::obo_gateway::GitForgeAttributionRequest::Person),
        "an explicit bot request does not probe the person"
    );
}

/// A transient forge failure starts nothing so the adapter can retry.
#[tokio::test]
async fn an_unavailable_forge_refuses_get_or_create_with_no_binding() {
    let lender = ProbeFake::unavailable();
    let (router, runtime, repo_id, _dir) = machine_with_lender(lender).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let refused = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "external_key": "T1/C-id/4.4", "repo_id": repo_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "forge_unavailable");
    assert!(
        tidebreak_core::db::code::get_external_binding(&runtime.db, &owner, "slack", "T1/C-id/4.4")
            .await
            .unwrap()
            .is_none(),
        "an unavailable forge starts nothing"
    );
}

/// A service-owned grant always acts as the bot, whatever the body says.
#[tokio::test]
async fn a_service_owned_grant_acts_as_the_bot_regardless_of_the_body() {
    let lender = ProbeFake::person_connected("mira");
    let tokens = crate::auth::TokenMap::parse(
        "admin aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa admin\n\
         bot-svc bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb service\n",
    )
    .unwrap();
    let service = crate::principal::Principal::User {
        id: crate::principal::UserId::new("bot-svc").unwrap(),
        kind: crate::principal::PrincipalKind::Service,
        role: crate::principal::Role::Member,
    };
    let owner = service.owner_id();
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = tidebreak_harness::AdapterRegistry::new();
    registry.register(Arc::new(
        crate::scripted_harness::ScriptedAdapter::new(crate::scripted_harness::plain_text_script())
            .with_approvals(tidebreak_core::CapLevel::Supported)
            .with_plan_mode(tidebreak_core::CapLevel::Supported)
            .with_auto_mode(tidebreak_core::CapLevel::Supported)
            .with_allow_mode(tidebreak_core::CapLevel::Supported),
    ));
    let runtime = Arc::new(
        CodeRuntime::with_registry_and_browser_runtime(
            db,
            dir.path().to_path_buf(),
            registry,
            None,
            None,
        )
        .with_git_credentials(lender.clone()),
    );
    let root = super::code::init_git_repo(dir.path());
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: root.display().to_string(),
        display_name: "tools".into(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    insert_repo(&runtime.db, &repo).await.unwrap();
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
    state.principal_authenticator = Arc::new(crate::auth::PrincipalAuthenticator::Static(tokens));
    state.adapter_bootstrap_tokens = Some(Arc::new(crate::auth::AdapterBootstrapTokens::for_test(
        ADAPTER_BOOTSTRAP_TOKEN,
    )));
    let router = app(state);
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U-bot", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C-svc/1.1",
            "repo_id": repo.id,
            "acts_as": "person",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED,);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["acts_as"], "bot", "{body}");
    let session_id = bound_session_id(&runtime, &owner, "T1/C-svc/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(session.acts_as(), tidebreak_core::ActsAs::Bot);
    assert_eq!(session.owner_kind.as_deref(), Some("service"));
    assert!(
        !lender
            .asked
            .lock()
            .unwrap()
            .contains(&crate::obo_gateway::GitForgeAttributionRequest::Person),
        "a service owner never probes the person"
    );
}

/// A sandbox deployment runs channel sessions in `allow` because confinement
/// is that placement's boundary; a request for any other mode is refused
/// rather than approximated, and `allow` itself passes through.
#[tokio::test]
async fn a_sandbox_deployment_refuses_any_mode_but_allow() {
    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let refused = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C1/1.1",
            "repo_id": repo_id,
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_unsupported", "{body}");

    let allowed = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C1/1.1",
            "repo_id": repo_id,
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C1/1.1").await;
    let session = runtime.get_session(&owner, session_id).await.unwrap();
    assert_eq!(
        session.permission_mode,
        tidebreak_core::PermissionMode::Allow
    );
}

/// A forge that refuses every borrow with a reason, so the loopback route's
/// answer and its journal row can be pinned.
struct RefusingFake(crate::obo_gateway::GitForgeError);

#[async_trait::async_trait]
impl crate::obo_gateway::GitCredentialLender for RefusingFake {
    async fn git_forge_identity(
        &self,
        _owner: &OwnerId,
        _attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitForgeIdentity, crate::obo_gateway::GitForgeError> {
        Err(crate::obo_gateway::GitForgeError::NoGitForge)
    }

    async fn git_credential(
        &self,
        _owner: &OwnerId,
        _repository: &str,
        _attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<crate::obo_gateway::GitCredential, crate::obo_gateway::GitForgeError> {
        Err(match &self.0 {
            crate::obo_gateway::GitForgeError::SignInRequired(message) => {
                crate::obo_gateway::GitForgeError::SignInRequired(message.clone())
            }
            crate::obo_gateway::GitForgeError::NotConnected { connect_url } => {
                crate::obo_gateway::GitForgeError::NotConnected {
                    connect_url: connect_url.clone(),
                }
            }
            _ => crate::obo_gateway::GitForgeError::NoGitForge,
        })
    }

    async fn list_repositories(
        &self,
        _owner: &OwnerId,
        _attribution: crate::obo_gateway::GitForgeAttributionRequest,
    ) -> Result<Vec<crate::obo_gateway::GitHubRepository>, crate::obo_gateway::GitForgeError> {
        Ok(Vec::new())
    }
}

/// A refused borrow is said twice: the route answers the helper a status
/// and the reason, and the session's journal takes a `credential_refused`
/// row with the reason class and the remedy, so the desktop and the channel
/// can show why a push stopped. A dead sign-in reads as the connection
/// having ended; a forge with no identity for the person reads as not
/// connected; anything else reads as the forge refusing.
#[tokio::test]
async fn a_refused_borrow_answers_the_helper_and_journals_the_reason() {
    for (refusal, status, reason) in [
        (
            crate::obo_gateway::GitForgeError::SignInRequired("sign in again".to_owned()),
            reqwest::StatusCode::UNAUTHORIZED,
            "connection_ended",
        ),
        (
            crate::obo_gateway::GitForgeError::NotConnected {
                connect_url: Some("https://gateway.example/account/apps".to_owned()),
            },
            reqwest::StatusCode::FORBIDDEN,
            "not_connected",
        ),
        (
            crate::obo_gateway::GitForgeError::NoGitForge,
            reqwest::StatusCode::BAD_GATEWAY,
            "forge_refused",
        ),
    ] {
        let lender = Arc::new(RefusingFake(refusal));
        let gateway = Arc::new(
            crate::obo_gateway::OboGateway::new(
                "https://gateway.example",
                "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed"
                    .to_owned(),
            )
            .unwrap(),
        );
        let relay = Arc::new(crate::code::harness_llm::HarnessLlmRelay::new(gateway));
        let (router, _fake, runtime, repo_id, _token, _dir) = external_app_built({
            let lender = lender.clone();
            let relay = relay.clone();
            move |runtime| runtime.with_git_credentials(lender).with_harness_llm(relay)
        })
        .await;
        let addr = serve(router).await;
        let client = reqwest::Client::new();
        let owner = OwnerId::local();
        let (_grant, pair) = runtime
            .mint_adapter_grant(&owner, "slack", "U1", "T1")
            .await
            .unwrap();
        let created = client
            .post(format!("http://{addr}/external/code/sessions"))
            .bearer_auth(&pair.token)
            .json(&serde_json::json!({ "external_key": "T1/C7/1.1", "repo_id": repo_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), reqwest::StatusCode::CREATED);
        let session_id = bound_session_id(&runtime, &owner, "T1/C7/1.1").await;
        let key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
            owner: owner.clone(),
            session: session_id,
        });
        let refused = client
            .post(format!(
                "http://{addr}{}",
                crate::code::harness_llm::GIT_CREDENTIAL_PATH
            ))
            .bearer_auth(&key)
            .body("protocol=https\nhost=github.com\npath=acme/tools.git\n")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), status, "{reason}");
        let answer = refused.text().await.unwrap();
        assert!(
            !answer.is_empty(),
            "the helper gets the reason for {reason}"
        );
        assert!(
            !answer.contains("username="),
            "a refusal lends nothing: {answer}"
        );

        let page = tidebreak_core::db::code::list_events(
            &runtime.db,
            &owner,
            session_id,
            0,
            tidebreak_core::db::code::MAX_REPLAY_EVENTS,
        )
        .await
        .unwrap();
        let row = page
            .events
            .iter()
            .find_map(|sequenced| match &sequenced.event {
                tidebreak_core::Event::CredentialRefused {
                    reason,
                    message,
                    remediation,
                } => Some((*reason, message.clone(), remediation.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a credential_refused row for {reason}"));
        assert_eq!(serde_json::to_value(row.0).unwrap(), reason);
        assert_eq!(
            row.1,
            answer.trim_end(),
            "the row carries the helper's reason"
        );
        assert!(!row.2.is_empty(), "the row names a remedy");
    }
}

async fn workspace_grant_app() -> (Router, Arc<CodeRuntime>, RepoId, OwnerId, tempfile::TempDir) {
    let (dir, store) = temp_db_store("workspace-grant.db").await;
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
    let service = OwnerId::new("user:channel").unwrap();
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: service.clone(),
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
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\nchannel {CAROL_TOKEN} service\n"),
    )
    .unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let mut state = AppState::new(
        config,
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
    (app(state), runtime, repo.id, service, dir)
}

async fn call_json(
    router: &Router,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

#[tokio::test]
async fn a_service_principal_starts_a_workspace_handshake_and_an_admin_approves_it() {
    let (router, runtime, repo_id, service, _dir) = workspace_grant_app().await;

    let (status, _) = call_json(
        &router,
        "POST",
        "/code/grants/workspace",
        ALICE_TOKEN,
        Some(serde_json::json!({
            "channel_kind": "slack",
            "workspace_identity": "T1",
            "display": "Acme Corp",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, started) = call_json(
        &router,
        "POST",
        "/code/grants/workspace",
        CAROL_TOKEN,
        Some(serde_json::json!({
            "channel_kind": "slack",
            "workspace_identity": "T1",
            "display": "Acme Corp",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let nonce = started["nonce"].as_str().unwrap();
    let confirmation_token = started["confirmation_token"].as_str().unwrap();
    let handshake_id = started["id"].as_str().unwrap();

    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/deployment/code/grants/workspace/{handshake_id}/approve"),
        BOB_TOKEN,
        Some(serde_json::json!({ "csrf": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, page) = call_json(
        &router,
        "GET",
        &format!("/deployment/code/grants/workspace/{handshake_id}"),
        ALICE_TOKEN,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let csrf = page["csrf"].as_str().unwrap();
    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/deployment/code/grants/workspace/{handshake_id}/approve"),
        ALICE_TOKEN,
        Some(serde_json::json!({ "csrf": csrf })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, status_body) = call_json(
        &router,
        "GET",
        &format!("/external/connect/{nonce}/status"),
        confirmation_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(status_body["state"], "approved");

    let (status, completed) = call_json(
        &router,
        "POST",
        &format!("/external/connect/{nonce}/complete"),
        confirmation_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(completed["grant"]["kind"], "workspace");
    let grant_token = completed["token"].as_str().unwrap().to_owned();
    let grant_id = completed["grant"]["id"].as_str().unwrap().to_owned();

    let (status, body) = call_json(
        &router,
        "POST",
        "/external/code/sessions",
        &grant_token,
        Some(serde_json::json!({
            "external_key": "T1/C1/1.1",
            "repo_id": repo_id,
            "channel_id": "C1",
            "set_by": { "identity": "U1", "display": "Casey" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["kind"], "repository_unconfirmed");

    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/deployment/code/grants/workspace/{grant_id}/channels/C1/repositories/confirm"),
        ALICE_TOKEN,
        Some(serde_json::json!({ "repository": "acme/tools" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call_json(
        &router,
        "POST",
        "/external/code/sessions",
        &grant_token,
        Some(serde_json::json!({
            "external_key": "T1/C1/1.1",
            "repo_id": repo_id,
            "channel_id": "C1",
            "set_by": { "identity": "U1", "display": "Casey" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &service, "T1/C1/1.1").await;
    let session = runtime.get_session(&service, session_id).await.unwrap();
    assert_eq!(session.acts_as(), tidebreak_core::ActsAs::Bot);
    let bindings_url = format!("/external/code/sessions/{session_id}/bindings");
    let destination = serde_json::json!({"external_key":"T1/C2/2.2", "channel_id":"C2", "set_by":{"identity":"U1", "display":"Casey"}});
    let (status, _) = call_json(
        &router,
        "POST",
        &bindings_url,
        &grant_token,
        Some(destination.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/deployment/code/grants/workspace/{grant_id}/channels/C2/repositories/confirm"),
        ALICE_TOKEN,
        Some(serde_json::json!({"repository":"acme/tools"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call_json(
        &router,
        "POST",
        &bindings_url,
        &grant_token,
        Some(destination),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let binding =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &service, session_id)
            .await
            .unwrap()
            .into_iter()
            .find(|binding| binding.external_key == "T1/C1/1.1")
            .unwrap();
    let mut message = serde_json::json!({
        "text": "ship it",
        "event_id": "Ev-ws",
        "channel_ts": "1700000099.000100",
        "actor": { "external_identity": "U9", "display": "Ines" },
        "context": [{"author": "Casey", "timestamp": "1700000098.000100", "text": "The button fails on mobile."}],
    });
    let messages_url = format!("/external/code/sessions/{session_id}/messages");
    let (status, refused) = call_json(
        &router,
        "POST",
        &messages_url,
        &grant_token,
        Some(message.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["kind"], "context_not_allowed");
    message["context_opt_in"] = serde_json::json!(true);
    let (status, refused) = call_json(
        &router,
        "POST",
        &messages_url,
        &grant_token,
        Some(message.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["kind"], "context_binding_required");
    message["context_binding_id"] = serde_json::json!(binding.id);
    let (status, named) = call_json(
        &router,
        "POST",
        &messages_url,
        &grant_token,
        Some(message.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(named["outcome"], "new_turn");
    let turn_id: tidebreak_core::TurnId = serde_json::from_value(named["turn_id"].clone()).unwrap();
    let turn = tidebreak_core::db::code::get_turn(&runtime.db, &service, turn_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        turn.actor
            .as_ref()
            .and_then(|actor| actor.external_identity.as_deref()),
        Some("U9")
    );
    assert_eq!(
        turn.actor
            .as_ref()
            .and_then(|actor| actor.display.as_deref()),
        Some("Ines")
    );
    assert!(turn.user_input.contains("Untrusted thread context"));
    assert!(turn.user_input.contains("The button fails on mobile."));
    assert!(turn.user_input.ends_with("Current request:\nship it"));
    assert!(
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &service, session_id)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == binding.id)
            .unwrap()
            .context_opt_in
    );
    let (status, replay) = call_json(
        &router,
        "POST",
        &messages_url,
        &grant_token,
        Some(message.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["turn_id"], named["turn_id"]);
    message["event_id"] = serde_json::json!("Ev-ws-later");
    let (status, refused) =
        call_json(&router, "POST", &messages_url, &grant_token, Some(message)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["kind"], "context_first_turn_only");

    let (status, _) = call_json(
        &router,
        "PUT",
        &format!("/external/code/sessions/{session_id}/access"),
        &grant_token,
        Some(serde_json::json!({
            "contributors": [
                { "external_identity": "U9" },
                { "external_identity": "U8" }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rows = tidebreak_core::db::code::list_session_access(&runtime.db, &service, session_id)
        .await
        .unwrap();
    let subjects: Vec<_> = rows.iter().map(|row| row.subject.as_str()).collect();
    assert!(subjects.contains(&"external:slack:U9"));
    assert!(subjects.contains(&"external:slack:U8"));

    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/code/grants/{grant_id}/revoke"),
        ALICE_TOKEN,
        Some(serde_json::json!({ "reason": "cut the workspace" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/external/code/sessions/{session_id}/messages"),
        &grant_token,
        Some(serde_json::json!({
            "text": "still there",
            "event_id": "Ev-after",
            "channel_ts": "1700000100.000100",
            "actor": { "external_identity": "U9", "display": "Ines" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_person_grant_refuses_a_body_actor() {
    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let (status, _) = call_json(
        &router,
        "POST",
        "/external/code/sessions",
        &pair.token,
        Some(serde_json::json!({ "external_key": "T1/C-actor/1.1", "repo_id": repo_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C-actor/1.1").await;
    let (status, _) = call_json(
        &router,
        "POST",
        &format!("/external/code/sessions/{session_id}/messages"),
        &pair.token,
        Some(serde_json::json!({
            "text": "nope",
            "event_id": "Ev-actor",
            "channel_ts": "1700000111.000100",
            "actor": { "external_identity": "U9", "display": "Ines" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn external_context_refuses_person_grants_even_with_opt_in() {
    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    let (status, _) = call_json(
        &router,
        "POST",
        "/external/code/sessions",
        &pair.token,
        Some(serde_json::json!({"external_key": "T1/D-context/1.1", "repo_id": repo_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/D-context/1.1").await;
    let binding =
        tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &owner, session_id)
            .await
            .unwrap()
            .remove(0);
    let (status, refused) = call_json(
        &router,
        "POST",
        &format!("/external/code/sessions/{session_id}/messages"),
        &pair.token,
        Some(serde_json::json!({
            "text": "Fix it", "event_id": "Ev-context", "channel_ts": "1.1",
            "context_opt_in": true, "context_binding_id": binding.id,
            "context": [{"author":"Casey", "timestamp":"1.0", "text":"Bug report"}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["kind"], "context_not_allowed");
    assert!(
        !tidebreak_core::db::code::list_bindings_for_session(&runtime.db, &owner, session_id)
            .await
            .unwrap()[0]
            .context_opt_in
    );
    assert!(
        tidebreak_core::db::code::list_queued_turns(&runtime.db, &owner, session_id)
            .await
            .unwrap()
            .is_empty()
    );
}

fn long_command_approval_script() -> Vec<tidebreak_harness::HarnessEvent> {
    use tidebreak_harness::{HarnessApprovalRef, HarnessEvent};
    let cmd = "x".repeat(600);
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: "scripted".into(),
            resume_ref: Some("scripted-session".into()),
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef::engine("toolu_scripted"),
            raw: serde_json::json!({}),
            kind: Some(tidebreak_core::ApprovalKind::Command {
                cmd,
                cwd: Some(".".into()),
            }),
        },
        HarnessEvent::AssistantDelta {
            text: "after the decision".into(),
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

/// A Default-mode machine session parks on a command; the external stream
/// carries the card facts with a bounded preview; a contributor settles it
/// and the journal names them; a second decision answers already_settled;
/// standing grants stay off for this engine; a sandbox session emits none.
#[tokio::test]
async fn a_machine_session_parks_on_an_approval_a_contributor_can_settle() {
    use std::time::Duration;
    use tidebreak_core::{ApprovalState, HarnessKind};

    let (router, runtime, repo_id, _dir) = machine_app_built(|mut runtime| {
        runtime.adapters.register(Arc::new(
            crate::scripted_harness::ScriptedAdapter::new(long_command_approval_script())
                .with_kind(HarnessKind::ClaudeCode)
                .with_approvals(tidebreak_core::CapLevel::Supported)
                .with_plan_mode(tidebreak_core::CapLevel::Supported)
                .with_auto_mode(tidebreak_core::CapLevel::Supported)
                .with_allow_mode(tidebreak_core::CapLevel::Supported),
        ));
        runtime
    })
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U7", "T1")
        .await
        .unwrap();

    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C-approve/1.1",
            "repo_id": repo_id,
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C-approve/1.1").await;

    let delivered = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "run a command",
            "event_id": "Ev-approve",
            "channel_ts": "1.1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(delivered.status(), reqwest::StatusCode::OK);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let approval = loop {
        let pending = runtime
            .list_approvals(&owner, Some(ApprovalState::Pending), Some(session_id))
            .await
            .unwrap();
        if let Some(row) = pending.into_iter().next() {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the machine session parks on an approval"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    let mut request = format!("ws://{addr}/external/code/sessions/{session_id}/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", pair.token).parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let mut saw_card = false;
    let ws_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !saw_card {
        let frame = tokio::time::timeout_at(ws_deadline, socket.next())
            .await
            .expect("the stream carries the approval card")
            .expect("the stream stays open")
            .unwrap();
        let Ok(text) = frame.to_text() else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        if value["event"]["type"] == "approval_requested" {
            let request = &value["event"]["request"];
            assert_eq!(request["kind"], "tool_use");
            assert_eq!(request["tool_name"], "exec");
            assert_eq!(request["preview_truncated"], true);
            let preview = request["preview"]["command"].as_str().unwrap();
            assert_eq!(preview.chars().count(), 512);
            assert!(
                request["grant_scopes"].is_null()
                    || request["grant_scopes"] == serde_json::json!([])
            );
            saw_card = true;
        }
    }

    let grant_refused = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/approvals/{}/decision",
            approval.id
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "decision": { "approve_with_grant": { "grant_index": 0 } },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        grant_refused.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let grant_body: serde_json::Value = grant_refused.json().await.unwrap();
    assert_eq!(grant_body["kind"], "standing_grants_unavailable");

    let decided = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/approvals/{}/decision",
            approval.id
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "decision": "approve",
            "actor": { "external_identity": "U7", "display": "Ada" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        decided.status(),
        reqwest::StatusCode::OK,
        "{}",
        decided.text().await.unwrap()
    );

    let page = tidebreak_core::db::code::list_events(
        &runtime.db,
        &owner,
        session_id,
        0,
        tidebreak_core::db::code::MAX_REPLAY_EVENTS,
    )
    .await
    .unwrap();
    let resolved = page
        .events
        .iter()
        .find_map(|sequenced| match &sequenced.event {
            tidebreak_core::Event::ApprovalResolved { actor, .. } => actor.clone(),
            _ => None,
        });
    let actor = resolved.expect("the journal names who decided");
    assert_eq!(actor.external_identity.as_deref(), Some("U7"));
    assert_eq!(actor.display.as_deref(), Some("Ada"));

    let second = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/questions/{}/decision",
            approval.id
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
    let second: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second["kind"], "already_settled");
    assert_eq!(second["actor"]["external_identity"], "U7");
    assert_eq!(second["actor"]["display"], "Ada");

    let (router, _fake, runtime, repo_id, _dir) = external_app().await;
    let addr = serve(router).await;
    let (_grant, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U7", "T1")
        .await
        .unwrap();
    let created = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "external_key": "T1/C-sandbox/1.1",
            "repo_id": repo_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session_id = bound_session_id(&runtime, &owner, "T1/C-sandbox/1.1").await;
    let _ = client
        .post(format!(
            "http://{addr}/external/code/sessions/{session_id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({
            "text": "run a command",
            "event_id": "Ev-sandbox",
            "channel_ts": "1.1",
        }))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let page = tidebreak_core::db::code::list_events(
        &runtime.db,
        &owner,
        session_id,
        0,
        tidebreak_core::db::code::MAX_REPLAY_EVENTS,
    )
    .await
    .unwrap();
    assert!(
        page.events.iter().all(|sequenced| {
            !matches!(
                sequenced.event,
                tidebreak_core::Event::ApprovalRequested { .. }
                    | tidebreak_core::Event::ApprovalResolved { .. }
            )
        }),
        "a sandbox session carries none of these cards"
    );
}

#[tokio::test]
async fn external_bindings_attach_idempotently_and_refuse_foreign_or_ended_targets() {
    let (router, fake, runtime, repo_id, token, _dir) = external_app_with_token().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let owner = OwnerId::local();
    let (_, pair) = runtime
        .mint_adapter_grant(&owner, "slack", "U-bindings", "T1")
        .await
        .unwrap();
    let (_, foreign) = runtime
        .mint_adapter_grant(&owner, "slack", "U-other", "T1")
        .await
        .unwrap();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({"external_key":"T1/C1/1.1", "repo_id":repo_id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id: tidebreak_core::SessionId =
        serde_json::from_value(created["session_id"].clone()).unwrap();
    let url = format!("http://{addr}/external/code/sessions/{id}/bindings");
    let body = serde_json::json!({"external_key":"T1/C2/2.2"});
    let request = || {
        client
            .post(&url)
            .bearer_auth(&pair.token)
            .json(&body)
            .send()
    };
    let (left, right) = tokio::join!(request(), request());
    let mut statuses = vec![
        left.unwrap().status().as_u16(),
        right.unwrap().status().as_u16(),
    ];
    statuses.sort();
    assert_eq!(statuses, vec![200, 201]);
    let bindings: serde_json::Value = client
        .get(&url)
        .bearer_auth(&pair.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bindings.as_array().unwrap().len(), 2);
    assert_eq!(bindings[0]["external_key"], "T1/C1/1.1");
    assert_eq!(bindings[1]["session_id"], created["session_id"]);
    let snapshot: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["external_origins"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["external_origin"], snapshot["external_origins"][0]);
    for request in [client.get(&url), client.post(&url).json(&body)] {
        assert_eq!(
            request
                .bearer_auth(&foreign.token)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    // A different grant can hold another session, but cannot capture its key.
    client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&foreign.token)
        .json(&serde_json::json!({"external_key":"T1/C3/3.3", "repo_id":repo_id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&pair.token)
            .json(&serde_json::json!({"external_key":"T1/C3/3.3"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    // The same grant cannot move a key away from another session.
    client
        .post(format!("http://{addr}/external/code/sessions"))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({"external_key":"T1/C5/5.5", "repo_id":repo_id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&pair.token)
            .json(&serde_json::json!({"external_key":"T1/C5/5.5"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let (_, other_owner) = runtime
        .mint_adapter_grant(
            &OwnerId::new("other-owner").unwrap(),
            "slack",
            "U-foreign-owner",
            "T1",
        )
        .await
        .unwrap();
    for request in [client.get(&url), client.post(&url).json(&body)] {
        assert_eq!(
            request
                .bearer_auth(&other_owner.token)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    for key in [
        String::new(),
        "   ".to_owned(),
        "bad\nkey".to_owned(),
        "x".repeat(1025),
    ] {
        let response = client
            .post(&url)
            .bearer_auth(&pair.token)
            .json(&serde_json::json!({"external_key":key}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["kind"],
            "invalid_external_key"
        );
    }
    for path in [
        format!(
            "/code/workspaces/{}/sessions",
            snapshot["workspace_id"].as_str().unwrap()
        ),
        format!("/code/sessions/{id}/debug"),
    ] {
        let response: serde_json::Value = client
            .get(format!("http://{addr}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let row = if response.is_array() {
            response
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == snapshot["id"])
                .unwrap()
        } else {
            &response["session"]
        };
        assert_eq!(row["external_origins"], snapshot["external_origins"]);
    }
    // Two readers see the same session and both origins before the same journal.
    let mut sockets = Vec::new();
    for _ in 0..2 {
        let mut request = format!("ws://{addr}/external/code/sessions/{id}/events")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", pair.token).parse().unwrap(),
        );
        let (mut socket, _) = connect_async(request).await.unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(
            value["snapshot"]["external_origins"],
            snapshot["external_origins"]
        );
        sockets.push(socket);
    }
    client
        .post(format!(
            "http://{addr}/external/code/sessions/{id}/messages"
        ))
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({"text":"hello", "event_id":"Ev-bindings", "channel_ts":"3.3"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fake.event_reads.lock().unwrap().push_back(SandboxEvents {
        sandbox_id: "sb-ext".to_owned(),
        state: SandboxState::Running,
        latest_event_seq: 1,
        events: vec![SandboxEvent {
            seq: 1,
            kind: "turn_started".to_owned(),
            payload: serde_json::json!({"turn":1}),
            created_at: String::new(),
        }],
    });
    let mut live = runtime.get_session(&owner, id).await.unwrap();
    runtime
        .remote_sessions()
        .unwrap()
        .driver(&runtime.db, runtime.bus.as_ref())
        .pump(&mut live, 0)
        .await
        .unwrap();
    let mut frames = Vec::new();
    for socket in &mut sockets {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        frames.push(serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap());
    }
    assert_eq!(frames[0], frames[1]);
    let mut session = runtime.get_session(&owner, id).await.unwrap();
    session.lifecycle = tidebreak_core::SessionLifecycle::Ended;
    assert!(
        tidebreak_core::db::code::save_session(&runtime.db, &session)
            .await
            .unwrap()
    );
    let ended = client
        .post(&url)
        .bearer_auth(&pair.token)
        .json(&serde_json::json!({"external_key":"T1/C4/4.4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        ended.json::<serde_json::Value>().await.unwrap()["kind"],
        "ended"
    );
}
