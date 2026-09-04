//! Memory route contracts: owner isolation, review lifecycle, and honest
//! capability responses.

use super::*;

use crate::principal::{AuthContext, Principal, Role, UserId};
use axum::http::Request;
use axum::response::Response;
use serde_json::json;
use std::path::Path;
use tidebreak_core::memory::{
    MemoryAuthor, MemoryBackend, MemoryKind, MemoryRecord, MemoryRecordId, MemoryScope,
    MemoryStatus,
};
use tidebreak_core::OwnerId;

/// State over one database, with memory routes bound to that same database
/// the way production boot binds them.
fn state_with_memory(
    config: Config,
    db: Arc<tidebreak_core::DbStore>,
    agent_config: AgentConfig,
) -> AppState {
    let mut state = AppState::new(
        config,
        db.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        agent_config,
    );
    state.memory = Some(db);
    state
}

async fn memory_app() -> (
    Router,
    Arc<str>,
    Arc<tidebreak_core::DbStore>,
    tempfile::TempDir,
) {
    let (dir, store) = temp_db_store("memory-routes.db").await;
    let db = Arc::new(store);
    let state = state_with_memory(
        Config::desktop(dir.path()),
        db.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    (app(state), token, db, dir)
}

fn user_record(id: MemoryRecordId) -> MemoryRecord {
    MemoryRecord {
        id,
        scope: MemoryScope::Personal,
        kind: MemoryKind::Lesson,
        status: MemoryStatus::Active,
        title: "When changing database migrations".to_owned(),
        body: "Run the migration chain test before publishing.".to_owned(),
        provenance: tidebreak_core::MemoryProvenance {
            author: MemoryAuthor::User,
            origin: Default::default(),
            evidence: Vec::new(),
        },
        links: Vec::new(),
        expires_at: None,
        superseded_by: None,
        observation_count: 0,
        revision: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

async fn request(router: &Router, bearer: &str, method: &str, uri: String) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn request_json(
    router: &Router,
    bearer: &str,
    method: &str,
    uri: String,
    body: serde_json::Value,
) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_ok(response: Response) -> serde_json::Value {
    let status = response.status();
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn memory_routes_isolate_records_by_owner() {
    let (_router, _token, store, _dir) = memory_app().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let id = MemoryRecordId::new();

    store.put(&alice, user_record(id)).await.unwrap();

    let bob_router = Router::new()
        .route("/memory/records", get(crate::routes::list_records))
        .route(
            "/memory/records/{id}",
            get(crate::routes::get_record).delete(crate::routes::delete_record),
        )
        .with_state(state_with_memory(
            Config::desktop(Path::new("/tmp/tidebreak-memory-test")),
            tidebreak_core::DbStore::connect("sqlite::memory:")
                .await
                .unwrap()
                .into(),
            AgentConfig::default(),
        ))
        .layer(axum::middleware::from_fn(
            |mut request: Request<Body>, next: axum::middleware::Next| async move {
                request.extensions_mut().insert(AuthContext {
                    principal: Principal::User {
                        id: UserId::new("bob").unwrap(),
                        role: Role::Member,
                    },
                    client_executor: false,
                });
                next.run(request).await
            },
        ));

    let listed =
        assert_ok(request(&bob_router, "ignored", "GET", "/memory/records".into()).await).await;
    assert_eq!(listed, json!([]));
    let detail = request(
        &bob_router,
        "ignored",
        "GET",
        format!("/memory/records/{id}"),
    )
    .await;
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn memory_routes_reject_bad_bodies_and_id_reuse() {
    let (router, token, _store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();

    let invalid = json!({
        "id": id,
        "kind": "lesson",
        "status": "active",
        "title": "",
        "body": "Run the migration chain test before publishing.",
        "author": "user"
    });
    let rejected = request_json(&router, &bearer, "POST", "/memory/records".into(), invalid).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let valid = json!({
        "id": id,
        "kind": "lesson",
        "status": "active",
        "title": "When changing database migrations",
        "body": "Run the migration chain test before publishing.",
        "author": "user"
    });
    let created = request_json(&router, &bearer, "POST", "/memory/records".into(), valid).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let replay = request_json(
        &router,
        &bearer,
        "POST",
        "/memory/records".into(),
        json!({
            "id": id,
            "kind": "lesson",
            "status": "active",
            "title": "Duplicate",
            "body": "Duplicate.",
            "author": "user"
        }),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn memory_routes_move_proposals_and_report_revision_conflicts() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();
    let mut record = user_record(id);
    record.status = MemoryStatus::Proposed;
    store.put(&OwnerId::local(), record).await.unwrap();

    let stale = json!({"expected_revision": 1, "status": "active"});
    let activation = request_json(
        &router,
        &bearer,
        "PUT",
        format!("/memory/records/{id}/status"),
        stale,
    )
    .await;
    assert_eq!(activation.status(), StatusCode::OK);
    let activated: serde_json::Value = json_body(activation).await;
    assert_eq!(activated["status"], json!("active"));
    assert_eq!(activated["revision"], json!(2));

    let conflict = json!({"expected_revision": 1, "status": "archived"});
    let stale_activation = request_json(
        &router,
        &bearer,
        "PUT",
        format!("/memory/records/{id}/status"),
        conflict,
    )
    .await;
    assert_eq!(stale_activation.status(), StatusCode::CONFLICT);
    let error: serde_json::Value = json_body(stale_activation).await;
    assert_eq!(error["kind"], json!("memory_revision_conflict"));
}

#[tokio::test]
async fn memory_search_digest_and_history_read_only_owner_rows() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();
    store.put(&OwnerId::local(), user_record(id)).await.unwrap();

    let search = assert_ok(
        request(
            &router,
            &bearer,
            "GET",
            "/memory/search?query=migration&limit=10".into(),
        )
        .await,
    )
    .await;
    assert_eq!(search[0]["record_id"], json!(id));

    let digest = assert_ok(request(&router, &bearer, "GET", "/memory/digest".into()).await).await;
    assert_eq!(digest["record_count"], json!(1));
    assert!(digest["markdown"]
        .as_str()
        .unwrap()
        .contains("When changing database migrations"));

    let history = assert_ok(
        request(
            &router,
            &bearer,
            "GET",
            format!("/memory/records/{id}/revisions"),
        )
        .await,
    )
    .await;
    assert_eq!(history[0]["snapshot"]["id"], json!(id));
}

#[tokio::test]
async fn memory_ingest_answers_not_implemented_for_the_default_backend() {
    let (router, token, _store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let response = request_json(
        &router,
        &bearer,
        "POST",
        "/memory/ingest".into(),
        json!({
            "scope": {"kind": "personal"},
            "content": "Always run the release smoke test."
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let error: serde_json::Value = json_body(response).await;
    assert_eq!(error["kind"], json!("not_implemented"));
}

#[tokio::test]
async fn memory_sweep_status_serves_the_recorded_last_run() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");

    let before = assert_ok(request(&router, &bearer, "GET", "/memory/sweep".into()).await).await;
    assert_eq!(before["last_run"], json!(null));

    let run = tidebreak_core::MemorySweepRun {
        ran_at: chrono::Utc::now(),
        scope: Some(MemoryScope::Personal),
        outcome: tidebreak_core::MemorySweepOutcome::NoModel,
        expired: 2,
        proposed: 0,
    };
    tidebreak_core::db::memory_sweep::record_sweep_run(&store, &OwnerId::local(), &run)
        .await
        .unwrap();

    let after = assert_ok(request(&router, &bearer, "GET", "/memory/sweep".into()).await).await;
    assert_eq!(after["last_run"]["outcome"], json!("no_model"));
    assert_eq!(after["last_run"]["expired"], json!(2));
    assert_eq!(after["last_run"]["scope"]["kind"], json!("personal"));
}

// ---------------------------------------------------------------------------
// Work-mode injection, the explicit tool, and post-turn capture (#2554).
// ---------------------------------------------------------------------------

use crate::engine;
use crate::memory_capture::{MemoryCandidate, MemoryCapture, Outcome as CaptureOutcome};
use crate::memory_tool::MemoryTool;
use tidebreak_core::{MemoryEvidence, MemoryListFilter, Tool, ToolCtx};

/// Answers every turn with one line and records each request's composed
/// system prompt, which is what the fingerprint assertions read.
#[derive(Clone, Default)]
struct SystemPromptRecorder {
    prompts: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for SystemPromptRecorder {
    fn id(&self) -> ProviderId {
        ProviderId::new("system-prompt-recorder")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.prompts
            .lock()
            .unwrap()
            .push(request.system.unwrap_or_default());
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "recorded".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// State over one database with the memory backend bound the way production
/// boot binds it, and a turn worker carrying the memory handle the way
/// production assembly attaches it.
async fn memory_turn_app(
    provider: Arc<dyn ModelProvider>,
) -> (
    Router,
    Arc<str>,
    Arc<tidebreak_core::DbStore>,
    Arc<dyn Store>,
    AppState,
    tempfile::TempDir,
) {
    let (dir, store) = temp_db_store("memory-turns.db").await;
    let db = Arc::new(store);
    db.set_setting(crate::routes::MEMORY_ENABLED_SETTING, &json!(true))
        .await
        .unwrap();
    let store: Arc<dyn Store> = db.clone();
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.memory = Some(db.clone());
    let token = state.token.clone();
    let worker = engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        engine::internal::leg::LegDriverConfig::default(),
    )
    .with_memory(db.clone(), test_capture(&state, db.clone()));
    tokio::spawn(worker.run());
    (app(state.clone()), token, db, store, state, dir)
}

fn test_capture(state: &AppState, db: Arc<tidebreak_core::DbStore>) -> MemoryCapture {
    MemoryCapture::new(
        state.store.clone(),
        db,
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.events.clone(),
    )
}

fn active_titled(id: MemoryRecordId, title: &str) -> MemoryRecord {
    let mut record = user_record(id);
    record.title = title.to_owned();
    record
}

async fn wait_for_turns(store: &Arc<dyn Store>, chat: SessionId, terminal: usize) {
    for _ in 0..500 {
        let events = store.list_events(chat, 0).await.unwrap();
        let finished = events
            .iter()
            .filter(|event| {
                matches!(
                    event.event,
                    AgentEvent::TurnCompleted { .. }
                        | AgentEvent::TurnRefused { .. }
                        | AgentEvent::TurnFailed { .. }
                        | AgentEvent::TurnCancelled { .. }
                )
            })
            .count();
        if finished >= terminal {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("turn {terminal} did not finish within the timeout");
}

/// Decision 0068's cache invariant, the one test that matters most: change a
/// memory record mid-conversation and the next turn's composed prompt must be
/// byte-identical — an implementation that re-renders the digest per turn
/// passes every functional test and fails only here. After a boundary that
/// already rebuilds the prefix (a model switch), the new digest appears.
#[tokio::test(flavor = "multi_thread")]
async fn a_mid_conversation_memory_change_leaves_the_composed_prompt_byte_identical() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, _state, _dir) =
        memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let owner = OwnerId::local();
    db.put(
        &owner,
        active_titled(MemoryRecordId::new(), "When planning releases"),
    )
    .await
    .unwrap();

    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "turn one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;

    // The store changes mid-conversation: a create and a status change, both
    // through the backend the digest renders from.
    let mid = MemoryRecordId::new();
    db.put(&owner, active_titled(mid, "When picking a database"))
        .await
        .unwrap();
    let seeded = db.get(&owner, mid).await.unwrap().unwrap();
    db.set_status(
        &owner,
        tidebreak_core::MemoryStatusChange {
            id: mid,
            expected_revision: seeded.revision,
            status: MemoryStatus::Archived,
        },
    )
    .await
    .unwrap();
    db.put(
        &owner,
        active_titled(MemoryRecordId::new(), "When writing tests"),
    )
    .await
    .unwrap();

    assert_eq!(
        send_message(&router, &bearer, chat.id, "turn two").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 2).await;

    let prompts = recorder.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2, "each turn makes exactly one model call");
    assert!(prompts[0].contains("## Memory"), "{}", prompts[0]);
    assert!(prompts[0].contains("When planning releases"));
    assert!(!prompts[0].contains("When writing tests"));
    assert_eq!(
        prompts[0], prompts[1],
        "a mid-conversation memory change must not re-render the pinned digest"
    );

    // A model switch is a prefix-rebuilding boundary: the next turn's prompt
    // carries the digest as the store now stands.
    let patched = super::conversations::patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": "fake-boundary"}),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    assert_eq!(
        send_message(&router, &bearer, chat.id, "turn three").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 3).await;

    let prompts = recorder.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 3);
    assert_ne!(prompts[2], prompts[1]);
    assert!(prompts[2].contains("When writing tests"));
    assert!(!prompts[2].contains("When picking a database"));
}

/// Incognito is the whole opt-out: an incognito chat composes no memory
/// section at all, active records notwithstanding, and capture refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn an_incognito_chat_composes_no_memory_section_and_captures_nothing() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, state, _dir) = memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let owner = OwnerId::local();
    db.put(
        &owner,
        active_titled(MemoryRecordId::new(), "When planning releases"),
    )
    .await
    .unwrap();

    let created = request_json(
        &router,
        &bearer,
        "POST",
        "/chats".into(),
        json!({"memory_incognito": true}),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let chat: tidebreak_core::Chat = json_body(created).await;
    assert!(chat.memory_incognito);

    assert_eq!(
        send_message(&router, &bearer, chat.id, "turn one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let prompts = recorder.prompts.lock().unwrap().clone();
    assert!(
        !prompts[0].contains("## Memory"),
        "an incognito chat must not inject the digest: {}",
        prompts[0]
    );

    // Capture refuses the chat even with its switches on.
    store
        .set_setting(crate::routes::MEMORY_CAPTURE_ENABLED_SETTING, &json!(true))
        .await
        .unwrap();
    let turn_id = store.list_turns(chat.id).await.unwrap()[0].id;
    let capture = test_capture(&state, db.clone());
    assert_eq!(
        capture.derive(chat.id, turn_id).await.unwrap(),
        CaptureOutcome::NotApplicable
    );

    // And the toggle is a per-chat PATCH away, both directions.
    let patched = super::conversations::patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"memory_incognito": false}),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    let chat: tidebreak_core::Chat = json_body(patched).await;
    assert!(!chat.memory_incognito);
}

/// Capture stays off until its stored switch is turned on, whatever else is
/// configured — model-authored writes are opt-in (decision 0067).
#[tokio::test]
async fn capture_is_off_by_default() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, state, _dir) = memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "turn one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let turn_id = store.list_turns(chat.id).await.unwrap()[0].id;
    let capture = test_capture(&state, db);
    assert_eq!(
        capture.derive(chat.id, turn_id).await.unwrap(),
        CaptureOutcome::NotApplicable
    );
}

/// The storage tier under a capture candidate: a plain candidate lands as a
/// reviewable proposal; a hypothesis lands as `tracking` with one
/// observation; a repeat of a tracked title in the same chat only counts,
/// while a repeat from a distinct chat graduates it to review; and a title
/// the user already rejected is never re-proposed.
#[tokio::test(flavor = "multi_thread")]
async fn capture_candidates_land_under_the_review_thresholds() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, state, _dir) = memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let owner = OwnerId::local();
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "remember my preferences").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let turn_id = store.list_turns(chat.id).await.unwrap()[0].id;
    let user_message = store
        .list_messages(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|message| message.role == tidebreak_core::Role::User)
        .unwrap();
    let evidence = MemoryEvidence::Message {
        message_id: user_message.id,
    };
    let capture = test_capture(&state, db.clone());

    let candidate = |title: &str, hypothesis: bool| MemoryCandidate {
        kind: MemoryKind::Preference,
        title: title.to_owned(),
        body: "Use tables rather than prose.".to_owned(),
        hypothesis,
    };

    // A confident candidate is a proposal — never active.
    let outcome = capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence.clone(),
            candidate("When formatting reports", false),
        )
        .await
        .unwrap();
    let CaptureOutcome::Proposed(proposed_id) = outcome else {
        panic!("expected a proposal, got {outcome:?}");
    };
    let proposed = db.get(&owner, proposed_id).await.unwrap().unwrap();
    assert_eq!(proposed.status, MemoryStatus::Proposed);
    assert_eq!(proposed.provenance.author, MemoryAuthor::Model);
    assert_eq!(proposed.provenance.origin.turn_id, Some(turn_id));

    // A weak signal is tracked, not sent to review.
    let outcome = capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence.clone(),
            candidate("When naming branches", true),
        )
        .await
        .unwrap();
    let CaptureOutcome::Tracked(tracked_id) = outcome else {
        panic!("expected a tracked hypothesis, got {outcome:?}");
    };
    let tracked = db.get(&owner, tracked_id).await.unwrap().unwrap();
    assert_eq!(tracked.status, MemoryStatus::Tracking);
    assert_eq!(tracked.observation_count, 1);
    // Tracked hypotheses never reach the digest (decision 0067).
    let digest = db
        .assemble_context(&owner, MemoryScope::Personal)
        .await
        .unwrap();
    assert!(!digest.markdown.contains("When naming branches"));

    // A repeat in the same conversation only counts.
    let outcome = capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence.clone(),
            candidate("When naming branches", true),
        )
        .await
        .unwrap();
    assert_eq!(outcome, CaptureOutcome::Tracked(tracked_id));
    let tracked = db.get(&owner, tracked_id).await.unwrap().unwrap();
    assert_eq!(tracked.status, MemoryStatus::Tracking);
    assert_eq!(tracked.observation_count, 2);

    // A repeat across distinct conversations graduates it to review.
    let other_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, other_chat.id, "branch names again").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, other_chat.id, 1).await;
    let other_turn = store.list_turns(other_chat.id).await.unwrap()[0].id;
    let other_evidence = MemoryEvidence::Message {
        message_id: store
            .list_messages(other_chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|message| message.role == tidebreak_core::Role::User)
            .unwrap()
            .id,
    };
    let outcome = capture
        .store_candidate(
            &owner,
            other_chat.id,
            other_turn,
            other_evidence.clone(),
            candidate("When naming branches", true),
        )
        .await
        .unwrap();
    assert_eq!(outcome, CaptureOutcome::Graduated(tracked_id));
    let graduated = db.get(&owner, tracked_id).await.unwrap().unwrap();
    assert_eq!(graduated.status, MemoryStatus::Proposed);
    assert_eq!(graduated.observation_count, 3);
    // The proposal is announced on the graduating conversation, so that is
    // where its origin points and where the transcript attaches it.
    assert_eq!(graduated.provenance.origin.chat_id, Some(other_chat.id));
    assert_eq!(graduated.provenance.origin.turn_id, Some(other_turn));
    assert!(
        graduated.provenance.evidence.contains(&other_evidence),
        "the graduating turn joins the evidence"
    );
    assert_eq!(
        graduated.provenance.evidence.len(),
        2,
        "the first sighting stays"
    );

    // A dismissed proposal suppresses re-capture for its retention horizon.
    let rejected = db.get(&owner, proposed_id).await.unwrap().unwrap();
    db.set_status(
        &owner,
        tidebreak_core::MemoryStatusChange {
            id: proposed_id,
            expected_revision: rejected.revision,
            status: MemoryStatus::Rejected,
        },
    )
    .await
    .unwrap();
    let outcome = capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence,
            candidate("when formatting reports", false),
        )
        .await
        .unwrap();
    assert_eq!(outcome, CaptureOutcome::Declined);
}

