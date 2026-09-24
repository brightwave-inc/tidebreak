//! Review changes: reading answers, placing findings, the copy, and the run.

use super::*;

use serde_json::json;

use crate::code::types::{CodeReviewFinding, CodeReviewSeverity};

fn answer(findings: serde_json::Value) -> String {
    format!(
        "Here is the review.\n\n```json\n{}\n```\n",
        json!({ "summary": "Mostly fine.", "findings": findings })
    )
}

fn finding(file: &str, start: u64, end: u64) -> serde_json::Value {
    json!({
        "file": file,
        "start_line": start,
        "end_line": end,
        "severity": "high",
        "title": "Retry loop never stops",
        "explanation": "The loop has no exit when the server keeps failing.",
    })
}

fn findings_of(answer: ReviewAnswer) -> (Vec<CodeReviewFinding>, u32) {
    match answer {
        ReviewAnswer::Findings {
            findings, rejected, ..
        } => (findings, rejected),
        ReviewAnswer::Unreadable { text } => panic!("the answer was unreadable: {text}"),
    }
}

#[test]
fn a_fenced_answer_reads_into_findings() {
    let read = read_answer(&answer(json!([finding("src/queue.ts", 12, 14)])));
    let ReviewAnswer::Findings {
        summary,
        findings,
        rejected,
    } = read
    else {
        panic!("a fenced findings object is readable");
    };
    assert_eq!(summary.as_deref(), Some("Mostly fine."));
    assert_eq!(rejected, 0);
    assert_eq!(
        findings,
        vec![CodeReviewFinding {
            path: "src/queue.ts".into(),
            start_line: 12,
            end_line: 14,
            severity: CodeReviewSeverity::High,
            title: "Retry loop never stops".into(),
            explanation: "The loop has no exit when the server keeps failing.".into(),
        }]
    );
}

#[test]
fn the_last_readable_object_wins_and_prose_around_one_is_fine() {
    let two = format!(
        "```json\n{}\n```\nOn reflection:\n```json\n{}\n```",
        json!({ "findings": [finding("a.rs", 1, 1)] }),
        json!({ "findings": [finding("b.rs", 2, 2)] }),
    );
    let (findings, _) = findings_of(read_answer(&two));
    assert_eq!(findings[0].path, "b.rs");

    let bare = format!(
        "I looked at everything. {} That is all.",
        json!({ "findings": [finding("c.rs", 3, 3)] })
    );
    let (findings, _) = findings_of(read_answer(&bare));
    assert_eq!(findings[0].path, "c.rs");

    let whole = json!({ "findings": [] }).to_string();
    let (findings, rejected) = findings_of(read_answer(&whole));
    assert!(findings.is_empty());
    assert_eq!(rejected, 0);
}

#[test]
fn malformed_findings_are_left_out_and_counted_never_repaired() {
    let mut string_line = finding("a.rs", 1, 1);
    string_line["start_line"] = json!("12");
    let mut fraction = finding("a.rs", 1, 1);
    fraction["end_line"] = json!(1.5);
    let mut negative = finding("a.rs", 1, 1);
    negative["start_line"] = json!(-3);
    let mut unknown_severity = finding("a.rs", 1, 1);
    unknown_severity["severity"] = json!("catastrophic");
    let mut missing_title = finding("a.rs", 1, 1);
    missing_title.as_object_mut().unwrap().remove("title");
    let mut control_title = finding("a.rs", 1, 1);
    control_title["title"] = json!("Looks fine\u{1b}[2J");
    let mut bidi_title = finding("a.rs", 1, 1);
    bidi_title["title"] = json!("safe \u{202e}exe.txt");
    let mut multiline_title = finding("a.rs", 1, 1);
    multiline_title["title"] = json!("two\nlines");
    let mut empty_explanation = finding("a.rs", 1, 1);
    empty_explanation["explanation"] = json!("   ");
    let entries = json!([
        finding("a.rs", 0, 1),
        finding("a.rs", 5, 4),
        finding("a.rs", 1, 1 + u64::from(MAX_FINDING_LINES)),
        finding("/etc/passwd", 1, 1),
        finding("../outside.rs", 1, 1),
        finding("src/../../x.rs", 1, 1),
        finding("src\\win.rs", 1, 1),
        finding("C:/win.rs", 1, 1),
        finding("", 1, 1),
        finding("a//b.rs", 1, 1),
        string_line,
        fraction,
        negative,
        unknown_severity,
        missing_title,
        control_title,
        bidi_title,
        multiline_title,
        empty_explanation,
        "not an object",
        42,
        null,
        finding("kept.rs", 7, 7),
    ]);
    let (findings, rejected) = findings_of(read_answer(&answer(entries)));
    assert_eq!(
        findings.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["kept.rs"]
    );
    assert_eq!(rejected, 22);
}

