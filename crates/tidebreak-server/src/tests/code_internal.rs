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

use tidebreak_core::{
    ApprovalClass, ChatRequest, CodeSessionId, DbStore, ModelProvider, ProviderEvent, ProviderId,
    StopReason, Tool, ToolCtx, ToolOutput, ToolRegistry, ToolSpec,
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
    Text(&'static str),
}

/// A provider that replays one scripted completion per model call.
struct ScriptedProvider {
    steps: Mutex<Vec<Step>>,
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted")
    }

    async fn stream(
        &self,
        _request: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
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
    tools.register_foreground_agent_orchestration();
    let provider = Arc::new(ScriptedProvider {
        steps: Mutex::new(steps),
        calls: AtomicUsize::new(0),
    });
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store_trait,
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "scripted".into(),
            ..AgentConfig::default()
        },
    );
    spawn_turn_worker(&state);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(InternalAdapter::new(state.clone())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    state.code = Some(runtime.clone());
    let token = state.token.clone();
    let addr = super::code::serve(app(state)).await;
    (addr, token, runtime, ran, dir)
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

    // The conversation is the engine's, not the owner's: no chat route sees it.
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
    assert!(chats.is_empty(), "engine-private chat listed: {chats:?}");
    let by_id = client
        .get(format!("http://{addr}/chats/{session_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_id.status(), reqwest::StatusCode::NOT_FOUND);

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
    assert_eq!(
        turn_statuses(&client, addr, &token, session_id).await,
        vec!["waiting"]
    );
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

    // The journal is the one the code wire serves, in the code vocabulary.
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
    let message = position(&types, "assistant_message", tool_completed);
    let completed = position(&types, "turn_completed", message);
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
    let plan_decision = serde_json::to_value(&events[plan_resolved].event).unwrap();
    assert_eq!(plan_decision["decision"]["type"], "plan_decided");
    let grant_decision = serde_json::to_value(&events[tool_resolved].event).unwrap();
    assert_eq!(grant_decision["decision"]["type"], "approved_with_grant");
    let text = serde_json::to_value(&events[message].event).unwrap();
    assert_eq!(text["text"], "all done");
}

/// The workspace-bound create path never selects the in-process engine,
/// and the engine's own create path needs no engine binary.
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
}

#[allow(dead_code)]
fn _db_type_is_used(_: &DbStore) {}
