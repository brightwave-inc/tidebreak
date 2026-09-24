//! Review changes through the routes: another engine reviews a workspace's
//! changes read-only, from a copy it cannot write back from, and its
//! findings come back placed on the diff it read.

use super::code::*;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::code::review::REFUSAL_FEEDBACK;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CapLevel, Event, HarnessKind, PermissionMode, ReviewOutcome, SessionId};
use tidebreak_harness::{
    AdapterRegistry, ApprovalDecision, HarnessApprovalRef, HarnessEvent, ProjectConfig,
};

/// A reviewer's one turn: it reads a file, then answers with `answer`.
fn review_script(answer: &str) -> Vec<HarnessEvent> {
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Codex,
            harness_version: "scripted".into(),
            resume_ref: None,
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::ToolStarted {
            call_id: "read-1".into(),
            name: "read".into(),
            detail: tidebreak_core::ToolDetail::FileRead {
                path: "README.md".into(),
            },
            parent_call_id: None,
        },
        HarnessEvent::AssistantMessage {
            text: answer.to_owned(),
            parent_call_id: None,
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

/// Two findings on the README change, one off the diff, one malformed.
fn findings_answer() -> String {
    format!(
        "Reviewed.\n```json\n{}\n```",
        serde_json::json!({
            "summary": "The README change needs a reason.",
            "findings": [
                {
                    "file": "README.md",
                    "start_line": 2,
                    "end_line": 2,
                    "severity": "medium",
                    "title": "Say why the greeting grew",
                    "explanation": "A second line with no context reads like a leftover.",
                },
                {
                    "file": "README.md",
                    "start_line": 40,
                    "end_line": 41,
                    "severity": "low",
                    "title": "Off the diff",
                    "explanation": "These lines are not in the change.",
                },
                { "file": "../etc/passwd", "start_line": 1, "end_line": 1 },
            ],
        })
    )
}

fn reviewer(answer: &str) -> ScriptedAdapter {
    ScriptedAdapter::new(review_script(answer)).with_kind(HarnessKind::Codex)
}

async fn two_engines(
    reviewer: &ScriptedAdapter,
) -> (
    axum::Router,
    Arc<str>,
    Arc<crate::code::CodeRuntime>,
    tempfile::TempDir,
) {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    registry.register(Arc::new(reviewer.clone()));
    code_app_with_registry(registry, None, None, None, false).await
}

struct Setup {
    addr: std::net::SocketAddr,
    token: Arc<str>,
    client: reqwest::Client,
    runtime: Arc<crate::code::CodeRuntime>,
    workspace: String,
    worktree: PathBuf,
    session: String,
    _dir: tempfile::TempDir,
}

/// A workspace whose README gained a line, and the conversation that owns it.
async fn setup(reviewer: &ScriptedAdapter) -> Setup {
    let (router, token, runtime, dir) = two_engines(reviewer).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree = PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&*token)
        .json(&serde_json::json!({ "harness": "claude_code", "permission_mode": "plan" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    std::fs::write(worktree.join("README.md"), "hello\nworld\n").unwrap();
    Setup {
        addr,
        client,
        runtime,
        workspace: json_id(&workspace).to_owned(),
        worktree,
        session: json_id(&session).to_owned(),
        token,
        _dir: dir,
    }
}

impl Setup {
    async fn start(&self, body: serde_json::Value) -> reqwest::Response {
        let mut body = body;
        body["session_id"] = serde_json::Value::String(self.session.clone());
        self.client
            .post(format!(
                "http://{}/code/workspaces/{}/reviews",
                self.addr, self.workspace
            ))
            .bearer_auth(&*self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn started(&self, body: serde_json::Value) -> serde_json::Value {
        let response = self.start(body).await;
        let status = response.status();
        let review: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{review}");
        assert_eq!(review["status"], "running");
        review
    }

    async fn review(&self, id: &str) -> serde_json::Value {
        self.client
            .get(format!(
                "http://{}/code/workspaces/{}/reviews/{id}",
                self.addr, self.workspace
            ))
            .bearer_auth(&*self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Poll until the review ends.
    async fn finished(&self, id: &str) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let review = self.review(id).await;
                if review["status"] != "running" {
                    return review;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the review ended")
    }

    async fn cancel(&self, id: &str) -> reqwest::Response {
        self.client
            .post(format!(
                "http://{}/code/workspaces/{}/reviews/{id}/cancel",
                self.addr, self.workspace
            ))
            .bearer_auth(&*self.token)
            .send()
            .await
            .unwrap()
    }

    async fn journaled_reviews(&self) -> Vec<Event> {
        let session: SessionId = self.session.parse().unwrap();
        journaled_events(&self.runtime.db, session)
            .await
            .into_iter()
            .map(|sequenced| sequenced.event)
            .filter(|event| matches!(event, Event::ReviewFinished { .. }))
            .collect()
    }
}

async fn launched_copy(reviewer: &ScriptedAdapter) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some((cwd, _)) = reviewer.launched_sessions().first() {
                return cwd.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the reviewer launched")
}

#[tokio::test]
async fn another_engine_reviews_a_copy_and_its_findings_come_back_on_the_diff() {
    let reviewer = reviewer(&findings_answer()).with_writes(&[
        ("README.md", "overwritten by the reviewer\n"),
        ("hack.txt", "the reviewer was here\n"),
    ]);
    let setup = setup(&reviewer).await;
    let started = setup
        .started(serde_json::json!({ "harness": "codex", "model": "gpt-5.5" }))
        .await;
    assert_eq!(started["permission_mode"], "plan");
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "completed", "{review}");

    let result = &review["result"];
    assert_eq!(result["summary"], "The README change needs a reason.");
    assert_eq!(result["rejected"], 1, "the malformed entry is left out");
    let placed = result["findings"].as_array().unwrap();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0]["path"], "README.md");
    assert_eq!(placed[0]["start_line"], 2);
    assert_eq!(placed[0]["severity"], "medium");
    let unplaced = result["unplaced"].as_array().unwrap();
    assert_eq!(unplaced.len(), 1);
    assert_eq!(unplaced[0]["start_line"], 40);
    let diff = result["diff"].as_str().unwrap();
    assert!(diff.contains("+world"), "{diff}");

    // The reviewer's writes landed in its copy, never in the worktree, and
    // the copy is gone.
    assert_eq!(
        std::fs::read_to_string(setup.worktree.join("README.md")).unwrap(),
        "hello\nworld\n"
    );
    assert!(!setup.worktree.join("hack.txt").exists());
    let launched = reviewer.launched_sessions();
    assert_eq!(launched.len(), 1);
    let (copy, mode) = &launched[0];
    assert_eq!(*mode, PermissionMode::Plan);
    assert_ne!(copy, &setup.worktree);
    assert!(!copy.starts_with(&setup.worktree));
    assert!(!copy.exists(), "the copy is deleted when the review ends");

    // No approval channel, no connected apps, no repository engine config.
    assert_eq!(reviewer.launched_approvals(), vec![None]);
    assert_eq!(reviewer.launched_apps(), vec![None]);
    assert_eq!(
        reviewer.launched_project_configs(),
        vec![ProjectConfig::Skip]
    );
    // A read-only launch, whose git reads the copy's empty global config and
    // never prompts for credentials.
    let [(read_only, env)] = reviewer.launched_postures().try_into().unwrap();
    assert!(read_only, "the engine's writing tools are taken away");
    let value = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(value("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    assert_eq!(value("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
    let global = PathBuf::from(value("GIT_CONFIG_GLOBAL").unwrap());
    assert!(global.starts_with(copy.parent().unwrap()), "{global:?}");

    // The reviewer read the diff it was asked about, with the chosen model.
    let inputs = reviewer.turn_inputs();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].model.as_deref(), Some("gpt-5.5"));
    assert!(inputs[0].text.contains("+world"));
    assert!(inputs[0].text.contains("`git diff HEAD`"));

    assert_eq!(
        setup.journaled_reviews().await,
        vec![Event::ReviewFinished {
            review_id: review["id"].as_str().unwrap().parse().unwrap(),
            harness: HarnessKind::Codex,
            model: Some("gpt-5.5".into()),
            turn_id: None,
            outcome: ReviewOutcome::Completed,
            findings: 2,
        }]
    );
}

#[tokio::test]
async fn every_request_a_reviewer_makes_to_change_something_is_refused() {
    let mut script = review_script(r#"{"findings": []}"#);
    script.insert(
        2,
        HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef::engine("write-1"),
            raw: serde_json::json!({ "tool_name": "Write", "input": { "file_path": "README.md" } }),
            kind: None,
        },
    );
    let reviewer = ScriptedAdapter::new(script)
        .with_kind(HarnessKind::Codex)
        .with_approvals(CapLevel::Supported);
    let setup = setup(&reviewer).await;
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "completed", "{review}");
    assert_eq!(review["progress"]["refused"], 1);
    assert_eq!(review["result"]["findings"], serde_json::json!([]));
    assert_eq!(
        reviewer.observed_decisions(),
        vec![(
            "write-1".to_owned(),
            ApprovalDecision::Deny {
                feedback: Some(REFUSAL_FEEDBACK.to_owned())
            }
        )]
    );
}

#[tokio::test]
async fn an_engine_with_no_plan_mode_reviews_in_ask_and_one_with_neither_is_refused() {
    let planless = ScriptedAdapter::new(review_script(r#"{"findings": []}"#))
        .with_kind(HarnessKind::Codex)
        .with_plan_mode(CapLevel::Unsupported)
        .with_approvals(CapLevel::Supported);
    let with_planless = setup(&planless).await;
    let started = with_planless
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    assert_eq!(started["permission_mode"], "ask");
    let review = with_planless
        .finished(started["id"].as_str().unwrap())
        .await;
    assert_eq!(review["status"], "completed", "{review}");
    assert_eq!(planless.launched_sessions()[0].1, PermissionMode::Ask);

    let neither = ScriptedAdapter::new(review_script(r#"{"findings": []}"#))
        .with_kind(HarnessKind::Opencode)
        .with_plan_mode(CapLevel::Unsupported);
    let with_neither = setup(&neither).await;
    let refused = with_neither
        .start(serde_json::json!({ "harness": "opencode" }))
        .await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "review_engine_unsupported", "{body}");
    assert!(neither.launched_sessions().is_empty());
}

/// Grok CLI can't turn off network access, so it does not review: a start
/// is refused with that reason before anything runs, and the engine list
/// shows Grok with the reason, installed or not.
#[tokio::test]
async fn grok_is_listed_but_never_reviews() {
    let reason = tidebreak_harness::grok::READ_ONLY_UNAVAILABLE;
    assert_eq!(
        reason,
        "Grok CLI can't review read-only yet: it can't turn off network access."
    );
    let grok = ScriptedAdapter::new(review_script(r#"{"findings": []}"#))
        .with_kind(HarnessKind::Grok)
        .with_plan_mode(CapLevel::Unsupported)
        .with_approvals(CapLevel::Supported)
        .with_read_only_blocker(reason);
    let setup = setup(&grok).await;
    let refused = setup.start(serde_json::json!({ "harness": "grok" })).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "review_engine_unsupported", "{body}");
    assert_eq!(body["message"], reason);
    assert!(grok.launched_sessions().is_empty(), "nothing ran");

    let doctor: serde_json::Value = setup
        .client
        .get(format!("http://{}/code/harnesses", setup.addr))
        .bearer_auth(&*setup.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = doctor["harnesses"].as_array().expect("doctor entries");
    let grok_entry = entries
        .iter()
        .find(|entry| entry["kind"] == "grok")
        .expect("grok is listed");
    assert_eq!(grok_entry["review_blocked"], reason);
    let claude = entries
        .iter()
        .find(|entry| entry["kind"] == "claude_code")
        .expect("the working engine is listed");
    assert!(claude.get("review_blocked").is_none(), "{claude}");

    // Asked once per install, not on every request.
    let again = setup.start(serde_json::json!({ "harness": "grok" })).await;
    assert_eq!(again.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(grok.read_only_checks(), 1);
}

#[tokio::test]
async fn a_running_review_never_holds_up_the_conversation_it_came_from() {
    let reviewer = reviewer(r#"{"findings": []}"#).with_turn_delay(Duration::from_secs(30));
    let setup = setup(&reviewer).await;
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let id = started["id"].as_str().unwrap();
    launched_copy(&reviewer).await;

    // The working agent's turn starts at once and ends while the review runs.
    let turn = run_turn_to_end(
        &setup.client,
        setup.addr,
        &setup.token,
        &setup.session,
        serde_json::json!({ "message": "keep going" }),
    )
    .await;
    assert_eq!(turn["status"], "completed");
    assert_eq!(setup.review(id).await["status"], "running");

    // A second review of the same workspace waits for this one.
    let second = setup.start(serde_json::json!({ "harness": "codex" })).await;
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(body["kind"], "review_running");

    assert_eq!(setup.cancel(id).await.status(), reqwest::StatusCode::OK);
    assert_eq!(setup.finished(id).await["status"], "cancelled");
}

#[tokio::test]
async fn a_cancelled_review_stops_the_engine_deletes_the_copy_and_says_so() {
    let reviewer = reviewer(r#"{"findings": []}"#).with_turn_delay(Duration::from_secs(30));
    let setup = setup(&reviewer).await;
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let id = started["id"].as_str().unwrap();
    let copy = launched_copy(&reviewer).await;
    assert!(copy.exists());

    let cancelled = setup.cancel(id).await;
    assert_eq!(cancelled.status(), reqwest::StatusCode::OK);
    let review = setup.finished(id).await;
    assert_eq!(review["status"], "cancelled", "{review}");
    assert!(review.get("result").is_none());
    assert!(!copy.exists(), "the copy is deleted");
    assert_eq!(reviewer.shutdown_count(), 1, "the engine is shut down");
    let recorded = setup.journaled_reviews().await;
    assert!(matches!(
        recorded.as_slice(),
        [Event::ReviewFinished {
            outcome: ReviewOutcome::Cancelled,
            findings: 0,
            ..
        }]
    ));
}

#[tokio::test]
async fn a_review_that_runs_past_its_limit_is_stopped() {
    let reviewer = reviewer(r#"{"findings": []}"#).with_turn_delay(Duration::from_secs(30));
    let setup = setup(&reviewer).await;
    setup
        .runtime
        .reviews
        .set_time_limit(Duration::from_millis(400));
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "timed_out", "{review}");
    assert_eq!(review["failure"]["kind"], "timed_out");
    let copy = &reviewer.launched_sessions()[0].0;
    assert!(!copy.exists());
}

#[tokio::test]
async fn a_failing_engine_says_why_and_an_unreadable_answer_still_arrives() {
    let limited = ScriptedAdapter::new(vec![
        HarnessEvent::TurnStarted,
        HarnessEvent::TurnFailed {
            error: tidebreak_core::BoundedError {
                message: "stream error: 429 Too Many Requests".into(),
            },
        },
    ])
    .with_kind(HarnessKind::Codex);
    let failing = setup(&limited).await;
    let started = failing
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = failing.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "failed", "{review}");
    assert_eq!(review["failure"]["kind"], "rate_limited");

    let chatty = reviewer("Looks good to me. I would ship it.");
    let answering = setup(&chatty).await;
    let started = answering
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = answering.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "completed", "{review}");
    assert_eq!(
        review["result"]["raw_text"],
        "Looks good to me. I would ship it."
    );
    assert_eq!(review["result"]["findings"], serde_json::json!([]));
}

#[tokio::test]
async fn a_review_is_refused_before_anything_runs_when_it_cannot_run() {
    let signed_out = reviewer(r#"{"findings": []}"#).with_authenticated(Some(false));
    let signed_out_setup = setup(&signed_out).await;
    let refused = signed_out_setup
        .start(serde_json::json!({ "harness": "codex" }))
        .await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "harness_not_authenticated", "{body}");

    let fine = reviewer(r#"{"findings": []}"#);
    let ready = setup(&fine).await;
    let flag = ready
        .start(serde_json::json!({ "harness": "codex", "model": "--dangerously-skip-permissions" }))
        .await;
    assert_eq!(flag.status(), reqwest::StatusCode::BAD_REQUEST);

    std::fs::write(ready.worktree.join("README.md"), "hello\n").unwrap();
    let empty = ready.start(serde_json::json!({ "harness": "codex" })).await;
    assert_eq!(empty.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = empty.json().await.unwrap();
    assert_eq!(body["kind"], "review_empty", "{body}");
    assert!(fine.launched_sessions().is_empty());
}

#[tokio::test]
async fn one_turn_can_be_reviewed_and_the_review_names_it() {
    let reviewer = reviewer(&findings_answer());
    let setup = setup(&reviewer).await;
    // The scripted working engine writes nothing, so the README edit the
    // setup made is the turn's change.
    let turn = run_turn_to_end(
        &setup.client,
        setup.addr,
        &setup.token,
        &setup.session,
        serde_json::json!({ "message": "grow the greeting" }),
    )
    .await;
    let turn_id = json_id(&turn).to_owned();
    // A later edit is outside the turn.
    std::fs::write(setup.worktree.join("LATER.md"), "later\n").unwrap();

    let started = setup
        .started(serde_json::json!({ "harness": "codex", "turn_id": turn_id }))
        .await;
    assert_eq!(started["turn_id"], turn_id.as_str());
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    assert_eq!(review["status"], "completed", "{review}");
    let text = &reviewer.turn_inputs()[0].text;
    assert!(text.contains("turn 1"), "{text}");
    assert!(text.contains("+world"));
    assert!(!text.contains("LATER.md"), "the turn's diff only");
    assert_eq!(review["result"]["findings"][0]["path"], "README.md");
    assert!(matches!(
        setup.journaled_reviews().await.as_slice(),
        [Event::ReviewFinished {
            turn_id: Some(_),
            ..
        }]
    ));
}

/// Run a review whose engine writes `write` inside its copy, where the
/// reviewed tree holds `link` as an untracked symlink to `target`. Returns
/// the review and the data folder the copies live under.
#[cfg(unix)]
async fn write_through_a_link(
    link: &str,
    target: impl AsRef<std::path::Path>,
    write: &str,
) -> (Setup, serde_json::Value, PathBuf) {
    let reviewer =
        reviewer(r#"{"findings": []}"#).with_writes(&[(write, "the reviewer was here\n")]);
    let setup = setup(&reviewer).await;
    std::os::unix::fs::symlink(target, setup.worktree.join(link)).unwrap();
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    // <data>/code/reviews/<id>/tree
    let copy = launched_copy(&reviewer).await;
    let data_dir = copy.ancestors().nth(4).unwrap().to_path_buf();
    assert!(!copy.exists(), "the copy is deleted when the review ends");
    (setup, review, data_dir)
}

/// A link in the reviewed tree to an absolute path outside it is copied as
/// a plain file holding its target, so a write "inside the copy" through it
/// goes nowhere.
#[cfg(unix)]
#[tokio::test]
async fn a_link_to_an_absolute_path_carries_no_write_out_of_the_copy() {
    let outside = tempfile::tempdir().unwrap();
    let (_setup, review, _) =
        write_through_a_link("escape", outside.path(), "escape/escaped.txt").await;
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "the write followed the link out: {review}"
    );
}

/// A relative link that climbs out of the copy, into the Tidebreak data
/// folder the copies live under, carries no write there.
#[cfg(unix)]
#[tokio::test]
async fn a_relative_link_carries_no_write_into_the_data_folder() {
    let (_setup, review, data_dir) =
        write_through_a_link("up", "../../../..", "up/escaped.txt").await;
    assert!(
        data_dir.join("code").is_dir(),
        "{} is the data folder",
        data_dir.display()
    );
    assert!(
        !data_dir.join("escaped.txt").exists(),
        "the write climbed into {}: {review}",
        data_dir.display()
    );
}

/// A link to the person's own worktree carries no write back into it.
#[cfg(unix)]
#[tokio::test]
async fn a_link_to_the_worktree_carries_no_write_into_it() {
    let reviewer = reviewer(r#"{"findings": []}"#)
        .with_writes(&[("wt/escaped.txt", "the reviewer was here\n")]);
    let setup = setup(&reviewer).await;
    std::os::unix::fs::symlink(&setup.worktree, setup.worktree.join("wt")).unwrap();
    let started = setup
        .started(serde_json::json!({ "harness": "codex" }))
        .await;
    let review = setup.finished(started["id"].as_str().unwrap()).await;
    assert!(
        !setup.worktree.join("escaped.txt").exists(),
        "the write reached the worktree: {review}"
    );
    assert_eq!(
        std::fs::read_to_string(setup.worktree.join("README.md")).unwrap(),
        "hello\nworld\n"
    );
}

/// A workspace too large to copy is refused before anything runs, with a
/// reason the person can read.
#[tokio::test]
async fn a_workspace_too_large_to_copy_is_refused_with_a_plain_reason() {
    let reviewer = reviewer(r#"{"findings": []}"#);
    let setup = setup(&reviewer).await;
    let limits = |files, bytes| crate::code::review::CopySize { files, bytes };
    setup.runtime.reviews.set_copy_limits(limits(1_000, 8));
    let refused = setup.start(serde_json::json!({ "harness": "codex" })).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "review_too_large", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.starts_with("This workspace is too large to review:"),
        "{message}"
    );
    assert!(message.contains("more than 8 bytes"), "{message}");

    for name in ["one.md", "two.md", "three.md"] {
        std::fs::write(setup.worktree.join(name), "note\n").unwrap();
    }
    setup.runtime.reviews.set_copy_limits(limits(2, 1_000_000));
    let refused = setup.start(serde_json::json!({ "harness": "codex" })).await;
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "review_too_large", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("it has more than 2 files"),
        "{body}"
    );
    assert!(reviewer.launched_sessions().is_empty(), "nothing ran");
    let listed: serde_json::Value = setup
        .client
        .get(format!(
            "http://{}/code/workspaces/{}/reviews",
            setup.addr, setup.workspace
        ))
        .bearer_auth(&*setup.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["reviews"], serde_json::json!([]), "{listed}");
}