/// The explicit tool's three verbs against the real backend: a propose lands
/// as an evidence-backed proposal, search and read find it, and an incognito
/// chat refuses the verb outright.
#[tokio::test(flavor = "multi_thread")]
async fn the_memory_tool_proposes_searches_and_reads() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, _state, _dir) =
        memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let owner = OwnerId::local();
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "I prefer tables in reports").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;

    let tool = MemoryTool::new(db.clone(), store.clone());
    let scratch = tempfile::tempdir().unwrap();
    let ctx = ToolCtx::new_legacy_workspace(chat.id, None, scratch.path().to_path_buf());

    let proposed = tool
        .execute(
            &ctx,
            json!({
                "verb": "propose",
                "kind": "preference",
                "title": "When formatting reports",
                "body": "Use tables rather than prose."
            }),
        )
        .await
        .unwrap();
    assert!(!proposed.is_error, "{}", proposed.content);
    assert!(proposed.content.contains("draft"));
    let records = db.list(&owner, MemoryListFilter::default()).await.unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.status, MemoryStatus::Proposed);
    assert_eq!(record.provenance.author, MemoryAuthor::Model);
    assert!(!record.provenance.evidence.is_empty());
    assert_eq!(record.provenance.origin.chat_id, Some(chat.id));
    assert!(record.provenance.origin.turn_id.is_some());

    // Search covers only authoritative records: the proposal is invisible
    // until the user activates it.
    let empty = tool
        .execute(&ctx, json!({"verb": "search", "query": "tables"}))
        .await
        .unwrap();
    assert!(empty.content.contains("No stored memory"));
    db.set_status(
        &owner,
        tidebreak_core::MemoryStatusChange {
            id: record.id,
            expected_revision: record.revision,
            status: MemoryStatus::Active,
        },
    )
    .await
    .unwrap();
    let found = tool
        .execute(&ctx, json!({"verb": "search", "query": "tables"}))
        .await
        .unwrap();
    assert!(
        found.content.contains("When formatting reports"),
        "{}",
        found.content
    );

    let read = tool
        .execute(
            &ctx,
            json!({"verb": "read", "record_id": record.id.to_string()}),
        )
        .await
        .unwrap();
    assert!(read.content.contains("Use tables rather than prose."));

    // A verb missing its field is corrected, not executed.
    let corrected = tool
        .execute(&ctx, json!({"verb": "propose", "kind": "fact"}))
        .await
        .unwrap();
    assert!(corrected.is_error);
    assert!(corrected.content.contains("title"));

    // An incognito chat refuses the verb even when called unadvertised.
    assert!(store
        .set_chat_memory_incognito(chat.id, true)
        .await
        .unwrap());
    let refused = tool
        .execute(&ctx, json!({"verb": "search", "query": "tables"}))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert!(
        refused.content.contains("memory off"),
        "{}",
        refused.content
    );
}