#[test]
fn a_leading_dot_slash_and_case_in_severity_name_the_same_thing() {
    let mut loud = finding("./src/app.rs", 3, 3);
    loud["severity"] = json!(" Medium ");
    let (findings, rejected) = findings_of(read_answer(&answer(json!([loud]))));
    assert_eq!(rejected, 0);
    assert_eq!(findings[0].path, "src/app.rs");
    assert_eq!(findings[0].severity, CodeReviewSeverity::Medium);
}

#[test]
fn a_flood_of_findings_is_capped_duplicates_dropped_and_long_text_bounded() {
    let mut entries: Vec<_> = (1..=80).map(|line| finding("a.rs", line, line)).collect();
    entries.insert(1, finding("a.rs", 1, 1));
    let (findings, rejected) = findings_of(read_answer(&answer(json!(entries))));
    assert_eq!(findings.len(), MAX_FINDINGS);
    assert_eq!(findings[0].start_line, 1);
    assert_eq!(
        findings[1].start_line, 2,
        "the duplicate of line 1 is dropped"
    );
    assert_eq!(rejected, 30, "the thirty past the cap");

    let mut long = finding("a.rs", 1, 1);
    long["title"] = json!("t".repeat(5_000));
    long["explanation"] = json!("e".repeat(50_000));
    let (findings, _) = findings_of(read_answer(&answer(json!([long]))));
    assert_eq!(findings[0].title.chars().count(), MAX_TITLE_CHARS);
    assert!(findings[0].title.ends_with('…'));
    assert_eq!(
        findings[0].explanation.chars().count(),
        MAX_EXPLANATION_CHARS
    );
}

#[test]
fn an_answer_without_a_findings_object_is_kept_as_bounded_text() {
    for text in [
        "Looks good to me, ship it.",
        "```json\n{\"findings\": \n```",
        "{\"issues\": []}",
        "[1, 2, 3]",
    ] {
        assert_eq!(
            read_answer(text),
            ReviewAnswer::Unreadable {
                text: text.trim().to_owned()
            },
            "{text}"
        );
    }
    // `findings` of the wrong type is unreadable too, not an empty review.
    assert!(matches!(
        read_answer(&json!({ "findings": "none" }).to_string()),
        ReviewAnswer::Unreadable { .. }
    ));
    // Nesting deeper than the parser allows fails the parse, not the process.
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    assert!(matches!(
        read_answer(&format!("{{\"findings\": {deep}}}")),
        ReviewAnswer::Unreadable { .. }
    ));
    let long = "x".repeat(100_000);
    let ReviewAnswer::Unreadable { text } = read_answer(&long) else {
        panic!("prose is unreadable");
    };
    assert_eq!(text.chars().count(), findings::MAX_RAW_CHARS);
}

#[test]
fn findings_sit_on_the_diff_only_inside_one_hunk_of_the_new_side() {
    let diff = "diff --git a/a.rs b/a.rs\n\
                --- a/a.rs\n\
                +++ b/a.rs\n\
                @@ -1,3 +1,4 @@\n\
                 one\n\
                +two\n\
                 three\n\
                 four\n\
                @@ -20,2 +21 @@\n\
                -gone\n\
                 kept\n\
                @@ -40,3 +40,0 @@\n\
                -a\n\
                -b\n\
                -c\n";
    let spans = new_side_spans(diff);
    assert_eq!(spans, vec![(1, 4), (21, 21)]);
    let at = |start: u32, end: u32| CodeReviewFinding {
        path: "a.rs".into(),
        start_line: start,
        end_line: end,
        severity: CodeReviewSeverity::Low,
        title: "t".into(),
        explanation: "e".into(),
    };
    assert!(on_the_diff(&at(2, 3), &spans));
    assert!(on_the_diff(&at(21, 21), &spans));
    assert!(!on_the_diff(&at(4, 21), &spans), "across two hunks");
    assert!(!on_the_diff(&at(10, 10), &spans), "between hunks");
    assert!(
        !on_the_diff(&at(40, 40), &spans),
        "a hunk that only removes"
    );
}

