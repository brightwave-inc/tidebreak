//! Shared fixtures for the code-mode wire surface.
//!
//! `fixtures/code-frames.json` holds one real value of every snapshot the code
//! routes return, every notice on `/code/updates`, and every event the
//! per-session socket can carry, serialized from the server's own types. Three
//! decoders read the file: this crate's round trip, the CLI's
//! `api::code` tests, and the renderer's `code/parsers.test.ts`. A shape
//! change shows up as a failing test on whichever side did not follow it.
//!
//! Regenerate with `UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server`, the
//! same switch that rewrites `wire.ts`.

use crate::wire::{
    CodeActionSnapshot, CodeApprovalSnapshot, CodeCommitSnapshot, CodeFileChange, CodePushSnapshot,
    CodeRepoSnapshot, CodeSessionDigest, CodeSessionExternalOrigin, CodeSessionSnapshot,
    CodeTurnRewriteState, CodeTurnSnapshot, CodeUpdateNotice, CodeWatchSnapshot, CodeWorkspaceDiff,
    CodeWorkspaceFiles, CodeWorkspacePrSnapshot, CodeWorkspaceSnapshot, HarnessAuthMode,
    HarnessDoctorEntry, HarnessDoctorReport, QueuedCodeTurn, QueuedCodeTurnsSnapshot,
    SequencedCodeEventFrame,
};
use crate::wire_types::generate;
use tidebreak_core::{
    ApprovalClass, ApprovalDecisionKind, Attention, AttentionSource, AttentionState, BoundedError,
    CapLevel, CheckpointHint, CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeEvent,
    CodeSessionActivity, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentStatus,
    CodeSubagentSummary, CodeTerminalId, CodeTurnId, CodeTurnStatus, CodeUsage, CodeWatchId,
    CodeWatchState, CodeWorkspaceStatus, Diffstat, FenceReason, FileChangeKind, GrantScope,
    HarnessCaps, HarnessCommand, HarnessKind, HarnessNoticeLevel, HarnessTier, ImageMediaType,
    ImageRef, InternalApprovalRequest, PermissionMode, PullRequestCheckCounts, PullRequestDigest,
    QuickAction, ReasoningEffort, RefusalDetails, RefusalOutcome, RepoId, ToolApprovalKind,
    ToolDetail, ToolOutcome, WorkspaceId,
};

/// Path of the shared code-mode fixtures, relative to this crate.
const CODE_FRAMES: &str = "fixtures/code-frames.json";

const REGENERATE: &str = "UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server";

/// One fixture: a stable name, the kind a reader dispatches on, and the value.
pub(crate) struct Fixture {
    pub(crate) name: &'static str,
    pub(crate) kind: &'static str,
    pub(crate) value: serde_json::Value,
}

/// Fixed ids, so the file does not change on every run.
fn id(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).expect("a fixed timestamp")
}

fn fixture<T: serde::Serialize>(name: &'static str, kind: &'static str, value: &T) -> Fixture {
    Fixture {
        name,
        kind,
        value: serde_json::to_value(value).expect("a wire value serializes"),
    }
}

fn repo_id() -> RepoId {
    RepoId(id(0x01))
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId(id(0x02))
}

fn session_id() -> CodeSessionId {
    CodeSessionId(id(0x03))
}

fn turn_id() -> CodeTurnId {
    CodeTurnId(id(0x04))
}

fn approval_id() -> CodeApprovalId {
    CodeApprovalId(id(0x05))
}

fn diffstat() -> Diffstat {
    Diffstat {
        files: 2,
        insertions: 14,
        deletions: 3,
        truncated: false,
    }
}

fn usage() -> CodeUsage {
    CodeUsage {
        input_tokens: 1_200,
        output_tokens: 340,
        cache_read_input_tokens: 900,
        cache_creation_input_tokens: 0,
        context_tokens: 2_440,
        first_call_context_tokens: Some(2_100),
    }
}

fn attention() -> Attention {
    Attention {
        state: AttentionState::Working,
        source: AttentionSource::Lifecycle,
    }
}

fn pull_request() -> PullRequestDigest {
    PullRequestDigest {
        number: 3006,
        url: Some("https://github.com/brightwave-inc/tidebreak/pull/3006".to_owned()),
        state: "open".to_owned(),
        title: Some("refactor(cli): decode the chat event socket".to_owned()),
        checks_summary: Some("3 pending".to_owned()),
        check_counts: Some(PullRequestCheckCounts {
            passing: 10,
            pending: 3,
            failing: 0,
            skipped: 2,
        }),
        checks: None,
        draft: Some(false),
        merged: Some(false),
        review_decision: Some("APPROVED".to_owned()),
        mergeable: Some("MERGEABLE".to_owned()),
        merge_state_status: Some("BLOCKED".to_owned()),
        head_branch: Some("thet/cli-wire-mirror".to_owned()),
        base_branch: Some("main".to_owned()),
        head_sha: Some("24b451ab7248eb057445718f2ad6304a43915f37".to_owned()),
        auto_merge_enabled: Some(true),
        in_merge_queue: Some(false),
    }
}