/// The transcript hands each terminal turn its model-authored records, so the
/// proposal chip survives reload; tracked hypotheses stay manager-only.
#[tokio::test(flavor = "multi_thread")]
async fn the_transcript_carries_each_turns_memory_proposals() {
    let recorder = SystemPromptRecorder::default();
    let (router, token, db, store, state, _dir) = memory_turn_app(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let owner = OwnerId::local();
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "remember this").await,
        StatusCode::ACCEPTED
    );
    wait_for_turns(&store, chat.id, 1).await;
    let turn_id = store.list_turns(chat.id).await.unwrap()[0].id;
    let user_message = store
        .list_messages(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|message| message.role == tidebreak_core::Role::User)
        .unwrap();
    let evidence = MemoryEvidence::Message {
        message_id: user_message.id,
    };
    let capture = test_capture(&state, db.clone());
    capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence.clone(),
            MemoryCandidate {
                kind: MemoryKind::Preference,
                title: "When formatting reports".to_owned(),
                body: "Use tables rather than prose.".to_owned(),
                hypothesis: false,
            },
        )
        .await
        .unwrap();
    capture
        .store_candidate(
            &owner,
            chat.id,
            turn_id,
            evidence,
            MemoryCandidate {
                kind: MemoryKind::Lesson,
                title: "When naming branches".to_owned(),
                body: "Short kebab case.".to_owned(),
                hypothesis: true,
            },
        )
        .await
        .unwrap();

    let transcript = assert_ok(
        request(
            &router,
            &bearer,
            "GET",
            format!("/chats/{}/messages", chat.id),
        )
        .await,
    )
    .await;
    let turns = transcript["terminal_turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1);
    let proposals = turns[0]["memory_proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 1, "hypotheses stay out of the transcript");
    assert_eq!(proposals[0]["title"], json!("When formatting reports"));
    assert_eq!(proposals[0]["status"], json!("proposed"));
}