#[test]
fn failures_sort_into_what_the_person_can_do() {
    let kind = |detail: &str| classify_failure(HarnessKind::Codex, detail).kind;
    assert_eq!(
        kind("stream error: 429 Too Many Requests"),
        CodeReviewFailureKind::RateLimited
    );
    assert_eq!(
        kind("You've hit your usage limit. Try again at 4pm."),
        CodeReviewFailureKind::RateLimited
    );
    assert_eq!(
        kind("API Error: 529 overloaded"),
        CodeReviewFailureKind::RateLimited
    );
    assert_eq!(
        kind("Invalid API key · Please run /login"),
        CodeReviewFailureKind::SignedOut
    );
    assert_eq!(
        kind("HTTP 401 from the provider"),
        CodeReviewFailureKind::SignedOut
    );
    assert_eq!(kind("error at line 4290"), CodeReviewFailureKind::Failed);
    assert_eq!(kind("process exited with 1"), CodeReviewFailureKind::Failed);
    let empty = classify_failure(HarnessKind::ClaudeCode, "  ");
    assert_eq!(empty.kind, CodeReviewFailureKind::Failed);
    assert_eq!(empty.message, "Claude Code stopped without saying why");
    let said = classify_failure(HarnessKind::Codex, "rate limit reached");
    assert!(said
        .message
        .starts_with("Codex CLI hit a rate or usage limit."));
    assert!(said.message.ends_with("Codex CLI said: rate limit reached"));
    let not_found = classify_harness_error(HarnessKind::Grok, &HarnessError::NotFound);
    assert_eq!(not_found.kind, CodeReviewFailureKind::NotInstalled);
}

fn caps(plan: CapLevel, approvals: CapLevel) -> HarnessCaps {
    HarnessCaps {
        resume: CapLevel::Supported,
        streaming_deltas: CapLevel::Supported,
        structured_approvals: approvals,
        mid_turn_steering: CapLevel::Unsupported,
        plan_mode: plan,
        auto_mode: CapLevel::Supported,
        allow_mode: CapLevel::Supported,
        reasoning_levels: CapLevel::Unsupported,
        native_file_change_events: CapLevel::Unsupported,
        native_interrupt: CapLevel::Supported,
        image_input: CapLevel::Unsupported,
        slash_commands: CapLevel::Unknown,
        durable_parks: CapLevel::Unsupported,
        user_questions: CapLevel::Unsupported,
        standing_grants: CapLevel::Unsupported,
        mid_turn_resume: CapLevel::Unsupported,
        transcript: CapLevel::Unsupported,
        memory_loopback: CapLevel::Unsupported,
    }
}

#[test]
fn an_engine_reviews_in_plan_mode_else_in_ask_with_everything_refused_else_not_at_all() {
    assert_eq!(
        review_permission_mode(&caps(CapLevel::Supported, CapLevel::Supported)),
        Some(PermissionMode::Plan)
    );
    assert_eq!(
        review_permission_mode(&caps(CapLevel::Unsupported, CapLevel::Supported)),
        Some(PermissionMode::Ask)
    );
    assert_eq!(
        review_permission_mode(&caps(CapLevel::Unknown, CapLevel::Unknown)),
        None,
        "an allow-all or auto posture never stands in for read-only"
    );
}

