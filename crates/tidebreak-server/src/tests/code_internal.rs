//! The internal engine behind the adapter contract (decision 0048 step 5).
//!
//! A conversation with no workspace selects the in-process engine and runs
//! on the code structures: sessions, turns, the sequenced journal, and
//! approval rows. This module is the tripwire for that: chat's plan
//! proposals, user questions, tool approvals with grant ladders, and
//! mid-turn steering all have to be reachable over `/code/*`, and nothing
//! about the conversation may leak into the chat routes.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

use tidebreak_core::{
    ApprovalClass, ChatRequest, CodeSessionId, CodeTurnStatus, ContentBlock, DbStore,
    ModelProvider, OwnerId, ProviderEvent, ProviderId, StopReason, Store,
    SubmitAgentRunResultOutcome, Tool, ToolCtx, ToolOutput, ToolRegistry, ToolSpec, TurnId,
    TurnParkWait, TurnRunStatus,
};
use tidebreak_harness::AdapterRegistry;

use crate::code::CodeRuntime;
use crate::engine::internal::InternalAdapter;

/// One model completion the scripted provider answers with.
enum Step {
    Tool {
        name: &'static str,
        input: serde_json::Value,
    },
    WaitForSpawnedAgent,
    Text(&'static str),
}

/// A provider that replays one scripted completion per model call.
struct ScriptedProvider {
    steps: Mutex<Vec<Step>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted")
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
        let request_for_step = request.clone();
        self.requests.lock().unwrap().push(request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let step = {
            let mut steps = self.steps.lock().unwrap();
            if steps.is_empty() {
                None
            } else {
                Some(steps.remove(0))
            }
        };
        let events = match step {
            Some(Step::Tool { name, input }) => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("scripted_{call}"),
                    name: name.to_owned(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: input.to_string(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            Some(Step::WaitForSpawnedAgent) => {
                let agent_id = request_for_step
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|message| message.content.iter().rev())
                    .find_map(|block| match block {
                        ContentBlock::ToolResult { content, .. } => {
                            serde_json::from_str::<serde_json::Value>(content)
                                .ok()
                                .and_then(|value| value["agent_id"].as_str().map(str::to_owned))
                        }
                        _ => None,
                    })
                    .expect("the spawn result names its child");
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: format!("scripted_{call}"),
                        name: tidebreak_core::WAIT_FOR_AGENTS_TOOL.to_owned(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: serde_json::json!({ "agent_ids": [agent_id] }).to_string(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Step::Text(text)) => vec![
                ProviderEvent::TextDelta {
                    text: text.to_owned(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
            None => vec![
                ProviderEvent::TextDelta {
                    text: "the script ended".to_owned(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A sensitive `exec` stand-in: named so the server builds an exact action
/// preview and a grant ladder for it, without running anything.
struct FakeExec {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for FakeExec {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _args: serde_json::Value,
    ) -> tidebreak_core::Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("hi"))
    }
}

/// An app whose chat turn lane runs the scripted provider and whose code
/// runtime knows only the internal engine.
async fn internal_engine_app(
    steps: Vec<Step>,
) -> (
    std::net::SocketAddr,
    Arc<str>,
    Arc<CodeRuntime>,
    Arc<AtomicUsize>,
    tempfile::TempDir,
) {
    let (router, token, runtime, ran, dir, _provider, _state) =
        internal_engine_app_capturing(steps).await;
    let addr = super::code::serve(router).await;
    (addr, token, runtime, ran, dir)
}

async fn internal_engine_app_capturing(
    steps: Vec<Step>,
) -> (
    axum::Router,
    Arc<str>,
    Arc<CodeRuntime>,
    Arc<AtomicUsize>,
    tempfile::TempDir,
    Arc<ScriptedProvider>,
    AppState,
) {
    let (dir, store) = temp_db_store("internal.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(FakeExec { ran: ran.clone() }));
    tools.register_validated_foreground_client(
        tidebreak_core::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        tidebreak_core::validate_ask_user_questions_arguments,
    );
    tools.register_validated_foreground_client(
        tidebreak_core::exit_plan_mode_tool_spec(),
        ApprovalClass::ReadOnly,
        tidebreak_core::validate_exit_plan_mode_arguments,
    );
    tools.register_validated_foreground_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Connect one folder through the trusted client".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
        serde_json::Value::is_object,
    );
    tools.register_foreground_agent_orchestration();
    let provider = Arc::new(ScriptedProvider {
        steps: Mutex::new(steps),
        calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store_trait,
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "scripted".into(),
            ..AgentConfig::default()
        },
    );
    spawn_turn_worker_with_blobs(&state);
    let mut runtime =
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), AdapterRegistry::new());
    // As in `lib.rs`: the engine follows the session's journal on the
    // runtime's store and bus, and the chat bus mirrors onto that bus.
    runtime.adapters.register(Arc::new(InternalAdapter::new(
        state.clone(),
        runtime.db.clone(),
        runtime.bus.clone(),
    )));
    state.events.mirror_into(runtime.bus.clone());
    let runtime = Arc::new(runtime);
    state.code = Some(runtime.clone());
    let token = state.token.clone();
    (
        app(state.clone()),
        token,
        runtime,
        ran,
        dir,
        provider,
        state,
    )
}

fn spawn_turn_worker_with_blobs(state: &AppState) {
    let worker = crate::engine::internal::leg::LegDriver::new(
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
        crate::engine::internal::leg::LegDriverConfig::default(),
    )
    .with_blobs(state.blobs.clone());
    tokio::spawn(worker.run());
}

async fn pending_approval_of_kind(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: CodeSessionId,
    kind: &str,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let approvals: Vec<serde_json::Value> = client
                .get(format!(
                    "http://{addr}/code/approvals?session_id={session_id}&state=pending"
                ))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(approval) = approvals
                .into_iter()
                .find(|approval| approval["kind"]["type"] == kind)
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("a pending {kind} approval appears"))
}

/// Poll until the session's turns read exactly `expected`, or fail.
async fn wait_for_turn_statuses(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: CodeSessionId,
    expected: &[&str],
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let statuses = turn_statuses(client, addr, token, session_id).await;
        if statuses == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "turn statuses stayed {statuses:?}, expected {expected:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn turn_statuses(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: CodeSessionId,
) -> Vec<String> {
    let turns: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    turns
        .iter()
        .map(|turn| turn["status"].as_str().unwrap().to_owned())
        .collect()
}

async fn decide(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    approval_id: &str,
    body: serde_json::Value,
) {
    let response = client
        .post(format!(
            "http://{addr}/code/approvals/{approval_id}/decision"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{text}");
}

fn event_types(events: &[tidebreak_core::SequencedCodeEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            serde_json::to_value(&event.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn position(types: &[String], wanted: &str, after: usize) -> usize {
    types
        .iter()
        .enumerate()
        .skip(after)
        .find(|(_, kind)| kind.as_str() == wanted)
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("{wanted} after {after} in {types:?}"))
}

async fn create_internal_session(
    router: &axum::Router,
    bearer: &str,
    permission_mode: &str,
) -> CodeSessionId {
    let created = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/code/sessions")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "permission_mode": permission_mode }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let session: serde_json::Value = super::json_body(created).await;
    session["id"].as_str().unwrap().parse().unwrap()
}

fn submit_internal_turn(
    router: &axum::Router,
    bearer: &str,
    session_id: CodeSessionId,
    message: &str,
) -> tokio::task::JoinHandle<axum::response::Response> {
    let router = router.clone();
    let bearer = bearer.to_owned();
    let message = message.to_owned();
    tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/code/sessions/{session_id}/turns"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "message": message }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    })
}

async fn wait_for_durable_park(
    runtime: &CodeRuntime,
    session_id: CodeSessionId,
) -> tidebreak_core::CodeTurn {
    let parked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(turn) =
                tidebreak_core::db::code::get_open_turn(&runtime.db, &OwnerId::local(), session_id)
                    .await
                    .unwrap()
            {
                if turn.status == CodeTurnStatus::Waiting
                    && turn.park_ref.is_some()
                    && turn.park_wait.is_some()
                {
                    break turn;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    match parked {
        Ok(turn) => turn,
        Err(_) => {
            let turns =
                tidebreak_core::db::code::list_turns(&runtime.db, &OwnerId::local(), session_id)
                    .await
                    .unwrap();
            let runs = runtime
                .db
                .list_agent_runs(tidebreak_core::ChatId(session_id.0))
                .await
                .unwrap();
            let events = tidebreak_core::db::code::list_recent_events(
                &runtime.db,
                &OwnerId::local(),
                session_id,
                32,
            )
            .await
            .unwrap();
            panic!(
                "the internal turn did not store its adapter park; turns={turns:?} runs={runs:?} events={events:?}"
            );
        }
    }
}

/// The whole chat interaction model, reached only through `/code/*`: a plan
/// proposal parks the turn as an approval and its acceptance re-postures
/// the session; a questions card parks and its answers resume; a sensitive
/// tool waits on a structured approval that offers a grant ladder; a steer
/// lands mid-turn; and the turn completes on the sequenced journal.
#[tokio::test]
async fn a_conversation_without_a_workspace_runs_on_the_code_wire() {
    let (addr, token, runtime, ran, _dir) = internal_engine_app(vec![
        Step::Tool {
            name: "exit_plan_mode",
            input: serde_json::json!({
                "title": "Ship the greeting",
                "plan": "1. Run the greeting command.\n2. Ask which greeting to use.\n3. Report back.",
            }),
        },
        Step::Tool {
            name: "ask_user_questions",
            input: serde_json::json!({
                "questions": [{
                    "id": "greeting",
                    "header": "Greeting",
                    "question": "Which greeting?",
                    "options": [
                        {"id": "hi", "label": "hi", "description": "short"},
                        {"id": "hello", "label": "hello", "description": "longer"}
                    ]
                }]
            }),
        },
        Step::Tool {
            name: "exec",
            input: serde_json::json!({"command": "echo", "args": ["hi"]}),
        },
        Step::Text("all done"),
    ])
    .await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "plan" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert_eq!(session["harness_kind"], "internal");
    assert!(session["workspace_id"].is_null(), "{session}");
    let session_id: CodeSessionId = session["id"].as_str().unwrap().parse().unwrap();

    // The session row is the conversation row: the chat routes read the
    // same row by the same id.
    let listed = client
        .get(format!("http://{addr}/chats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = listed.status();
    let body = listed.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    let chats: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(chats.len(), 1, "the session is the one chat: {chats:?}");
    assert_eq!(chats[0]["id"], session["id"]);
    let by_id = client
        .get(format!("http://{addr}/chats/{session_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_id.status(), reqwest::StatusCode::OK);
    let chat: serde_json::Value = by_id.json().await.unwrap();
    assert_eq!(chat["permission_mode"], "plan");

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "plan it, then greet" }))
                .send()
                .await
                .unwrap()
        }
    });

    // 1. The plan proposal is a parked approval.
    let plan = pending_approval_of_kind(&client, addr, &token, session_id, "plan").await;
    assert_eq!(plan["kind"]["proposed_mode"], "auto");
    let raw: serde_json::Value =
        serde_json::from_str(plan["harness_raw_json"].as_str().unwrap()).unwrap();
    assert_eq!(raw["title"], "Ship the greeting");
    // The approval is published before the worker persists the park, so
    // the row can still read `running` for a moment.
    wait_for_turn_statuses(&client, addr, &token, session_id, &["waiting"]).await;
    decide(
        &client,
        addr,
        &token,
        plan["id"].as_str().unwrap(),
        serde_json::json!({ "decision": { "plan_decision": { "approve": true } } }),
    )
    .await;

    // 2. The questions card is the next park; answers resume it.
    let questions = pending_approval_of_kind(&client, addr, &token, session_id, "questions").await;
    assert_eq!(questions["kind"]["questions"][0]["id"], "greeting");
    // Accepting the plan moved the session out of plan mode before the
    // resume, so the questions could be asked at all.
    let snapshot: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["permission_mode"], "auto");
    let listed: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], session["id"]);
    decide(
        &client,
        addr,
        &token,
        questions["id"].as_str().unwrap(),
        serde_json::json!({ "decision": { "answers": { "answers": [
            { "question_id": "greeting", "selected_option_ids": ["hi"] }
        ] } } }),
    )
    .await;

    // 3. The sensitive tool waits on a structured approval with a ladder,
    //    on a turn that is running, not parked. A steer lands meanwhile.
    let tool = pending_approval_of_kind(&client, addr, &token, session_id, "tool_use").await;
    assert_eq!(tool["kind"]["preview"]["tool"], "exec");
    assert!(
        tool["kind"]["preview"]
            .get("summary")
            .is_none_or(serde_json::Value::is_null),
        "model narration never reaches the card: {tool}"
    );
    let offered = tool["kind"]["offered_grants"].as_array().unwrap();
    assert!(!offered.is_empty(), "exec offers a grant ladder: {tool}");
    let statuses = turn_statuses(&client, addr, &token, session_id).await;
    assert_eq!(statuses, vec!["running"]);
    let turn_id = {
        let turns: Vec<serde_json::Value> = client
            .get(format!("http://{addr}/code/sessions/{session_id}/turns"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        turns[0]["id"].as_str().unwrap().to_owned()
    };
    let steered = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": turn_id,
            "guidance": "and say thanks",
        }))
        .send()
        .await
        .unwrap();
    assert!(steered.status().is_success(), "{}", steered.status());
    decide(
        &client,
        addr,
        &token,
        tool["id"].as_str().unwrap(),
        serde_json::json!({ "decision": { "approve_with_grant": { "grant_index": 0 } } }),
    )
    .await;

    let response = tokio::time::timeout(Duration::from_secs(20), turn)
        .await
        .expect("the turn completes")
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(ran.load(Ordering::SeqCst), 1, "the approved tool ran once");
    assert_eq!(
        turn_statuses(&client, addr, &token, session_id).await,
        vec!["completed"]
    );

    // The journal is the one the code wire serves, in the code vocabulary,
    // and the lane wrote it: each park and the consent card is one approval
    // row with one request and one resolution, and the lane's own rows are
    // there once each.
    let events = super::code::journaled_events(&runtime.db, session_id).await;
    let types = event_types(&events);
    let started = position(&types, "turn_started", 0);
    let plan_requested = position(&types, "approval_requested", started);
    let plan_resolved = position(&types, "approval_resolved", plan_requested);
    let questions_requested = position(&types, "approval_requested", plan_resolved);
    let questions_resolved = position(&types, "approval_resolved", questions_requested);
    let tool_requested = position(&types, "approval_requested", questions_resolved);
    let tool_resolved = position(&types, "approval_resolved", tool_requested);
    let tool_completed = position(&types, "tool_completed", tool_resolved);
    let steered = position(&types, "user_steered", tool_requested);
    let completed = position(&types, "turn_completed", tool_completed);
    for (index, kind) in [
        (plan_requested, "plan"),
        (questions_requested, "questions"),
        (tool_requested, "tool_use"),
    ] {
        let request = serde_json::to_value(&events[index].event).unwrap();
        assert_eq!(request["request"]["kind"], kind, "{request}");
    }
    assert_eq!(
        types
            .iter()
            .filter(|kind| kind.as_str() == "approval_requested")
            .count(),
        3,
        "one request per card: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|kind| kind.as_str() == "approval_resolved")
            .count(),
        3,
        "one resolution per card: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|kind| kind.as_str() == "user_steered")
            .count(),
        1,
        "one steer, journaled once: {types:?}"
    );
    assert_eq!(completed + 1, types.len(), "{types:?}");
    assert!(steered < completed);
    // Three legs: the opening run, then one resume per park.
    assert_journaled_once(&events, 3);
    let plan_decision = serde_json::to_value(&events[plan_resolved].event).unwrap();
    assert_eq!(plan_decision["decision"]["type"], "plan_decided");
    let grant_decision = serde_json::to_value(&events[tool_resolved].event).unwrap();
    assert_eq!(grant_decision["decision"]["type"], "approved_with_grant");
    assert_eq!(streamed_text(&events), "all done");
    assert_chat_replay_is_the_journal(&runtime.db, session_id, &events).await;
}

/// Every fact the lane journals once: one `TurnStarted` per leg the lane
/// ran (a resumed park starts the stream again, and the chat surface has
/// always replayed that), one terminal row, and no `AssistantMessage`
/// beside the deltas that carry the answer (the whole message lives on the
/// transcript row, not a second journal row).
fn assert_journaled_once(events: &[tidebreak_core::SequencedCodeEvent], legs: usize) {
    let types = event_types(events);
    let count = |kind: &str| types.iter().filter(|entry| entry.as_str() == kind).count();
    assert_eq!(count("turn_started"), legs, "{types:?}");
    assert_eq!(count("turn_completed"), 1, "{types:?}");
    assert_eq!(
        count("turn_failed") + count("turn_interrupted"),
        0,
        "{types:?}"
    );
    assert_eq!(count("assistant_message"), 0, "{types:?}");
}

/// The assistant text a journal streams, concatenated.
fn streamed_text(events: &[tidebreak_core::SequencedCodeEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match &event.event {
            tidebreak_core::CodeEvent::AssistantDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The chat replay read is a projection of the same rows: every event it
/// serves sits at its own sequence in the code journal and is the chat
/// reading of exactly that row.
async fn assert_chat_replay_is_the_journal(
    db: &DbStore,
    session_id: CodeSessionId,
    journal: &[tidebreak_core::SequencedCodeEvent],
) {
    use tidebreak_core::Store as _;
    let replay = db
        .list_events(tidebreak_core::ChatId(session_id.0), 0)
        .await
        .unwrap();
    assert!(!replay.is_empty());
    for event in &replay {
        let row = journal
            .iter()
            .find(|row| row.seq == event.seq)
            .unwrap_or_else(|| panic!("chat seq {} has no journal row", event.seq));
        // The row can say more than its chat reading (a grant scope, denial
        // feedback); the reading is what the chat surface gets from it.
        assert_eq!(
            tidebreak_core::chat_journal::chat_event(row.event.clone()).unwrap(),
            Some(event.event.clone()),
            "chat seq {} replays a different row",
            event.seq
        );
    }
    let projected = journal
        .iter()
        .filter(|row| {
            tidebreak_core::chat_journal::chat_event(row.event.clone())
                .unwrap()
                .is_some()
        })
        .count();
    assert_eq!(
        replay.len(),
        projected,
        "the chat replay skips only code-only rows"
    );
}

/// A questions park answered through the chat route resumes the session
/// the runtime drives: the route settles the row (the answer carries
/// context the engine contract has no field for), and the worker resumes
/// the park from the settled row rather than waiting for a decision that
/// will never come through the session route.
#[tokio::test]
async fn a_questions_park_answered_from_the_chat_route_resumes_the_session() {
    let (addr, token, runtime, _ran, _dir) = internal_engine_app(vec![
        Step::Tool {
            name: "ask_user_questions",
            input: serde_json::json!({
                "questions": [{
                    "id": "greeting",
                    "header": "Greeting",
                    "question": "Which greeting?",
                    "options": [
                        {"id": "hi", "label": "hi", "description": "short"},
                        {"id": "hello", "label": "hello", "description": "longer"}
                    ]
                }]
            }),
        },
        Step::Text("greeted"),
    ])
    .await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "ask" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    let hosted: CodeSessionId = session["id"].as_str().unwrap().parse().unwrap();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{hosted}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "ask me" }))
                .send()
                .await
                .unwrap()
        }
    });
    let questions = pending_approval_of_kind(&client, addr, &token, hosted, "questions").await;
    wait_for_turn_statuses(&client, addr, &token, hosted, &["waiting"]).await;

    let answered = client
        .post(format!(
            "http://{addr}/chats/{hosted}/questions/{}/answer",
            questions["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "answers": [{ "question_id": "greeting", "selected_option_ids": ["hi"] }],
            "additional_user_context": "keep it short",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), reqwest::StatusCode::OK);

    let response = tokio::time::timeout(Duration::from_secs(20), turn)
        .await
        .expect("the turn completes")
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(
        turn_statuses(&client, addr, &token, hosted).await,
        vec!["completed"]
    );
    let events = super::code::journaled_events(&runtime.db, hosted).await;
    let types = event_types(&events);
    let requested = position(&types, "approval_requested", 0);
    let resolved = position(&types, "approval_resolved", requested);
    let completed = position(&types, "turn_completed", resolved);
    assert_eq!(completed + 1, types.len(), "{types:?}");
    let decision = serde_json::to_value(&events[resolved].event).unwrap();
    assert_eq!(decision["decision"]["type"], "answered");
}

/// A plan decided through the chat route resumes the session the runtime
/// drives, whichever path the decision takes: the default mode goes through
/// the session decision route as an alias and the worker applies the
/// proposed mode; a chosen mode settles the row directly, carries the mode
/// itself, and the worker resumes from the settled row.
#[tokio::test]
async fn a_plan_accepted_from_the_chat_route_resumes_the_session() {
    for (chosen_mode, expected_mode) in [(None, "auto"), (Some("ask"), "ask")] {
        let (addr, token, _runtime, _ran, _dir) = internal_engine_app(vec![
            Step::Tool {
                name: "exit_plan_mode",
                input: serde_json::json!({
                    "title": "Ship the greeting",
                    "plan": "1. Run the greeting command.\n2. Ask which greeting to use.\n3. Report back.",
                }),
            },
            Step::Text("planned"),
        ])
        .await;
        let client = reqwest::Client::new();
        let created = client
            .post(format!("http://{addr}/code/sessions"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "permission_mode": "plan" }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), reqwest::StatusCode::CREATED);
        let session: serde_json::Value = created.json().await.unwrap();
        let hosted: CodeSessionId = session["id"].as_str().unwrap().parse().unwrap();
        let turn = tokio::spawn({
            let client = client.clone();
            let token = token.clone();
            async move {
                client
                    .post(format!("http://{addr}/code/sessions/{hosted}/turns"))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({ "message": "plan it" }))
                    .send()
                    .await
                    .unwrap()
            }
        });
        let plan = pending_approval_of_kind(&client, addr, &token, hosted, "plan").await;
        wait_for_turn_statuses(&client, addr, &token, hosted, &["waiting"]).await;

        let mut body = serde_json::json!({ "decision": "accept" });
        if let Some(mode) = chosen_mode {
            body["permission_mode"] = serde_json::Value::String(mode.to_owned());
        }
        let decided = client
            .post(format!(
                "http://{addr}/chats/{hosted}/plans/{}/decision",
                plan["id"].as_str().unwrap()
            ))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(decided.status(), reqwest::StatusCode::OK, "{chosen_mode:?}");

        let response = tokio::time::timeout(Duration::from_secs(20), turn)
            .await
            .expect("the turn completes")
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        assert_eq!(
            turn_statuses(&client, addr, &token, hosted).await,
            vec!["completed"],
            "{chosen_mode:?}"
        );
        let snapshot: serde_json::Value = client
            .get(format!("http://{addr}/code/sessions/{hosted}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            snapshot["permission_mode"], expected_mode,
            "{chosen_mode:?}"
        );
        let chat: serde_json::Value = client
            .get(format!("http://{addr}/chats/{hosted}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(chat["permission_mode"], expected_mode, "{chosen_mode:?}");
    }
}

#[tokio::test]
async fn an_internal_client_wait_resumes_through_one_adapter_park() {
    let (router, token, runtime, _ran, _dir, provider, _state) =
        internal_engine_app_capturing(vec![
            Step::Tool {
                name: "connect_folder",
                input: serde_json::json!({ "suggested_name": "Documents" }),
            },
            Step::Text("folder connected"),
        ])
        .await;
    let bearer = format!("Bearer {token}");
    let session_id = create_internal_session(&router, &bearer, "ask").await;
    let response = submit_internal_turn(&router, &bearer, session_id, "connect documents");

    let parked = wait_for_durable_park(&runtime, session_id).await;
    let call_id = match parked.park_wait.as_ref().unwrap() {
        TurnParkWait::ClientToolCall { call_id } => call_id.clone(),
        other => panic!("the turn parked on the wrong dependency: {other:?}"),
    };
    assert_eq!(parked.park_ref.as_deref(), Some(call_id.as_str()));
    let call_id: tidebreak_core::CallId = call_id.parse().unwrap();
    let pending = runtime
        .db
        .list_pending_client_tool_calls(tidebreak_core::ChatId(session_id.0))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, call_id);
    assert_eq!(pending[0].name, "connect_folder");
    let before_resume = runtime
        .db
        .get_turn(TurnId(parked.id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (before_resume.attempt_count, before_resume.claim_count),
        (1, 1)
    );

    let lease_token = uuid::Uuid::new_v4();
    let claim = super::lifecycle::post_native_json(
        &router,
        &bearer,
        &format!("/chats/{session_id}/client-executions/{call_id}/claim"),
        serde_json::json!({
            "executor_id": uuid::Uuid::new_v4(),
            "lease_token": lease_token,
        }),
    )
    .await;
    assert_eq!(claim.status(), StatusCode::OK);
    let resolved = super::lifecycle::post_native_json(
        &router,
        &bearer,
        &format!("/chats/{session_id}/client-executions/{call_id}/resolve"),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {
                "status": "completed",
                "result": "connected-root",
            },
        }),
    )
    .await;
    assert_eq!(resolved.status(), StatusCode::OK);

    let response = match tokio::time::timeout(Duration::from_secs(20), response).await {
        Ok(response) => response.unwrap(),
        Err(_) => {
            let code_turn =
                tidebreak_core::db::code::get_turn(&runtime.db, &OwnerId::local(), parked.id)
                    .await
                    .unwrap();
            let run = runtime.db.get_turn(TurnId(parked.id.0)).await.unwrap();
            let events = tidebreak_core::db::code::list_recent_events(
                &runtime.db,
                &OwnerId::local(),
                session_id,
                32,
            )
            .await
            .unwrap();
            let requests = provider.requests.lock().unwrap().len();
            panic!(
                "the client result did not resume the turn; code_turn={code_turn:?} run={run:?} requests={requests} events={events:?}"
            );
        }
    };
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let completed = tidebreak_core::db::code::get_turn(&runtime.db, &OwnerId::local(), parked.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, CodeTurnStatus::Completed);
    assert_eq!(completed.park_ref, None);
    assert_eq!(completed.park_wait, None);
    let after_resume = runtime
        .db
        .get_turn(TurnId(parked.id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_resume.status, TurnRunStatus::Completed);
    assert_eq!(
        (after_resume.attempt_count, after_resume.claim_count),
        (1, 2)
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult { content, .. } if content == "connected-root"
            )
        })
    }));
}

#[tokio::test]
async fn an_internal_agent_wait_resumes_through_one_adapter_park() {
    let (router, token, runtime, _ran, _dir, _provider, state) =
        internal_engine_app_capturing(vec![
            Step::Tool {
                name: tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL,
                input: serde_json::json!({ "task": "research the answer" }),
            },
            Step::WaitForSpawnedAgent,
            Step::Text("child result received"),
        ])
        .await;
    let bearer = format!("Bearer {token}");
    let session_id = create_internal_session(&router, &bearer, "allow").await;
    let response = submit_internal_turn(&router, &bearer, session_id, "delegate this");

    let parked = wait_for_durable_park(&runtime, session_id).await;
    let run_ids = match parked.park_wait.as_ref().unwrap() {
        TurnParkWait::AgentRuns { run_ids } => run_ids.clone(),
        other => panic!("the turn parked on the wrong dependency: {other:?}"),
    };
    assert_eq!(run_ids.len(), 1);
    let child_id: tidebreak_core::AgentRunId = run_ids[0].parse().unwrap();
    let children = runtime
        .db
        .list_agent_runs(tidebreak_core::ChatId(session_id.0))
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.parent_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child_id);
    let before_resume = runtime
        .db
        .get_turn(TurnId(parked.id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (before_resume.attempt_count, before_resume.claim_count),
        (1, 2)
    );

    let child_lease = uuid::Uuid::new_v4();
    let claimed = runtime
        .db
        .claim_agent_run(child_lease, chrono::Duration::minutes(5), 4, 4)
        .await
        .unwrap()
        .expect("the sandbox child is claimable");
    assert_eq!(claimed.id, child_id);
    assert!(matches!(
        runtime
            .db
            .submit_agent_run_result(child_id, child_lease, "research complete")
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Completed(_))
    ));
    let wait_id: tidebreak_core::CallId = parked.park_ref.as_deref().unwrap().parse().unwrap();
    let resumed = runtime
        .db
        .resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    let event = match resumed {
        tidebreak_core::ResumeTurnForAgentRunWaitSetOutcome::Resumed { event, .. } => event,
        other => panic!("the ready wait did not resume: {other:?}"),
    };
    let _ = state
        .events
        .sender(tidebreak_core::ChatId(session_id.0))
        .send(event);
    state.turn_job_wake.notify_one();

