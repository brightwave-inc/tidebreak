//! Shared fixtures for the code-mode wire surface.
//!
//! `fixtures/code-frames.json` holds one real value of every snapshot the code
//! routes return, every notice on `/updates`, and every event the
//! per-session socket can carry, serialized from the server's own types. Three
//! decoders read the file: this crate's round trip, the CLI's
//! `api::code` tests, and the renderer's `code/parsers.test.ts`. A shape
//! change shows up as a failing test on whichever side did not follow it.
//!
//! Regenerate with `UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server`, the
//! same switch that rewrites `wire.ts`.

use crate::wire::{
    ApprovalSnapshot, CodeActionSnapshot, CodeCheckpointRestorePreview,
    CodeCheckpointRestoreResult, CodeCommitSnapshot, CodeFileChange, CodeProjectConfigEffect,
    CodeProjectConfigEffectKind, CodeProjectConfigFile, CodePushSnapshot, CodeRepoSnapshot,
    CodeRepoTrust, CodeRepoTrustSnapshot, CodeRestoreAffectedTurn, CodeWatchSnapshot,
    CodeWorkspaceDiff, CodeWorkspaceFiles, CodeWorkspaceGitState, CodeWorkspacePrSnapshot,
    CodeWorkspaceSnapshot, HarnessAuthMode, HarnessDoctorEntry, HarnessDoctorReport, QueuedTurn,
    QueuedTurnsSnapshot, SequencedEventFrame, SessionDigest, SessionExternalOrigin,
    SessionSnapshot, TurnRewriteState, TurnSnapshot, UpdateNotice,
};
use crate::wire_types::generate;
use tidebreak_core::{
    ApprovalClass, ApprovalDecisionKind, ApprovalId, ApprovalKind, ApprovalState, Attention,
    AttentionSource, AttentionState, BoundedError, CapLevel, CheckpointHint,
    CheckpointRestoreTarget, CodeRestoreId, CodeSubagentStatus, CodeSubagentSummary,
    CodeTerminalId, CodeWatchId, CodeWatchState, CodeWorkspaceStatus, CredentialRefusalReason,
    Diffstat, Event, FenceReason, FileChangeKind, GrantScope, HarnessCaps, HarnessCommand,
    HarnessKind, HarnessNoticeLevel, HarnessTier, ImageMediaType, ImageRef,
    InternalApprovalRequest, PermissionMode, PullRequestCheckCounts, PullRequestDigest,
    QuickAction, ReasoningEffort, RefusalOutcome, RepoId, SessionActivity, SessionId, SessionKind,
    SessionLifecycle, ToolApprovalKind, ToolDetail, ToolOutcome, TurnId, TurnStatus, TurnUsage,
    WorkspaceId,
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

fn session_id() -> SessionId {
    SessionId(id(0x03))
}

fn turn_id() -> TurnId {
    TurnId(id(0x04))
}

fn approval_id() -> ApprovalId {
    ApprovalId(id(0x05))
}

fn restore_id() -> CodeRestoreId {
    CodeRestoreId(id(0x30))
}

fn diffstat() -> Diffstat {
    Diffstat {
        files: 2,
        insertions: 14,
        deletions: 3,
        truncated: false,
    }
}

fn usage() -> TurnUsage {
    TurnUsage {
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
        mid_turn_resume: CapLevel::Unsupported,
        transcript: CapLevel::Unsupported,
        memory_loopback: CapLevel::Unsupported,
    }
}

fn session() -> SessionSnapshot {
    SessionSnapshot {
        inference_resolutions: None,
        access: Some(tidebreak_core::SessionAccessLevel::View),
        is_owner: Some(false),
        visibility: tidebreak_core::SessionVisibility::Private,
        id: session_id(),
        owner_kind: Some("service".to_owned()),
        workspace_id: Some(workspace_id()),
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: Some("2.0.14".to_owned()),
        harness_resume_ref: Some("9f2c1d4e-resume".to_owned()),
        permission_mode: PermissionMode::Ask,
        model: Some("claude-opus-5".to_owned()),
        reasoning_effort: Some(ReasoningEffort::High),
        fast_mode: false,
        lifecycle: SessionLifecycle::Running,
        fence_reason: None,
        attention: attention(),
        unrecognized_event_count: 0,
        created_at: at(1_756_700_000),
        execution_location: tidebreak_core::ExecutionLocation::Sandbox,
        acts_as: Some(tidebreak_core::ActsAs::Bot),
        external_origins: Some(vec![
            SessionExternalOrigin {
                channel_kind: "slack".to_owned(),
                external_key: "T0/C1/1756700000.000100".to_owned(),
            },
            SessionExternalOrigin {
                channel_kind: "slack".to_owned(),
                external_key: "T0/C2/1756700001.000100".to_owned(),
            },
        ]),
        external_origin: Some(SessionExternalOrigin {
            channel_kind: "slack".to_owned(),
            external_key: "T0/C1/1756700000.000100".to_owned(),
        }),
        children: Vec::new(),
        wait: None,
        parent_session_id: None,
    }
}

fn turn() -> TurnSnapshot {
    TurnSnapshot {
        actor: Some(tidebreak_core::TurnActor {
            principal: Some("mara".into()),
            display: Some("Mara".into()),
            channel_kind: None,
            external_identity: None,
            trigger: None,
        }),
        id: turn_id(),
        session_id: session_id(),
        ordinal: 3,
        status: TurnStatus::Completed,
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

fn queued_turn() -> QueuedTurn {
    QueuedTurn {
        id: TurnId(id(0x06)),
        session_id: session_id(),
        message: "Then add the fixture test.".to_owned(),
        // A trigger-parked row, so the fixture exercises the structured
        // event the tray renders (decision 60 via decision 69).
        actor: Some(tidebreak_core::TurnActor {
            display: Some("Trigger: checks_failed".to_owned()),
            trigger: Some(tidebreak_core::TriggerTurnContext {
                source: tidebreak_core::TriggerTurnSource::Trigger,
                condition: tidebreak_core::CodeTriggerCondition::ChecksFailed,
                pr_number: 3411,
                pr_title: Some("Bound the code parser".to_owned()),
                pr_url: Some("https://github.com/acme/tidebreak/pull/3411".to_owned()),
                head_sha: Some("0f0e0d0c0b0a0908".to_owned()),
                failing_checks: vec![tidebreak_core::PullRequestCheck {
                    name: "desktop-ui".to_owned(),
                    bucket: tidebreak_core::PullRequestCheckBucket::Fail,
                    detail: None,
                    url: Some("https://github.com/acme/tidebreak/actions/runs/1".to_owned()),
                }],
            }),
            ..Default::default()
        }),
        position: 0,
        created_at: at(1_756_700_170),
        updated_at: at(1_756_700_170),
    }
}

fn digest() -> SessionDigest {
    SessionDigest {
        external_origin: None,
        can_open_chat: None,
        workspace: Some(workspace_id()),
        session: session_id(),
        kind: SessionKind::Interactive,
        harness_kind: Some(HarnessKind::ClaudeCode),
        lifecycle: SessionLifecycle::Running,
        attention: attention(),
        fence_reason: None,
        title: "Bound the code parser".to_owned(),
        turn_count: 3,
        trigger_target_at: Some(at(1_756_700_100)),
        activity: Some(SessionActivity::Shell),
        activity_detail: Some("cargo test -p tidebreak-server code_parser".to_owned()),
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
        memory_proposal_count: None,
        parent_session: None,
        wait: None,
    }
}

/// The in-process engine's session binds no workspace (decision 0048 step 5).
fn internal_digest() -> SessionDigest {
    SessionDigest {
        can_open_chat: None,
        workspace: None,
        session: SessionId(id(0x22)),
        harness_kind: Some(HarnessKind::Internal),
        title: "Plan the memory substrate".to_owned(),
        turn_count: 1,
        activity: Some(SessionActivity::Agent),
        activity_detail: None,
        pr_state: None,
        pr_count: None,
        subagents: None,
        recap: None,
        ..digest()
    }
}

fn digest_notice(d: SessionDigest) -> UpdateNotice {
    UpdateNotice::Digest {
        workspace: d.workspace,
        session: d.session,
        kind: d.kind,
        harness_kind: d.harness_kind,
        lifecycle: d.lifecycle,
        fence_reason: d.fence_reason.map(Box::new),
        attention: Box::new(d.attention),
        title: d.title,
        external_origin: d.external_origin.map(Box::new),
        turn_count: d.turn_count,
        trigger_target_at: d.trigger_target_at,
        activity: d.activity,
        activity_detail: d.activity_detail,
        pr_state: d.pr_state.map(Box::new),
        pr_count: d.pr_count,
        watch_state: d.watch_state,
        watch_detail: d.watch_detail,
        watch_cycles: d.watch_cycles,
        subagents: d.subagents,
        recap: d.recap,
        memory_proposal_count: d.memory_proposal_count,
        parent_session: d.parent_session,
        wait: d.wait,
    }
}

fn frame(seq: i64, event: Event) -> SequencedEventFrame {
    SequencedEventFrame {
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
            "repo trust",
            "repo_trust",
            &CodeRepoTrustSnapshot {
                repo_id: repo_id(),
                trust: CodeRepoTrust::Undecided,
                files: vec![
                    CodeProjectConfigFile {
                        path: ".claude/settings.json".to_owned(),
                        engines: vec![HarnessKind::ClaudeCode],
                        effects: vec![
                            CodeProjectConfigEffect {
                                kind: CodeProjectConfigEffectKind::Hooks,
                                count: 2,
                            },
                            CodeProjectConfigEffect {
                                kind: CodeProjectConfigEffectKind::EnvironmentVariables,
                                count: 1,
                            },
                        ],
                    },
                    CodeProjectConfigFile {
                        path: ".opencode/plugin/".to_owned(),
                        engines: vec![HarnessKind::Opencode],
                        effects: Vec::new(),
                    },
                ],
            },
        ),
        fixture(
            "workspace",
            "workspace",
            &CodeWorkspaceSnapshot {
                read_only: Some(true),
                is_owner: Some(false),
                base_refresh_warning: None,
                id: workspace_id(),
                repo_id: repo_id(),
                repo_display_name: Some("brightwave-inc/tidebreak".to_owned()),
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
                setup_error: None,
            },
        ),
        fixture(
            "released workspace",
            "workspace",
            &CodeWorkspaceSnapshot {
                read_only: Some(true),
                is_owner: Some(false),
                base_refresh_warning: None,
                id: WorkspaceId(id(0x12)),
                repo_id: repo_id(),
                repo_display_name: None,
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
                setup_error: None,
            },
        ),
        fixture("session", "session", &session()),
        fixture(
            "fenced session",
            "session",
            &SessionSnapshot {
                workspace_id: None,
                harness_kind: HarnessKind::Internal,
                lifecycle: SessionLifecycle::Fenced,
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
            &QueuedTurnsSnapshot {
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
                        pinned_version: Some("2.1.259".to_owned()),
                        managed_version: Some("2.1.259".to_owned()),
                        latest_version: Some("2.1.259".to_owned()),
                        update_available: false,
                        sign_in_command: Some("claude auth login".to_owned()),
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
                        auth_mode: HarnessAuthMode::GatewayRelay,
                        remediation: "Install grok-build and sign in.".to_owned(),
                        stderr: "grok: command not found".to_owned(),
                        unrecognized_event_count: 2,
                        relaunch_composes_permission_mode: false,
                        pinned_version: None,
                        managed_version: None,
                        latest_version: None,
                        update_available: false,
                        sign_in_command: None,
                    },
                ],
                update_channel: tidebreak_core::HarnessUpdateChannel::Latest,
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
                        uncommitted: false,
                    },
                    CodeFileChange {
                        path: "crates/tidebreak-server-api/fixtures/code-frames.json".to_owned(),
                        kind: FileChangeKind::Renamed,
                        insertions: 2,
                        deletions: 0,
                        previous_path: Some(
                            "crates/tidebreak-server-api/fixtures/code.json".to_owned(),
                        ),
                        uncommitted: false,
                    },
                ],
                truncated: false,
                stat: diffstat(),
                turn_id: Some(turn_id()),
                revision: None,
                revision_ref: None,
                revision_saved_at: None,
                worktree_tree: None,
            },
        ),
        fixture(
            "uncommitted workspace files",
            "workspace_files",
            &CodeWorkspaceFiles {
                files: vec![
                    CodeFileChange {
                        path: "crates/tidebreak-cli/src/api/code.rs".to_owned(),
                        kind: FileChangeKind::Modified,
                        insertions: 12,
                        deletions: 3,
                        previous_path: None,
                        uncommitted: true,
                    },
                    CodeFileChange {
                        path: "docs/code-mode.md".to_owned(),
                        kind: FileChangeKind::Modified,
                        insertions: 4,
                        deletions: 0,
                        previous_path: None,
                        uncommitted: false,
                    },
                ],
                truncated: false,
                stat: diffstat(),
                turn_id: None,
                revision: None,
                revision_ref: None,
                revision_saved_at: None,
                worktree_tree: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned()),
            },
        ),
        fixture(
            "checkpoint restore preview",
            "checkpoint_restore_preview",
            &CodeCheckpointRestorePreview {
                target: CheckpointRestoreTarget::BeforeTurn { turn_id: turn_id() },
                session_id: session_id(),
                files: vec![
                    CodeFileChange {
                        path: "src/parser.rs".to_owned(),
                        kind: FileChangeKind::Modified,
                        insertions: 18,
                        deletions: 4,
                        previous_path: None,
                        uncommitted: false,
                    },
                    CodeFileChange {
                        path: "notes/scratch.md".to_owned(),
                        kind: FileChangeKind::Added,
                        insertions: 3,
                        deletions: 0,
                        previous_path: None,
                        uncommitted: false,
                    },
                ],
                truncated: false,
                stat: diffstat(),
                current_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned(),
                blocked: vec![".env".to_owned()],
                affected_turns: vec![CodeRestoreAffectedTurn {
                    session_id: SessionId(id(0x32)),
                    turn_id: TurnId(id(0x33)),
                    ordinal: 4,
                    harness_kind: HarnessKind::Codex,
                }],
            },
        ),
        fixture(
            "checkpoint restore",
            "checkpoint_restore",
            &CodeCheckpointRestoreResult {
                restore_id: restore_id(),
                target: CheckpointRestoreTarget::BeforeRestore {
                    restore_id: CodeRestoreId(id(0x31)),
                },
                session_id: session_id(),
                files: vec![CodeFileChange {
                    path: "src/parser.rs".to_owned(),
                    kind: FileChangeKind::Modified,
                    insertions: 4,
                    deletions: 18,
                    previous_path: None,
                    uncommitted: false,
                }],
                truncated: false,
                stat: diffstat(),
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
                revision: Some(crate::code::types::WorkspaceContentRevision::Live),
                revision_ref: Some("mg-wip/sb-1-i1".to_owned()),
                revision_saved_at: Some(at(1_756_700_120)),
            },
        ),
        fixture(
            "pending approval",
            "approval",
            &ApprovalSnapshot {
                actor: None,
                id: approval_id(),
                session_id: session_id(),
                turn_id: turn_id(),
                kind: ApprovalKind::Command {
                    cmd: "cargo test -p tidebreak-cli".to_owned(),
                    cwd: Some("/Users/mara/code/tidebreak".to_owned()),
                },
                harness_raw_json: r#"{"tool":"Bash","command":"cargo test -p tidebreak-cli"}"#
                    .to_owned(),
                state: ApprovalState::Pending,
                feedback: None,
                requested_at: at(1_756_700_120),
                decided_at: None,
            },
        ),
        fixture(
            "denied approval",
            "approval",
            &ApprovalSnapshot {
                actor: None,
                id: ApprovalId(id(0x15)),
                session_id: session_id(),
                turn_id: turn_id(),
                kind: ApprovalKind::FileWrite {
                    paths: vec!["/etc/hosts".to_owned()],
                },
                harness_raw_json: r#"{"tool":"Write","path":"/etc/hosts"}"#.to_owned(),
                state: ApprovalState::Denied,
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
                remote: None,
                git: Some(CodeWorkspaceGitState {
                    branch: Some("mara/code-parsers-bounded".to_owned()),
                    head_sha: "cec166ffc1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6".to_owned(),
                    base_ref: "main".to_owned(),
                    upstream: Some("origin/mara/code-parsers-bounded".to_owned()),
                    ahead_of_upstream: 2,
                    behind_upstream: 0,
                    changed_files: 1,
                    staged_files: 0,
                    unstaged_files: 1,
                    untracked_files: 0,
                    conflicted_files: 0,
                    branch_has_changes: true,
                }),
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
                    session_id: SessionId(id(0x21)),
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
            "recovery digest with manual attention",
            "session_digest",
            &SessionDigest {
                lifecycle: SessionLifecycle::Fenced,
                attention: Attention::manual("Review these edits"),
                fence_reason: Some(FenceReason::ProbeAmbiguous {
                    detail: "The previous engine process could not be identified".to_owned(),
                }),
                activity: None,
                activity_detail: None,
                ..digest()
            },
        ),
        fixture(
            "watch digest",
            "session_digest",
            &SessionDigest {
                session: SessionId(id(0x21)),
                kind: SessionKind::Watch,
                title: "Watch #3006".to_owned(),
                activity: None,
                activity_detail: None,
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

/// One notice per variant of [`UpdateNotice`].
fn update_notices() -> Vec<Fixture> {
    vec![
        fixture(
            "updates: snapshot",
            "update_notice",
            &UpdateNotice::Snapshot {
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
            &UpdateNotice::TerminalActivity {
                workspace_id: workspace_id(),
                terminal_id: CodeTerminalId(id(0x40)),
            },
        ),
        fixture(
            "updates: files changed",
            "update_notice",
            &UpdateNotice::FilesChanged {
                workspace_id: workspace_id(),
            },
        ),
        fixture(
            "updates: clone progress",
            "update_notice",
            &UpdateNotice::CloneProgress {
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
            &UpdateNotice::HarnessInstall {
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
            &UpdateNotice::Delivery,
        ),
        fixture(
            "updates: turn rewrite",
            "update_notice",
            &UpdateNotice::TurnRewrite {
                session: session_id(),
                turn_id: turn_id(),
                state: TurnRewriteState::Rewritten,
                rewrite: Some("Every code-mode string now has a bound.".to_owned()),
            },
        ),
    ]
}

/// One frame per variant of [`Event`], plus the frame flags a reader
/// has to honor: `replayed`, `transient` with `replacement`, and `truncated`.
fn event_frames() -> Vec<Fixture> {
    let started = Event::SessionStarted {
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: "2.0.14".to_owned(),
        resume_ref: Some("9f2c1d4e-resume".to_owned()),
    };
    let frames = vec![
        (
            "event: session_started (replayed, truncated)",
            SequencedEventFrame {
                replayed: Some(true),
                truncated: Some(true),
                ..frame(40, started)
            },
        ),
        (
            "event: turn_started",
            SequencedEventFrame {
                replayed: Some(true),
                ..frame(41, Event::TurnStarted { turn_id: turn_id() })
            },
        ),
        (
            "event: turn_resumed",
            frame(42, Event::TurnResumed { turn_id: turn_id() }),
        ),
        (
            "event: assistant_delta (transient replacement)",
            SequencedEventFrame {
                transient: Some(true),
                replacement: Some(true),
                ..frame(
                    41,
                    Event::AssistantDelta {
                        text: "Reading the parser".to_owned(),
                    },
                )
            },
        ),
        (
            "event: assistant_message",
            frame(
                42,
                Event::AssistantMessage {
                    text: "The parser checks presence only.".to_owned(),
                    parent_call_id: None,
                },
            ),
        ),
        (
            "event: reasoning_delta",
            frame(
                43,
                Event::ReasoningDelta {
                    text: "Which fields are drawn on one line?".to_owned(),
                },
            ),
        ),
        (
            "event: tool_started",
            frame(
                44,
                Event::ToolStarted {
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
                Event::ToolCompleted {
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
                Event::FileChanged {
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
                Event::ApprovalRequested {
                    approval_id: approval_id(),
                    request: None,
                },
            ),
        ),
        (
            "event: approval_resolved",
            frame(
                48,
                Event::ApprovalResolved {
                    approval_id: approval_id(),
                    decision: ApprovalDecisionKind::Deny {
                        feedback: Some("Not that file.".to_owned()),
                    },
                    actor: Some(tidebreak_core::TurnActor {
                        principal: None,
                        display: Some("Ines".into()),
                        channel_kind: Some("slack".into()),
                        external_identity: Some("U123".into()),
                        trigger: None,
                    }),
                },
            ),
        ),
        (
            "event: user_steered",
            frame(
                49,
                Event::UserSteered {
                    text: "Keep the raw tier for diffs.".to_owned(),
                    message_id: None,
                },
            ),
        ),
        (
            "event: turn_completed",
            frame(
                50,
                Event::TurnCompleted {
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
                Event::TurnFailed {
                    error: BoundedError {
                        message: "the engine exited with status 1".to_owned(),
                    },
                    detail: None,
                },
            ),
        ),
        (
            "event: turn_interrupted",
            frame(52, Event::TurnInterrupted { usage: None }),
        ),
        (
            "event: credential_refused",
            frame(
                66,
                Event::CredentialRefused {
                    reason: CredentialRefusalReason::ConnectionEnded,
                    message: "this external connection has no live gateway delegation; \
                              reconnect it from Slack"
                        .to_owned(),
                    remediation: "Reconnect this session from Slack; a newer connect or a \
                                  revoke ended the one it used."
                        .to_owned(),
                },
            ),
        ),
        (
            "event: checkpoint_recorded",
            frame(
                53,
                Event::CheckpointRecorded {
                    turn_id: turn_id(),
                    diffstat: diffstat(),
                },
            ),
        ),
        (
            "event: checkpoint_restored",
            frame(
                68,
                Event::CheckpointRestored {
                    restore_id: restore_id(),
                    target: CheckpointRestoreTarget::BeforeTurn { turn_id: turn_id() },
                    diffstat: diffstat(),
                    actor: None,
                    status: tidebreak_core::CheckpointRestoreStatus::Completed,
                    error: None,
                },
            ),
        ),
        (
            "event: model_reported",
            frame(
                67,
                Event::ModelReported {
                    model: "selected-model".to_owned(),
                },
            ),
        ),
        (
            "event: harness_notice",
            frame(
                54,
                Event::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message: "context is 80% full".to_owned(),
                },
            ),
        ),
        // The internal engine's own rows: what the chat lane journals when a
        // session with no workspace runs.
        (
            "event: turn_refused (internal engine)",
            frame(
                56,
                Event::TurnRefused {
                    usage: usage(),
                    refusal: RefusalOutcome::report_blocked(),
                },
            ),
        ),
        (
            "event: stream_interrupted (internal engine)",
            SequencedEventFrame {
                transient: Some(true),
                ..frame(57, Event::StreamInterrupted)
            },
        ),
        (
            "event: tool_args_delta (internal engine, transient)",
            SequencedEventFrame {
                transient: Some(true),
                ..frame(
                    57,
                    Event::ToolArgsDelta {
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
                Event::ApprovalRequested {
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
                        preview_truncated: false,
                    }),
                },
            ),
        ),
        (
            "event: approval_requested (internal engine questions park)",
            frame(
                59,
                Event::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::Questions { turn_id: turn_id() }),
                },
            ),
        ),
        (
            "event: approval_requested (internal engine plan park)",
            frame(
                60,
                Event::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::Plan { turn_id: turn_id() }),
                },
            ),
        ),
        (
            "event: approval_requested (truncated preview)",
            frame(
                61,
                Event::ApprovalRequested {
                    approval_id: approval_id(),
                    request: Some(InternalApprovalRequest::ToolUse {
                        auto_judging: false,
                        tool_name: "exec".to_owned(),
                        class: ApprovalClass::Workspace,
                        approval: ToolApprovalKind::ExecMayRunNetworkedCommand,
                        grant_scopes: Vec::new(),
                        preview: Some(tidebreak_core::ToolActionPreview::Exec {
                            command: "x".repeat(512),
                            args: Vec::new(),
                            cwd: ".".to_owned(),
                            files: Vec::new(),
                            summary: None,
                        }),
                        preview_truncated: true,
                    }),
                },
            ),
        ),
        (
            "event: task_plan_updated (internal engine)",
            frame(
                62,
                Event::TaskPlanUpdated {
                    call_id: "call_04".to_owned(),
                    turn_id: turn_id(),
                },
            ),
        ),
        (
            "event: context_truncated (internal engine)",
            frame(
                63,
                Event::ContextTruncated {
                    original_tokens: 210_000,
                    fitted_tokens: 180_000,
                },
            ),
        ),
        (
            "event: compaction_started (internal engine)",
            frame(64, Event::CompactionStarted),
        ),
        (
            "event: compaction_finished (internal engine)",
            frame(65, Event::CompactionFinished { compacted: true }),
        ),
        (
            "event: session_tree",
            frame(
                66,
                Event::SessionTree {
                    children: vec![tidebreak_core::SessionTreeChild {
                        id: session_id(),
                        title: Some("Inspect the parser".to_owned()),
                        status: tidebreak_core::SessionTreeChildStatus::Running,
                        attention: false,
                        fenced: false,
                        workspace_id: None,
                        execution_location: Some(tidebreak_core::ExecutionLocation::Machine),
                    }],
                    wait: None,
                },
            ),
        ),
        (
            "event: background_activity (a message)",
            frame(
                67,
                Event::BackgroundActivity {
                    event: Box::new(Event::AssistantMessage {
                        text: "The background job finished.".to_owned(),
                        parent_call_id: None,
                    }),
                },
            ),
        ),
        (
            "event: background_activity (a tool call)",
            frame(
                68,
                Event::BackgroundActivity {
                    event: Box::new(Event::ToolStarted {
                        call_id: "toolu_own".to_owned(),
                        name: "Bash".to_owned(),
                        detail: ToolDetail::Command {
                            cmd: "cat job.log".to_owned(),
                            cwd: String::new(),
                        },
                        parent_call_id: None,
                    }),
                },
            ),
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
    let declared = declared_tags::<Event>("Event");
    let covered = fixture_tags("event_frame", |value| value.get("event")?.get("type"));
    let missing: Vec<_> = declared.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "event types without a code-frame fixture: {missing:?}"
    );

    let declared = declared_tags::<UpdateNotice>("UpdateNotice");
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
            "repo_trust" => round_trip::<CodeRepoTrustSnapshot>(entry),
            "workspace" => round_trip::<CodeWorkspaceSnapshot>(entry),
            "session" => round_trip::<SessionSnapshot>(entry),
            "turn" => round_trip::<TurnSnapshot>(entry),
            "queued_turn" => round_trip::<QueuedTurn>(entry),
            "queued_turns" => round_trip::<QueuedTurnsSnapshot>(entry),
            "harness_doctor" => round_trip::<HarnessDoctorReport>(entry),
            "workspace_files" => round_trip::<CodeWorkspaceFiles>(entry),
            "workspace_diff" => round_trip::<CodeWorkspaceDiff>(entry),
            "checkpoint_restore_preview" => round_trip::<CodeCheckpointRestorePreview>(entry),
            "checkpoint_restore" => round_trip::<CodeCheckpointRestoreResult>(entry),
            "approval" => round_trip::<ApprovalSnapshot>(entry),
            "commit" => round_trip::<CodeCommitSnapshot>(entry),
            "push" => round_trip::<CodePushSnapshot>(entry),
            "workspace_pr" => round_trip::<CodeWorkspacePrSnapshot>(entry),
            "action" => round_trip::<CodeActionSnapshot>(entry),
            "session_digest" => round_trip::<SessionDigest>(entry),
            "update_notice" => round_trip::<UpdateNotice>(entry),
            "event_frame" => round_trip::<SequencedEventFrame>(entry),
            other => panic!("fixture {} has no decoder for kind {other}", entry.name),
        }
    }
}

/// A key a newer server adds is ignored at the snapshot, the notice, and the
/// frame, and the value reads exactly as it would without it. The round trip
/// above is what still fails on a key a type does not declare.
#[test]
fn code_values_ignore_unknown_keys() {
    fn ignores<T: serde::de::DeserializeOwned + serde::Serialize>(
        entry: &Fixture,
        value: serde_json::Value,
    ) {
        let decoded: T = serde_json::from_value(value).unwrap_or_else(|error| {
            panic!(
                "fixture {} should read past an unknown key: {error}",
                entry.name
            )
        });
        assert_eq!(
            serde_json::to_value(&decoded).expect("a decoded value serializes"),
            entry.value,
            "fixture {} changed when an unknown key was added",
            entry.name
        );
    }
    for entry in code_frame_fixtures() {
        let mut value = entry.value.clone();
        value
            .as_object_mut()
            .expect("every fixture is an object")
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        if entry.kind == "event_frame" {
            // Inside the event union too, which the server also reads back
            // from its own journal.
            if let Some(event) = value["event"].as_object_mut() {
                event.insert("extra".to_owned(), serde_json::Value::Bool(true));
            }
        }
        match entry.kind {
            "repo" => ignores::<CodeRepoSnapshot>(&entry, value),
            "repo_trust" => ignores::<CodeRepoTrustSnapshot>(&entry, value),
            "workspace" => ignores::<CodeWorkspaceSnapshot>(&entry, value),
            "session" => ignores::<SessionSnapshot>(&entry, value),
            "turn" => ignores::<TurnSnapshot>(&entry, value),
            "queued_turn" => ignores::<QueuedTurn>(&entry, value),
            "queued_turns" => ignores::<QueuedTurnsSnapshot>(&entry, value),
            "harness_doctor" => ignores::<HarnessDoctorReport>(&entry, value),
            "workspace_files" => ignores::<CodeWorkspaceFiles>(&entry, value),
            "workspace_diff" => ignores::<CodeWorkspaceDiff>(&entry, value),
            "checkpoint_restore_preview" => ignores::<CodeCheckpointRestorePreview>(&entry, value),
            "checkpoint_restore" => ignores::<CodeCheckpointRestoreResult>(&entry, value),
            "approval" => ignores::<ApprovalSnapshot>(&entry, value),
            "commit" => ignores::<CodeCommitSnapshot>(&entry, value),
            "push" => ignores::<CodePushSnapshot>(&entry, value),
            "workspace_pr" => ignores::<CodeWorkspacePrSnapshot>(&entry, value),
            "action" => ignores::<CodeActionSnapshot>(&entry, value),
            "session_digest" => ignores::<SessionDigest>(&entry, value),
            "update_notice" => ignores::<UpdateNotice>(&entry, value),
            "event_frame" => ignores::<SequencedEventFrame>(&entry, value),
            other => panic!("fixture {} has no decoder for kind {other}", entry.name),
        }
    }
}

/// A notice tag this build does not know fails the notice rather than
/// folding to something a client would misread.
#[test]
fn unknown_update_notices_fail() {
    let unknown = r#"{"type":"some_future_notice","extra":true}"#;
    assert!(serde_json::from_str::<UpdateNotice>(unknown).is_err());
    let unknown_event = r#"{"seq":9,"event":{"type":"some_future_event"}}"#;
    assert!(serde_json::from_str::<SequencedEventFrame>(unknown_event).is_err());
}