fn caps() -> HarnessCaps {
    HarnessCaps {
        resume: CapLevel::Supported,
        streaming_deltas: CapLevel::Supported,
        structured_approvals: CapLevel::Supported,
        mid_turn_steering: CapLevel::Supported,
        plan_mode: CapLevel::Supported,
        auto_mode: CapLevel::Supported,
        allow_mode: CapLevel::Supported,
        reasoning_levels: CapLevel::Supported,
        native_file_change_events: CapLevel::Unsupported,
        native_interrupt: CapLevel::Supported,
        image_input: CapLevel::Supported,
        slash_commands: CapLevel::Supported,
        durable_parks: CapLevel::Unknown,
        user_questions: CapLevel::Supported,
        standing_grants: CapLevel::Supported,
    }
}

fn session() -> CodeSessionSnapshot {
    CodeSessionSnapshot {
        id: session_id(),
        workspace_id: Some(workspace_id()),
        kind: CodeSessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: Some("2.0.14".to_owned()),
        harness_resume_ref: Some("9f2c1d4e-resume".to_owned()),
        permission_mode: PermissionMode::Ask,
        model: Some("claude-opus-5".to_owned()),
        reasoning_effort: Some(ReasoningEffort::High),
        fast_mode: false,
        lifecycle: CodeSessionLifecycle::Running,
        fence_reason: None,
        attention: attention(),
        unrecognized_event_count: 0,
        created_at: at(1_756_700_000),
        external_origin: Some(CodeSessionExternalOrigin {
            channel_kind: "slack".to_owned(),
            external_key: "T0/C1/1756700000.000100".to_owned(),
        }),
    }
}

fn turn() -> CodeTurnSnapshot {
    CodeTurnSnapshot {
        id: turn_id(),
        session_id: session_id(),
        ordinal: 3,
        status: CodeTurnStatus::Completed,
        model: Some("claude-opus-5".to_owned()),
        fast_mode: false,
        user_input: "Bound every string the code parser draws.".to_owned(),
        attachments: vec![ImageRef {
            blob_id: id(0x30),
            media_type: ImageMediaType::Png,
            width: 640,
            height: 480,
            byte_len: 51_200,
        }],
        usage: Some(usage()),
        checkpoint_ref: Some("refs/tidebreak/checkpoints/3".to_owned()),
        diffstat: Some(diffstat()),
        started_at: at(1_756_700_100),
        ended_at: Some(at(1_756_700_160)),
        rewrite: Some("Every code-mode string now has a bound.".to_owned()),
    }
}

fn queued_turn() -> QueuedCodeTurn {
    QueuedCodeTurn {
        id: CodeTurnId(id(0x06)),
        session_id: session_id(),
        message: "Then add the fixture test.".to_owned(),
        position: 0,
        created_at: at(1_756_700_170),
        updated_at: at(1_756_700_170),
    }
}

fn digest() -> CodeSessionDigest {
    CodeSessionDigest {
        workspace: Some(workspace_id()),
        session: session_id(),
        kind: CodeSessionKind::Interactive,
        harness_kind: Some(HarnessKind::ClaudeCode),
        lifecycle: CodeSessionLifecycle::Running,
        attention: attention(),
        title: "Bound the code parser".to_owned(),
        turn_count: 3,
        trigger_target_at: Some(at(1_756_700_100)),
        activity: Some(CodeSessionActivity::Shell),
        pr_state: Some(pull_request()),
        pr_count: Some(1),
        watch_state: None,
        watch_detail: None,
        watch_cycles: None,
        subagents: Some(vec![CodeSubagentSummary {
            call_id: "call-explore".to_owned(),
            name: "Explore".to_owned(),
            status: CodeSubagentStatus::Running,
        }]),
        recap: Some("The parser bounds every field; the tests are next.".to_owned()),
    }
}

/// The in-process engine's session binds no workspace (decision 0048 step 5).
fn internal_digest() -> CodeSessionDigest {
    CodeSessionDigest {
        workspace: None,
        session: CodeSessionId(id(0x22)),
        harness_kind: Some(HarnessKind::Internal),
        title: "Plan the memory substrate".to_owned(),
        turn_count: 1,
        activity: Some(CodeSessionActivity::Agent),
        pr_state: None,
        pr_count: None,
        subagents: None,
        recap: None,
        ..digest()
    }
}