    let response = tokio::time::timeout(Duration::from_secs(20), response)
        .await
        .expect("the child result resumes the turn")
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let completed = tidebreak_core::db::code::get_turn(&runtime.db, &OwnerId::local(), parked.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, CodeTurnStatus::Completed);
    assert_eq!(completed.park_ref, None);
    assert_eq!(completed.park_wait, None);
    let after_resume = runtime
        .db
        .get_turn(TurnId(parked.id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_resume.status, TurnRunStatus::Completed);
    assert_eq!(
        (after_resume.attempt_count, after_resume.claim_count),
        (1, 3)
    );
}

#[tokio::test]
async fn interrupting_an_internal_client_park_closes_the_turn() {
    let (router, token, runtime, _ran, _dir, _provider, _state) =
        internal_engine_app_capturing(vec![Step::Tool {
            name: "connect_folder",
            input: serde_json::json!({ "suggested_name": "Documents" }),
        }])
        .await;
    let bearer = format!("Bearer {token}");
    let session_id = create_internal_session(&router, &bearer, "ask").await;
    let response = submit_internal_turn(&router, &bearer, session_id, "connect documents");
    let parked = wait_for_durable_park(&runtime, session_id).await;
    let call_id = match parked.park_wait.as_ref().unwrap() {
        TurnParkWait::ClientToolCall { call_id } => call_id.clone(),
        other => panic!("the turn parked on the wrong dependency: {other:?}"),
    };

    let interrupted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/code/sessions/{session_id}/interrupt"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(interrupted.status(), StatusCode::ACCEPTED);
    let response = tokio::time::timeout(Duration::from_secs(20), response)
        .await
        .expect("the interrupt closes the parked turn")
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let closed = tidebreak_core::db::code::get_turn(&runtime.db, &OwnerId::local(), parked.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.status, CodeTurnStatus::Interrupted);
    let call_id: tidebreak_core::CallId = call_id.parse().unwrap();
    let call = runtime
        .db
        .list_tool_calls(tidebreak_core::ChatId(session_id.0))
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == call_id)
        .unwrap();
    assert_eq!(call.status, tidebreak_core::ToolCallStatus::Cancelled);
}