#[test]
fn each_pinned_engine_reviews_in_the_posture_the_pull_request_names() {
    let registry = tidebreak_harness::builtin_registry();
    for (kind, expected) in [
        (HarnessKind::ClaudeCode, PermissionMode::Plan),
        (HarnessKind::Codex, PermissionMode::Plan),
        (HarnessKind::Opencode, PermissionMode::Plan),
        (HarnessKind::Grok, PermissionMode::Ask),
    ] {
        let adapter = registry.get(kind).expect("a builtin adapter");
        let pin = tidebreak_harness::pin::pin_for(kind).expect("a pinned version");
        let probe = HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/engine")),
            version: Some(pin.version.to_owned()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
            reported_efforts: None,
        };
        assert_eq!(
            review_permission_mode(&adapter.capabilities(&probe)),
            Some(expected),
            "{kind}"
        );
    }
}

#[test]
fn a_model_id_reaches_argv_only_when_it_cannot_read_as_a_flag() {
    assert_eq!(review_model(None).unwrap(), None);
    assert_eq!(review_model(Some("  ".into())).unwrap(), None);
    assert_eq!(
        review_model(Some(" gpt-5.5 ".into())).unwrap().as_deref(),
        Some("gpt-5.5")
    );
    for bad in [
        "--dangerously-skip-permissions",
        "gpt 5",
        "gpt\n5",
        &"m".repeat(MAX_MODEL_CHARS + 1),
    ] {
        assert!(review_model(Some(bad.to_owned())).is_err(), "{bad}");
    }
    assert!(review_focus(Some("x".repeat(MAX_FOCUS_CHARS + 1))).is_err());
    assert_eq!(review_focus(Some("  ".into())).unwrap(), None);
}

#[test]
fn a_reviewer_gets_no_ssh_agent() {
    let env = vec![
        ("PATH".into(), "/bin".into()),
        ("SSH_AUTH_SOCK".into(), "/tmp/agent.sock".into()),
        ("SSH_AGENT_PID".into(), "42".into()),
    ];
    let kept = reviewer_env(&env);
    assert_eq!(kept, vec![("PATH".into(), "/bin".into())]);
}