fn digest_notice(d: CodeSessionDigest) -> CodeUpdateNotice {
    CodeUpdateNotice::Digest {
        workspace: d.workspace,
        session: d.session,
        kind: d.kind,
        harness_kind: d.harness_kind,
        lifecycle: d.lifecycle,
        attention: d.attention,
        title: d.title,
        turn_count: d.turn_count,
        trigger_target_at: d.trigger_target_at,
        activity: d.activity,
        pr_state: d.pr_state.map(Box::new),
        pr_count: d.pr_count,
        watch_state: d.watch_state,
        watch_detail: d.watch_detail,
        watch_cycles: d.watch_cycles,
        subagents: d.subagents,
        recap: d.recap,
    }
}

fn frame(seq: i64, event: CodeEvent) -> SequencedCodeEventFrame {
    SequencedCodeEventFrame {
        seq,
        event,
        replayed: None,
        transient: None,
        replacement: None,
        truncated: None,
    }
}

/// Every snapshot, notice, and event the code surface serializes, once each.
///
/// [`the_code_frame_fixtures_cover_every_event`] proves the event and notice
/// lists cannot silently fall behind their unions.
pub(crate) fn code_frame_fixtures() -> Vec<Fixture> {
    let mut out = vec![
        fixture(
            "repo",
            "repo",
            &CodeRepoSnapshot {
                id: repo_id(),
                root_path: "/Users/mara/code/tidebreak".to_owned(),
                display_name: "tidebreak".to_owned(),
                default_base_ref: "main".to_owned(),
                branch_prefix: "mara/".to_owned(),
                setup_script: Some("pnpm install".to_owned()),
                archive_script: None,
                quick_actions: vec![QuickAction {
                    name: "test".to_owned(),
                    command: "cargo test".to_owned(),
                    auto_run_on_create: false,
                }],
                created_at: at(1_756_600_000),
            },
        ),
        fixture(
            "workspace",
            "workspace",
            &CodeWorkspaceSnapshot {
                id: workspace_id(),
                repo_id: repo_id(),
                title: "Bound the code parser".to_owned(),
                worktree_path: "/Users/mara/code/tidebreak/.tidebreak/wt-2".to_owned(),
                branch_name: "mara/code-parsers-bounded".to_owned(),
                base_ref: "main".to_owned(),
                status: CodeWorkspaceStatus::Active,
                pr: Some(pull_request()),
                created_at: at(1_756_690_000),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
        ),
        fixture(
            "released workspace",
            "workspace",
            &CodeWorkspaceSnapshot {
                id: WorkspaceId(id(0x12)),
                repo_id: repo_id(),
                title: "Old work".to_owned(),
                worktree_path: "/Users/mara/code/tidebreak/.tidebreak/wt-1".to_owned(),
                branch_name: "mara/old-work".to_owned(),
                base_ref: "main".to_owned(),
                status: CodeWorkspaceStatus::Released,
                pr: None,
                created_at: at(1_756_000_000),
                archived_at: Some(at(1_756_100_000)),
                released_at: Some(at(1_756_200_000)),
                released_tip: Some("0bace692f4b5a7e3d2c1f0a9b8c7d6e5f4a3b2c1".to_owned()),
                bundle_bytes: Some(48_213),
            },
        ),
        fixture("session", "session", &session()),
        fixture(
            "fenced session",
            "session",
            &CodeSessionSnapshot {
                workspace_id: None,
                harness_kind: HarnessKind::Internal,
                lifecycle: CodeSessionLifecycle::Fenced,
                fence_reason: Some(FenceReason::ResumeLost {
                    detail: "the engine forgot the resume ref".to_owned(),
                }),
                attention: Attention {
                    state: AttentionState::Fenced {
                        reason: FenceReason::ResumeLost {
                            detail: "the engine forgot the resume ref".to_owned(),
                        },
                    },
                    source: AttentionSource::Lifecycle,
                },
                external_origin: None,
                ..session()
            },
        ),
        fixture("turn", "turn", &turn()),
        fixture("queued turn", "queued_turn", &queued_turn()),
        fixture(
            "queued turns",
            "queued_turns",
            &QueuedCodeTurnsSnapshot {
                queued: vec![queued_turn()],
                paused: true,
            },
        ),
        fixture(
            "harness doctor",
            "harness_doctor",
            &HarnessDoctorReport {
                harnesses: vec![
                    HarnessDoctorEntry {
                        kind: HarnessKind::ClaudeCode,
                        found: true,
                        installable: true,
                        path: Some("/opt/homebrew/bin/claude".to_owned()),
                        version: Some("2.0.14".to_owned()),
                        tier: HarnessTier::Reference,
                        caps: caps(),
                        commands: vec![HarnessCommand {
                            name: "review".to_owned(),
                            description: "Review the pending changes".to_owned(),
                        }],
                        authenticated: Some(true),
                        auth_mode: HarnessAuthMode::LocalSignIn,
                        remediation: String::new(),
                        stderr: String::new(),
                        unrecognized_event_count: 0,
                        relaunch_composes_permission_mode: true,
                    },
                    HarnessDoctorEntry {
                        kind: HarnessKind::Grok,
                        found: false,
                        installable: false,
                        path: None,
                        version: None,
                        tier: HarnessTier::BestEffort,
                        caps: caps(),
                        commands: Vec::new(),
                        authenticated: None,
                        auth_mode: HarnessAuthMode::HostedUnavailable,
                        remediation: "Install grok-build and sign in.".to_owned(),
                        stderr: "grok: command not found".to_owned(),
                        unrecognized_event_count: 2,
                        relaunch_composes_permission_mode: false,
                    },
                ],
            },
        ),
        fixture(
            "workspace files",
            "workspace_files",
            &CodeWorkspaceFiles {
                files: vec![
                    CodeFileChange {
                        path: "crates/tidebreak-cli/src/api/code.rs".to_owned(),
                        kind: FileChangeKind::Modified,
                        insertions: 12,
                        deletions: 3,
                        previous_path: None,
                    },
                    CodeFileChange {
                        path: "crates/tidebreak-server/fixtures/code-frames.json".to_owned(),
                        kind: FileChangeKind::Renamed,
                        insertions: 2,
                        deletions: 0,
                        previous_path: Some(
                            "crates/tidebreak-server/fixtures/code.json".to_owned(),
                        ),
                    },
                ],
                truncated: false,
                stat: diffstat(),
                turn_id: Some(turn_id()),
            },
        ),
        fixture(
            "workspace diff",
            "workspace_diff",
            &CodeWorkspaceDiff {
                diff: "--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n".to_owned(),
                truncated: false,
                stat: Diffstat {
                    files: 1,
                    insertions: 1,
                    deletions: 1,
                    truncated: false,
                },
                turn_id: None,
                file: Some("README.md".to_owned()),
            },
        ),
        fixture(
            "pending approval",
            "approval",
            &CodeApprovalSnapshot {
                id: approval_id(),
                session_id: session_id(),
                turn_id: turn_id(),
                kind: CodeApprovalKind::Command {
                    cmd: "cargo test -p tidebreak-cli".to_owned(),
                    cwd: Some("/Users/mara/code/tidebreak".to_owned()),
                },
                harness_raw_json: r#"{"tool":"Bash","command":"cargo test -p tidebreak-cli"}"#
                    .to_owned(),
                state: CodeApprovalState::Pending,
                feedback: None,
                requested_at: at(1_756_700_120),
                decided_at: None,
            },
        ),
        fixture(
            "denied approval",
            "approval",
            &CodeApprovalSnapshot {
                id: CodeApprovalId(id(0x15)),
                session_id: session_id(),
                turn_id: turn_id(),
                kind: CodeApprovalKind::FileWrite {
                    paths: vec!["/etc/hosts".to_owned()],
                },
                harness_raw_json: r#"{"tool":"Write","path":"/etc/hosts"}"#.to_owned(),
                state: CodeApprovalState::Denied,
                feedback: Some("Not that file.".to_owned()),
                requested_at: at(1_756_700_130),
                decided_at: Some(at(1_756_700_140)),
            },
        ),
        fixture(
            "commit",
            "commit",
            &CodeCommitSnapshot {
                sha: "cec166ffc1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6".to_owned(),
                message: "fix(desktop): bound every code-mode string".to_owned(),
                stat: diffstat(),
            },
        ),
        fixture(
            "push",
            "push",
            &CodePushSnapshot {
                branch: "mara/code-parsers-bounded".to_owned(),
                remote: "origin".to_owned(),
            },
        ),
        fixture(
            "workspace pr",
            "workspace_pr",
            &CodeWorkspacePrSnapshot {
                dirty: false,
                unpushed: true,
                ahead: 2,
                has_upstream: true,
                suggested_commit_message: "fix(desktop): bound every code-mode string".to_owned(),
                pr: Some(pull_request()),
                gh_found: true,
                gh_authenticated: Some(true),
                remediation: String::new(),
                pushes_as: Some("mara".to_owned()),
                pushes_as_self: Some(true),
                watch: Some(CodeWatchSnapshot {
                    id: CodeWatchId(id(0x20)),
                    workspace_id: workspace_id(),
                    session_id: CodeSessionId(id(0x21)),
                    pr_number: 3006,
                    state: CodeWatchState::Watching,
                    detail: Some("waiting on the desktop UI lane".to_owned()),
                    cycles: 4,
                    created_at: at(1_756_700_200),
                    updated_at: at(1_756_700_260),
                }),
            },
        ),
        fixture(
            "action",
            "action",
            &CodeActionSnapshot {
                name: "test".to_owned(),
                success: false,
                exit_code: Some(101),
                stdout: "running 3 tests\n".to_owned(),
                stderr: "test parsers::bounds ... FAILED\n".to_owned(),
                timed_out: false,
            },
        ),
        fixture("session digest", "session_digest", &digest()),
        fixture(
            "watch digest",
            "session_digest",
            &CodeSessionDigest {
                session: CodeSessionId(id(0x21)),
                kind: CodeSessionKind::Watch,
                title: "Watch #3006".to_owned(),
                activity: None,
                pr_count: None,
                watch_state: Some(CodeWatchState::Fixing),
                watch_detail: Some("addressing the review".to_owned()),
                watch_cycles: Some(4),
                subagents: None,
                recap: None,
                ..digest()
            },
        ),
        fixture(
            "internal session digest",
            "session_digest",
            &internal_digest(),
        ),
    ];
    out.extend(update_notices());
    out.extend(event_frames());
    out
}

/// One notice per variant of [`CodeUpdateNotice`].
fn update_notices() -> Vec<Fixture> {
    vec![
        fixture(
            "updates: snapshot",
            "update_notice",
            &CodeUpdateNotice::Snapshot {
                sessions: vec![digest(), internal_digest()],
            },
        ),
        fixture("updates: digest", "update_notice", &digest_notice(digest())),
        fixture(
            "updates: internal session digest",
            "update_notice",
            &digest_notice(internal_digest()),
        ),
        fixture(
            "updates: terminal activity",
            "update_notice",
            &CodeUpdateNotice::TerminalActivity {
                workspace_id: workspace_id(),
                terminal_id: CodeTerminalId(id(0x40)),
            },
        ),
        fixture(
            "updates: clone progress",
            "update_notice",
            &CodeUpdateNotice::CloneProgress {
                job: "clone-7".to_owned(),
                phase: "receiving objects".to_owned(),
                percent: Some(62),
                done: false,
                error: None,
                repo_id: Some(repo_id()),
            },
        ),
        fixture(
            "updates: harness install",
            "update_notice",
            &CodeUpdateNotice::HarnessInstall {
                kind: HarnessKind::Codex,
                version: Some("0.42.0".to_owned()),
                phase: "failed".to_owned(),
                done: true,
                error: Some("checksum mismatch".to_owned()),
            },
        ),
        fixture(
            "updates: delivery",
            "update_notice",
            &CodeUpdateNotice::Delivery,
        ),
        fixture(
            "updates: turn rewrite",
            "update_notice",
            &CodeUpdateNotice::TurnRewrite {
                session: session_id(),
                turn_id: turn_id(),
                state: CodeTurnRewriteState::Rewritten,
                rewrite: Some("Every code-mode string now has a bound.".to_owned()),
            },
        ),
    ]
}

/// One frame per variant of [`CodeEvent`], plus the frame flags a reader
/// has to honor: `replayed`, `transient` with `replacement`, and `truncated`.
fn event_frames() -> Vec<Fixture> {
    let started = CodeEvent::SessionStarted {
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: "2.0.14".to_owned(),
        resume_ref: Some("9f2c1d4e-resume".to_owned()),
    };
    let frames = vec![
        (
            "event: session_started (replayed, truncated)",
            SequencedCodeEventFrame {
                replayed: Some(true),
                truncated: Some(true),
                ..frame(40, started)
            },
        ),
        (
            "event: turn_started",
            SequencedCodeEventFrame {
                replayed: Some(true),
                ..frame(41, CodeEvent::TurnStarted { turn_id: turn_id() })
            },
        ),
        (
            "event: assistant_delta (transient replacement)",
            SequencedCodeEventFrame {
                transient: Some(true),
                replacement: Some(true),
                ..frame(
                    41,
                    CodeEvent::AssistantDelta {
                        text: "Reading the parser".to_owned(),
                    },
                )
            },
        ),
        (
            "event: assistant_message",
            frame(
                42,
                CodeEvent::AssistantMessage {
                    text: "The parser checks presence only.".to_owned(),
                    parent_call_id: None,
                },
            ),
        ),
        (
            "event: reasoning_delta",
            frame(
                43,
                CodeEvent::ReasoningDelta {
                    text: "Which fields are drawn on one line?".to_owned(),
                },
            ),
        ),
        (
            "event: tool_started",
            frame(
                44,
                CodeEvent::ToolStarted {
                    call_id: "call-1".to_owned(),
                    name: "Bash".to_owned(),
                    detail: ToolDetail::Command {
                        cmd: "cargo test -p tidebreak-cli".to_owned(),
                        cwd: "/Users/mara/code/tidebreak".to_owned(),
                    },
                    parent_call_id: None,
                },
            ),
        ),
        (
            "event: tool_completed",
            frame(
                45,
                CodeEvent::ToolCompleted {
                    call_id: "call-2".to_owned(),
                    outcome: ToolOutcome::Succeeded,
                    preview: "1 file changed".to_owned(),
                    output: None,
                    action: None,
                    result: None,
                    detail: Some(ToolDetail::FileEdit {
                        path: "crates/tidebreak-cli/src/api/code.rs".to_owned(),
                    }),
                    parent_call_id: Some("call-1".to_owned()),
                },
            ),
        ),
        (
            "event: file_changed",
            frame(
                46,
                CodeEvent::FileChanged {
                    path: "crates/tidebreak-cli/src/api/code.rs".to_owned(),
                    kind: FileChangeKind::Modified,
                    diffstat: diffstat(),
                },
            ),
        ),
        (
            "event: approval_requested",
            frame(
                47,
                CodeEvent::ApprovalRequested {
                    approval_id: approval_id(),
                    request: None,
                },
            ),
        ),
        (
            "event: approval_resolved",
            frame(
                48,
                CodeEvent::ApprovalResolved {
                    approval_id: approval_id(),
                    decision: ApprovalDecisionKind::Deny {
                        feedback: Some("Not that file.".to_owned()),
                    },
                },
            ),
        ),
        (
            "event: user_steered",
            frame(
                49,
                CodeEvent::UserSteered {
                    text: "Keep the raw tier for diffs.".to_owned(),
                    message_id: None,
                },
            ),
        ),
        (
            "event: turn_completed",
            frame(
                50,
                CodeEvent::TurnCompleted {
                    usage: usage(),
                    checkpoint: Some(CheckpointHint {
                        checkpoint_ref: Some("refs/tidebreak/checkpoints/3".to_owned()),
                        diffstat: Some(diffstat()),
                    }),
                    stop_reason: None,
                },
            ),
        ),
        (
            "event: turn_failed",
            frame(
                51,
                CodeEvent::TurnFailed {
                    error: BoundedError {
                        message: "the engine exited with status 1".to_owned(),
                    },
                    detail: None,
                },
            ),
        ),
        (
            "event: turn_interrupted",
            frame(52, CodeEvent::TurnInterrupted { usage: None }),
        ),
        (
            "event: checkpoint_recorded",
            frame(
                53,
                CodeEvent::CheckpointRecorded {
                    turn_id: turn_id(),
                    diffstat: diffstat(),
                },
            ),
        ),
        (
            "event: harness_notice",
            frame(
                54,
                CodeEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message: "context is 80% full".to_owned(),
                },
            ),
        ),
        (
            "event: attention_changed",
            frame(
                55,
                CodeEvent::AttentionChanged {
                    state: AttentionState::NeedsYou {
                        prompt: "Approve the write to /etc/hosts?".to_owned(),
                        source: AttentionSource::Structured,
                    },
                    source: AttentionSource::Structured,
                },
            ),
        ),
        // The internal engine's own rows: what the chat lane journals when a
        // session with no workspace runs.
        (
            "event: turn_refused (internal engine)",
            frame(
                56,
                CodeEvent::TurnRefused {
                    usage: usage(),
                    refusal: RefusalOutcome::new(
                        RefusalDetails::from_category(Some("safety")),
                        true,
                    ),
                },
            ),
        ),
        (
            "event: stream_interrupted (internal engine)",
            SequencedCodeEventFrame {
                transient: Some(true),
                ..frame(57, CodeEvent::StreamInterrupted)
            },
        ),
        (
            "event: tool_args_delta (internal engine, transient)",
            SequencedCodeEventFrame {
                transient: Some(true),
                ..frame(
                    57,
                    CodeEvent::ToolArgsDelta {
                        call_id: "call_01".to_owned(),
                        fragment: "{\"command\":\"cargo\",".to_owned(),
                    },
                )
            },
        ),
        (
            "event: approval_requested (internal engine consent card)",
            frame(
                58,
                CodeEvent::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::ToolUse {
                        auto_judging: false,
                        tool_name: "exec".to_owned(),
                        class: ApprovalClass::Workspace,
                        approval: ToolApprovalKind::ExecMayRunNetworkedCommand,
                        grant_scopes: vec![GrantScope::AnyArgsFor {
                            command: "cargo".to_owned(),
                        }],
                        preview: None,
                    }),
                },
            ),
        ),
        (
            "event: approval_requested (internal engine questions park)",
            frame(
                59,
                CodeEvent::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::Questions { turn_id: turn_id() }),
                },
            ),
        ),
        (
            "event: approval_requested (internal engine plan park)",
            frame(
                60,
                CodeEvent::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::Plan { turn_id: turn_id() }),
                },
            ),
        ),
        (
            "event: task_plan_updated (internal engine)",
            frame(
                62,
                CodeEvent::TaskPlanUpdated {
                    call_id: "call_04".to_owned(),
                    turn_id: turn_id(),
                },
            ),
        ),
        (
            "event: context_truncated (internal engine)",
            frame(
                63,
                CodeEvent::ContextTruncated {
                    original_tokens: 210_000,
                    fitted_tokens: 180_000,
                },
            ),
        ),
        (
            "event: compaction_started (internal engine)",
            frame(64, CodeEvent::CompactionStarted),
        ),
        (
            "event: compaction_finished (internal engine)",
            frame(65, CodeEvent::CompactionFinished { compacted: true }),
        ),
    ];
    frames
        .into_iter()
        .map(|(name, frame)| fixture(name, "event_frame", &frame))
        .collect()
}