/// The plainest turn on the internal engine — one answer, no tools — is in
/// the journal once: the lane's rows, and nothing the worker wrote beside
/// them.
#[tokio::test]
async fn a_plain_internal_turn_is_journaled_once() {
    let (addr, token, runtime, _ran, _dir) =
        internal_engine_app(vec![Step::Text("just the answer")]).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "ask" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    let hosted: CodeSessionId = session["id"].as_str().unwrap().parse().unwrap();

    let response = tokio::time::timeout(Duration::from_secs(20), async {
        client
            .post(format!("http://{addr}/code/sessions/{hosted}/turns"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": "say it" }))
            .send()
            .await
            .unwrap()
    })
    .await
    .expect("the turn completes");
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(
        turn_statuses(&client, addr, &token, hosted).await,
        vec!["completed"]
    );

    let events = super::code::journaled_events(&runtime.db, hosted).await;
    assert_journaled_once(&events, 1);
    assert_eq!(streamed_text(&events), "just the answer");
    let types = event_types(&events);
    let started = position(&types, "turn_started", 0);
    let completed = position(&types, "turn_completed", started);
    assert_eq!(completed + 1, types.len(), "{types:?}");
    assert_chat_replay_is_the_journal(&runtime.db, hosted, &events).await;
}

/// The workspace-bound create path never selects the in-process engine,
/// and the engine's own create path needs no engine binary.
/// A chat the code runtime has never driven is a session row, but not one
/// of the runtime's: the session list does not show it, and boot recovery
/// does not attach a worker to it. Attaching would show, because the list
/// includes every session a worker has ever attached to.
#[tokio::test]
async fn a_chat_is_not_a_runtime_session_until_the_runtime_drives_it() {
    let (addr, token, runtime, _ran, _dir) = internal_engine_app(Vec::new()).await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("http://{addr}/chats"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "title": "plain chat" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let chat: serde_json::Value = created.json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_owned();

    let sessions = || async {
        client
            .get(format!("http://{addr}/code/sessions"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<Vec<serde_json::Value>>()
            .await
            .unwrap()
    };
    assert!(
        sessions().await.is_empty(),
        "a chat listed as a code session"
    );

    let actions = runtime.recover().await.unwrap();
    assert!(
        actions.is_empty(),
        "boot recovery acted on a chat: {actions:?}"
    );
    assert!(
        sessions().await.is_empty(),
        "boot recovery attached a worker to a chat"
    );

    // The same id still resolves on the session route: one id, one row.
    let by_id = client
        .get(format!("http://{addr}/code/sessions/{chat_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_id.status(), reqwest::StatusCode::OK);
    let session: serde_json::Value = by_id.json().await.unwrap();
    assert_eq!(session["harness_kind"], "internal");
    assert!(session["workspace_id"].is_null(), "{session}");
}

/// A session the runtime created and attached carries the engine's
/// `SessionStarted` in the code journal. The chat route deletes the
/// conversation and the code-side rows with it, and both routes then miss.
#[tokio::test]
async fn deleting_a_hosted_session_through_the_chat_route_removes_it_from_both_surfaces() {
    let (addr, token, _runtime, _ran, _dir) = internal_engine_app(Vec::new()).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{addr}/code/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "plan" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    let id: CodeSessionId = session["id"].as_str().unwrap().parse().unwrap();

    // Creating the session attached the engine, which journaled
    // `SessionStarted` under this id; without the code-side cascade the
    // delete below fails on that row's foreign key.
    let deleted = client
        .delete(format!("http://{addr}/chats/{id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = deleted.status();
    let body = deleted.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{body}");
    for (surface, route) in [
        ("chat", format!("http://{addr}/chats/{id}")),
        ("code", format!("http://{addr}/code/sessions/{id}")),
    ] {
        let missing = client.get(route).bearer_auth(&token).send().await.unwrap();
        assert_eq!(
            missing.status(),
            reqwest::StatusCode::NOT_FOUND,
            "the {surface} route still resolves the deleted conversation"
        );
    }
}

#[tokio::test]
async fn the_internal_engine_is_only_reachable_without_a_workspace() {
    let (addr, token, _runtime, _ran, dir) = internal_engine_app(Vec::new()).await;
    let client = reqwest::Client::new();
    let repo = super::code::init_git_repo(dir.path());
    let (_repo, workspace) =
        super::code::register_and_workspace(&client, addr, &token, &repo).await;
    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            super::code::json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "harness": "internal", "permission_mode": "ask" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "harness_needs_no_workspace");

    let doctor: serde_json::Value = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let internal = doctor["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "internal")
        .expect("the doctor lists the internal engine");
    assert_eq!(internal["found"], true);
    assert!(internal["path"].is_null());
    assert_eq!(internal["caps"]["durable_parks"], "supported");
    assert_eq!(internal["caps"]["image_input"], "supported");
}

/// A turn with an image attachment on the internal engine reaches the
/// model request with the image bytes: the worker hydrates them onto
/// the turn input, the engine publishes them through chat's attachment
/// model, and the lane's existing resolution puts them on the request.
#[tokio::test]
async fn an_internal_turn_with_an_image_reaches_the_model_with_the_bytes() {
    let (router, token, _runtime, _ran, _dir, provider, _state) =
        internal_engine_app_capturing(vec![Step::Text("i see it")]).await;
    let bearer = format!("Bearer {token}");
    let created = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/code/sessions")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "permission_mode": "ask" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let session: serde_json::Value = super::json_body(created).await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let pixels = crate::routes::image_attachment::png_header(4, 4);

    let published = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/code/sessions/{session_id}/attachments/images"))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "image/png")
                .body(Body::from(pixels.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);
    let attachment: serde_json::Value = super::json_body(published).await;
    let blob_id: uuid::Uuid = attachment["attachment_id"]
        .as_str()
        .expect("the publication names the blob")
        .parse()
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(20), async {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/code/sessions/{session_id}/turns"))
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "message": "what is in this",
                            "attachments": [{
                                "blob_id": blob_id,
                                "media_type": "image/png",
                            }],
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    })
    .await
    .expect("the turn completes");
    assert!(
        response.status().is_success(),
        "turn was not accepted: {}",
        response.status()
    );

    let request = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.images.get(blob_id).is_some())
        .cloned()
        .expect("the model received a request that carries the image");
    assert!(
        request.messages.iter().any(|message| {
            message.content.iter().any(
                |block| matches!(block, ContentBlock::Image { image } if image.blob_id == blob_id),
            )
        }),
        "the admitted user message must carry the image block"
    );
    let data = request
        .images
        .get(blob_id)
        .expect("the model request must carry the image bytes");
    assert_eq!(data.bytes(), pixels.as_slice());
}

#[allow(dead_code)]
fn _db_type_is_used(_: &DbStore) {}