#[test]
fn the_trace_refuses_approvals_counts_reads_and_keeps_the_closing_message() {
    let tree = PathBuf::from("/data/code/reviews/r/tree");
    let mut trace = ReviewTrace::new(tree.clone());
    assert!(trace
        .observe(&HarnessEvent::AssistantDelta {
            text: "Let me look.".into()
        })
        .is_none());
    trace.observe(&HarnessEvent::ToolStarted {
        call_id: "1".into(),
        name: "Read".into(),
        detail: ToolDetail::FileRead {
            path: tree.join("src/queue.ts").to_string_lossy().into_owned(),
        },
        parent_call_id: None,
    });
    trace.observe(&HarnessEvent::ToolStarted {
        call_id: "2".into(),
        name: "Read".into(),
        detail: ToolDetail::FileRead {
            path: tree.join("src/queue.ts").to_string_lossy().into_owned(),
        },
        parent_call_id: None,
    });
    let refused = trace.observe(&HarnessEvent::ApprovalRequested {
        harness_ref: HarnessApprovalRef::engine("write-1"),
        raw: serde_json::Value::Null,
        kind: None,
    });
    assert_eq!(
        refused.map(|approval| approval.call_id),
        Some("write-1".into())
    );
    trace.observe(&HarnessEvent::AssistantDelta {
        text: "```json\n{\"findings\": []}\n```".into(),
    });
    let progress = trace.progress();
    assert_eq!(progress.tool_calls, 2);
    assert_eq!(progress.files_read, 1);
    assert_eq!(progress.refused, 1);
    assert_eq!(
        trace.final_text().as_deref(),
        Some("```json\n{\"findings\": []}\n```"),
        "text streamed after the last tool call is the answer"
    );

    let mut whole = ReviewTrace::new(tree);
    whole.observe(&HarnessEvent::AssistantDelta {
        text: "draft".into(),
    });
    whole.observe(&HarnessEvent::AssistantMessage {
        text: "the answer".into(),
        parent_call_id: None,
    });
    whole.observe(&HarnessEvent::AssistantMessage {
        text: "a subagent's words".into(),
        parent_call_id: Some("task".into()),
    });
    assert_eq!(whole.final_text().as_deref(), Some("the answer"));
    assert!(whole.progress().activity.is_none());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repository(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "dev@example.com"]);
    git(&repo, &["config", "user.name", "Dev"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/queue.ts"), "const MAX = 10;\nexport {};\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

#[tokio::test]
async fn the_copy_holds_the_changes_shares_nothing_writable_and_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repository(dir.path());
    // The person's worktree: one committed-then-edited file, one untracked.
    std::fs::write(repo.join("src/queue.ts"), "const MAX = 20;\nexport {};\n").unwrap();
    std::fs::write(repo.join("NOTES.md"), "new\n").unwrap();
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let to = crate::code::checkpoint::snapshot_tree(&repo).await.unwrap();
    let refs_before = git(&repo, &["for-each-ref"]);

    let root = dir.path().join("data/code/reviews/one");
    let copy = materialize(&repo, &head, &to, &root).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(copy.tree.join("src/queue.ts")).unwrap(),
        "const MAX = 20;\nexport {};\n"
    );
    assert_eq!(
        std::fs::read_to_string(copy.tree.join("NOTES.md")).unwrap(),
        "new\n"
    );
    // HEAD is the state before the changes, so `git diff HEAD` is the review.
    let stat = git(&copy.tree, &["diff", "HEAD", "--name-only"]);
    assert_eq!(
        stat.lines().collect::<Vec<_>>(),
        vec!["NOTES.md", "src/queue.ts"]
    );
    assert_eq!(git(&copy.tree, &["remote"]), "", "the copy has no remote");

    // A reviewer that writes, commits, and moves refs changes only the copy.
    std::fs::write(copy.tree.join("src/queue.ts"), "hacked\n").unwrap();
    std::fs::write(copy.tree.join("evil.sh"), "rm -rf /\n").unwrap();
    git(
        &copy.tree,
        &[
            "-c",
            "user.email=r@example.com",
            "-c",
            "user.name=R",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qam",
            "x",
        ],
    );
    git(&copy.tree, &["update-ref", "refs/heads/main", "HEAD"]);
    git(&copy.tree, &["branch", "evil"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/queue.ts")).unwrap(),
        "const MAX = 20;\nexport {};\n"
    );
    assert!(!repo.join("evil.sh").exists());
    assert_eq!(git(&repo, &["for-each-ref"]), refs_before);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);

    snapshot::remove(&root).await;
    assert!(!root.exists(), "the copy is deleted");
    assert!(repo.join("src/queue.ts").exists());
}

#[tokio::test]
async fn a_copy_that_cannot_be_made_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repository(dir.path());
    let root = dir.path().join("data/code/reviews/two");
    let error = materialize(&repo, "HEAD", "no-such-tree", &root)
        .await
        .unwrap_err();
    assert!(!error.is_empty());
    assert!(!root.exists());
    assert!(materialize(&repo, "--output=/tmp/x", "HEAD", &root)
        .await
        .is_err());
}

#[test]
fn the_sweep_removes_copies_nothing_is_running_in() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ReviewRegistry::default();
    let stale = reviews_root(dir.path()).join(CodeReviewId::new().to_string());
    std::fs::create_dir_all(stale.join("tree")).unwrap();
    std::fs::write(stale.join("tree/file"), "x").unwrap();
    let running = CodeReviewSnapshot {
        id: CodeReviewId::new(),
        workspace_id: WorkspaceId::new(),
        session_id: SessionId::new(),
        harness: HarnessKind::Codex,
        model: None,
        turn_id: None,
        permission_mode: PermissionMode::Plan,
        status: CodeReviewStatus::Running,
        progress: CodeReviewProgress::default(),
        started_at: Utc::now(),
        finished_at: None,
        failure: None,
        result: None,
    };
    let live = review_root(dir.path(), running.id);
    std::fs::create_dir_all(&live).unwrap();
    let _cancelled = registry
        .admit(OwnerId::local(), running.clone())
        .expect("the first review is admitted");
    sweep_copies(dir.path(), &registry);
    assert!(!stale.exists());
    assert!(live.exists());

    let second = CodeReviewSnapshot {
        id: CodeReviewId::new(),
        ..running
    };
    assert!(
        registry.admit(OwnerId::local(), second).is_err(),
        "one running review per workspace"
    );
}