/// The checked-in fixture file: a JSON array of `{ "name", "kind", "value" }`.
fn rendered_code_frames() -> String {
    let entries = code_frame_fixtures()
        .into_iter()
        .map(|entry| {
            serde_json::json!({ "name": entry.name, "kind": entry.kind, "value": entry.value })
        })
        .collect::<Vec<_>>();
    let mut rendered = serde_json::to_string_pretty(&entries).expect("the fixture list serializes");
    rendered.push('\n');
    rendered
}

/// Tags of a `#[serde(tag = "type")]` union, read from its generated
/// declaration rather than a hand-kept list.
fn declared_tags<T: ts_rs::TS + 'static>(name: &str) -> std::collections::BTreeSet<String> {
    let cfg = generate::config();
    let mut declarations = std::collections::BTreeMap::new();
    generate::collect_from::<T>(&cfg, &mut declarations);
    let declaration = &declarations[name];
    let tags: std::collections::BTreeSet<String> = declaration
        .split("\"type\": \"")
        .skip(1)
        .map(|rest| rest.split('"').next().expect("a closed tag").to_owned())
        .collect();
    assert!(
        tags.len() > 3,
        "the {name} union parse found too few tags: {tags:?}"
    );
    tags
}

fn fixture_tags(
    kind: &str,
    tag_at: fn(&serde_json::Value) -> Option<&serde_json::Value>,
) -> std::collections::BTreeSet<String> {
    code_frame_fixtures()
        .iter()
        .filter(|entry| entry.kind == kind)
        .filter_map(|entry| tag_at(&entry.value))
        .filter_map(|tag| tag.as_str())
        .map(str::to_owned)
        .collect()
}

fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(entry: &Fixture) {
    let decoded: T = serde_json::from_value(entry.value.clone())
        .unwrap_or_else(|error| panic!("fixture {} does not decode: {error}", entry.name));
    let again = serde_json::to_value(&decoded).expect("a decoded value serializes");
    assert_eq!(
        again, entry.value,
        "fixture {} changed across the round trip",
        entry.name
    );
}

/// Three decoders read these bytes. A diff here means the code surface's
/// shape changed, and every client test that consumes the file re-runs
/// against the new shape.
#[test]
fn the_code_frame_fixtures_are_current() {
    generate::check_or_update(CODE_FRAMES, &rendered_code_frames(), REGENERATE);
}

/// Every event variant and every update-notice variant has a fixture, so a
/// new variant fails here until it has one.
#[test]
fn the_code_frame_fixtures_cover_every_event() {
    let declared = declared_tags::<CodeEvent>("CodeEvent");
    let covered = fixture_tags("event_frame", |value| value.get("event")?.get("type"));
    let missing: Vec<_> = declared.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "event types without a code-frame fixture: {missing:?}"
    );

    let declared = declared_tags::<CodeUpdateNotice>("CodeUpdateNotice");
    let covered = fixture_tags("update_notice", |value| value.get("type"));
    let missing: Vec<_> = declared.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "update notices without a code-frame fixture: {missing:?}"
    );
}

/// Every fixture kind has a decoder here, so a new kind fails until the
/// server can read what it wrote.
#[test]
fn every_code_frame_fixture_round_trips() {
    for entry in &code_frame_fixtures() {
        match entry.kind {
            "repo" => round_trip::<CodeRepoSnapshot>(entry),
            "workspace" => round_trip::<CodeWorkspaceSnapshot>(entry),
            "session" => round_trip::<CodeSessionSnapshot>(entry),
            "turn" => round_trip::<CodeTurnSnapshot>(entry),
            "queued_turn" => round_trip::<QueuedCodeTurn>(entry),
            "queued_turns" => round_trip::<QueuedCodeTurnsSnapshot>(entry),
            "harness_doctor" => round_trip::<HarnessDoctorReport>(entry),
            "workspace_files" => round_trip::<CodeWorkspaceFiles>(entry),
            "workspace_diff" => round_trip::<CodeWorkspaceDiff>(entry),
            "approval" => round_trip::<CodeApprovalSnapshot>(entry),
            "commit" => round_trip::<CodeCommitSnapshot>(entry),
            "push" => round_trip::<CodePushSnapshot>(entry),
            "workspace_pr" => round_trip::<CodeWorkspacePrSnapshot>(entry),
            "action" => round_trip::<CodeActionSnapshot>(entry),
            "session_digest" => round_trip::<CodeSessionDigest>(entry),
            "update_notice" => round_trip::<CodeUpdateNotice>(entry),
            "event_frame" => round_trip::<SequencedCodeEventFrame>(entry),
            other => panic!("fixture {} has no decoder for kind {other}", entry.name),
        }
    }
}

/// Unknown keys fail the value, at the snapshot, the notice, and the frame.
///
/// The one gap is serde's: a payload-less notice (`{"type":"delivery"}`) is
/// a unit variant of an internally tagged enum, and serde reads no fields for
/// it, so `deny_unknown_fields` has nothing to check. A stray key beside the
/// tag is accepted there. The renderer's `onlyKeys` guard still rejects it.
#[test]
fn code_values_reject_unknown_keys() {
    for entry in code_frame_fixtures() {
        if entry.kind == "update_notice"
            && entry.value.as_object().map(serde_json::Map::len) == Some(1)
        {
            continue;
        }
        let mut value = entry.value.clone();
        let object = match entry.kind {
            "event_frame" => value["event"].as_object_mut(),
            _ => value.as_object_mut(),
        }
        .expect("every fixture is an object");
        object.insert("extra".to_owned(), serde_json::Value::Bool(true));
        let rejected = match entry.kind {
            "repo" => serde_json::from_value::<CodeRepoSnapshot>(value).is_err(),
            "workspace" => serde_json::from_value::<CodeWorkspaceSnapshot>(value).is_err(),
            "session" => serde_json::from_value::<CodeSessionSnapshot>(value).is_err(),
            "turn" => serde_json::from_value::<CodeTurnSnapshot>(value).is_err(),
            "queued_turn" => serde_json::from_value::<QueuedCodeTurn>(value).is_err(),
            "queued_turns" => serde_json::from_value::<QueuedCodeTurnsSnapshot>(value).is_err(),
            "harness_doctor" => serde_json::from_value::<HarnessDoctorReport>(value).is_err(),
            "workspace_files" => serde_json::from_value::<CodeWorkspaceFiles>(value).is_err(),
            "workspace_diff" => serde_json::from_value::<CodeWorkspaceDiff>(value).is_err(),
            "approval" => serde_json::from_value::<CodeApprovalSnapshot>(value).is_err(),
            "commit" => serde_json::from_value::<CodeCommitSnapshot>(value).is_err(),
            "push" => serde_json::from_value::<CodePushSnapshot>(value).is_err(),
            "workspace_pr" => serde_json::from_value::<CodeWorkspacePrSnapshot>(value).is_err(),
            "action" => serde_json::from_value::<CodeActionSnapshot>(value).is_err(),
            "session_digest" => serde_json::from_value::<CodeSessionDigest>(value).is_err(),
            "update_notice" => serde_json::from_value::<CodeUpdateNotice>(value).is_err(),
            // The event union is `tidebreak_core`'s and tolerates extra keys
            // inside a variant; the frame around it does not.
            "event_frame" => {
                let mut frame = entry.value.clone();
                frame["extra"] = serde_json::Value::Bool(true);
                serde_json::from_value::<SequencedCodeEventFrame>(frame).is_err()
            }
            other => panic!("fixture {} has no decoder for kind {other}", entry.name),
        };
        assert!(rejected, "fixture {} accepted an unknown key", entry.name);
    }
}

/// A notice tag this build does not know fails the notice rather than
/// folding to something a client would misread.
#[test]
fn unknown_update_notices_fail() {
    let unknown = r#"{"type":"some_future_notice","extra":true}"#;
    assert!(serde_json::from_str::<CodeUpdateNotice>(unknown).is_err());
    let unknown_event = r#"{"seq":9,"event":{"type":"some_future_event"}}"#;
    assert!(serde_json::from_str::<SequencedCodeEventFrame>(unknown_event).is_err());
}
