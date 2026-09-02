//! `tidebreak code …` — the headless client of the code-mode surface.
//!
//! Every verb here is a thin wrapper over a `/code/*` route the server already
//! serves. The CLI embeds a server by default and attaches with `--server` /
//! `--attach` the same way `-p` and the setup family do. `--json` (or
//! `--output-format json`) writes one object, or NDJSON for the two streaming
//! commands (`run`, `watch`); human output otherwise.

use std::future::Future as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use futures::StreamExt as _;
use tidebreak_core::{
    AgentError, ApprovalDecisionKind, Attention, AttentionState, CapLevel, CodeApprovalId,
    CodeApprovalKind, CodeEvent, CodeSessionId, CodeSessionLifecycle, CodeTurnId, HarnessCaps,
    HarnessKind, PermissionMode, RepoId, Result, WorkspaceId,
};
use tokio_tungstenite::tungstenite::Message;

use crate::api::client::Client;
use crate::api::code::{
    decode_event_frame, decode_update_notice, is_turn_terminal, supported_caps_summary,
    turn_exit_code, CodeApprovalSnapshot, CodeSessionDigest, CodeSessionSnapshot, CodeTurnSnapshot,
    CodeUpdateNotice, CodeWorkspaceSnapshot, HarnessAuthMode, SubmitTurnResponse,
};
use crate::connect::Server;
use crate::print::OutputFormat;

/// Exit status when `--on-approval fail` sees a parked approval.
const EXIT_APPROVAL_PARKED: i32 = 3;
/// Timed out waiting for a turn or a watch snapshot. Same number GNU
/// `timeout(1)` uses, so a driver can treat both the same way.
const EXIT_TIMEOUT: i32 = 124;
/// SIGINT, following the shell's 128+signal convention and `-p`.
const EXIT_INTERRUPTED: i32 = 130;

/// Short usage for the `code` family. Parse errors print this instead of the
/// whole CLI surface — a driving agent should not have to scrape 80 lines to
/// see that `--session` was missing.
pub const USAGE: &str = "\
usage: tidebreak code doctor [--refresh]
       tidebreak code repo add <path> [--name <name>] [--base-ref <ref>] [--branch-prefix <p>]
       tidebreak code repo list
       tidebreak code repo rm <id>
       tidebreak code ws new --repo <id|path> [--title <title>] [--base-ref <ref>]
       tidebreak code ws list [--repo <id|path>]
       tidebreak code ws show <id>
       tidebreak code ws archive <id> [--force]
       tidebreak code session start --ws <id> --harness <kind> [--mode plan|ask|auto|allow]
       tidebreak code session show <id>
       tidebreak code session mode <id> plan|ask|auto|allow
       tidebreak code session reap <id>
       tidebreak code run (--session <id> | --ws <id>) [<message>]
                  [--on-approval wait|fail] [--timeout <secs>]
       tidebreak code approvals [--session <id>]
       tidebreak code approve <approval-id>
       tidebreak code deny <approval-id> [-m <feedback>]
       tidebreak code interrupt --session <id>
       tidebreak code turns --session <id>
       tidebreak code diff --ws <id> [--turn N] [--file PATH]
       tidebreak code files --ws <id> [--turn N]
       tidebreak code git commit --ws <id> [-m MSG]
       tidebreak code git push --ws <id>
       tidebreak code git pr --ws <id> [--title <title>] [--body <body>]
       tidebreak code git status --ws <id>
       tidebreak code action <name> --ws <id>
       tidebreak code watch [--once] [--timeout <secs>]

Every verb takes --json (or --output-format json). run and watch stream NDJSON
under --json. --timeout is seconds. watch --once prints the connect snapshot
and exits. session start without --mode uses ask when the doctor says
structured approvals are supported, otherwise plan.";

const RECONNECT_ATTEMPTS: usize = 3;
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// What to do when `code run` sees an `approval_requested` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnApproval {
    /// Keep streaming while the turn is parked (the default).
    Wait,
    /// Exit nonzero as soon as an approval is requested.
    Fail,
}

/// How `--turn` named a turn: the workspace-visible ordinal, or an exact id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRef {
    Ordinal(i64),
    Id(CodeTurnId),
}

/// One parsed `tidebreak code` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Doctor {
        refresh: bool,
        format: OutputFormat,
    },
    RepoAdd {
        path: PathBuf,
        name: Option<String>,
        base_ref: Option<String>,
        branch_prefix: Option<String>,
        format: OutputFormat,
    },
    RepoList {
        format: OutputFormat,
    },
    RepoRm {
        id: RepoId,
        format: OutputFormat,
    },
    WsNew {
        repo: String,
        title: Option<String>,
        base_ref: Option<String>,
        format: OutputFormat,
    },
    WsList {
        repo: Option<String>,
        format: OutputFormat,
    },
    WsShow {
        id: WorkspaceId,
        format: OutputFormat,
    },
    WsArchive {
        id: WorkspaceId,
        force: bool,
        format: OutputFormat,
    },
    SessionStart {
        workspace: WorkspaceId,
        harness: HarnessKind,
        /// `None` means the doctor-driven default: ask when this engine's
        /// structured approvals are Supported, otherwise plan. An explicit
        /// `--mode` is passed through verbatim.
        mode: Option<PermissionMode>,
        format: OutputFormat,
    },
    SessionShow {
        id: CodeSessionId,
        format: OutputFormat,
    },
    SessionMode {
        id: CodeSessionId,
        mode: PermissionMode,
        format: OutputFormat,
    },
    SessionReap {
        id: CodeSessionId,
        format: OutputFormat,
    },
    Run {
        session: Option<CodeSessionId>,
        workspace: Option<WorkspaceId>,
        message: String,
        on_approval: OnApproval,
        timeout: Option<u64>,
        format: OutputFormat,
    },
    Approvals {
        session: Option<CodeSessionId>,
        format: OutputFormat,
    },
    Approve {
        id: CodeApprovalId,
        format: OutputFormat,
    },
    Deny {
        id: CodeApprovalId,
        feedback: Option<String>,
        format: OutputFormat,
    },
    Interrupt {
        session: CodeSessionId,
        format: OutputFormat,
    },
    Turns {
        session: CodeSessionId,
        format: OutputFormat,
    },
    Diff {
        workspace: WorkspaceId,
        turn: Option<TurnRef>,
        file: Option<String>,
        format: OutputFormat,
    },
    Files {
        workspace: WorkspaceId,
        turn: Option<TurnRef>,
        format: OutputFormat,
    },
    GitCommit {
        workspace: WorkspaceId,
        message: Option<String>,
        format: OutputFormat,
    },
    GitPush {
        workspace: WorkspaceId,
        format: OutputFormat,
    },
    GitPr {
        workspace: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
        format: OutputFormat,
    },
    GitStatus {
        workspace: WorkspaceId,
        format: OutputFormat,
    },
    Action {
        name: String,
        workspace: WorkspaceId,
        format: OutputFormat,
    },
    Watch {
        once: bool,
        timeout: Option<u64>,
        format: OutputFormat,
    },
}

/// Hand-rolled argument parsing, matching the rest of the CLI.
pub fn parse(args: impl IntoIterator<Item = String>) -> std::result::Result<Command, String> {
    let mut cursor = Cursor::new(args.into_iter().collect());
    let verb = match cursor.next() {
        Some(verb) if !verb.starts_with("--") => verb,
        _ => return Err("code requires a subcommand".to_owned()),
    };
    match verb.as_str() {
        "doctor" => parse_doctor(&mut cursor),
        "repo" => parse_repo(&mut cursor),
        "ws" => parse_ws(&mut cursor),
        "session" => parse_session(&mut cursor),
        "run" => parse_run(&mut cursor),
        "approvals" => parse_approvals(&mut cursor),
        "approve" => parse_approve(&mut cursor),
        "deny" => parse_deny(&mut cursor),
        "interrupt" => parse_interrupt(&mut cursor),
        "turns" => parse_turns(&mut cursor),
        "diff" => parse_diff(&mut cursor),
        "files" => parse_files(&mut cursor),
        "git" => parse_git(&mut cursor),
        "action" => parse_action(&mut cursor),
        "watch" => parse_watch(&mut cursor),
        other => Err(format!("unknown code subcommand {other:?}")),
    }
}

/// Run one code-mode command against the profile's server.
pub async fn run(command: Command, server: Server) -> Result<i32> {
    let session = crate::connect::Session::open(&server).await?;
    execute(session.client(), command).await
}

async fn execute(client: &Client, command: Command) -> Result<i32> {
    match command {
        Command::Doctor { refresh, format } => {
            let report = if refresh {
                client.refresh_harnesses().await?
            } else {
                client.list_harnesses().await?
            };
            if format == OutputFormat::Json {
                emit(&serde_json::to_value(&report).unwrap_or_default())?;
                return Ok(0);
            }
            if report.harnesses.is_empty() {
                eprintln!("tidebreak: no coding harnesses are registered on this server");
                return Ok(0);
            }
            println!(
                "{:<14} {:<6} {:<28} {:<12} {:<12} {:<5} CAPS",
                "KIND", "FOUND", "PATH", "VERSION", "TIER", "AUTH"
            );
            for entry in &report.harnesses {
                let found = if entry.found { "yes" } else { "no" };
                let path = entry.path.as_deref().unwrap_or("-");
                let version = entry.version.as_deref().unwrap_or("-");
                let auth = match entry.auth_mode {
                    // The vendor login is one mode of three. A machine whose
                    // engines run on gateway credentials has no login to
                    // report, and printing "no" there reads as broken.
                    HarnessAuthMode::GatewayManaged => "gw",
                    HarnessAuthMode::GatewayRelay => "relay",
                    HarnessAuthMode::HostedUnavailable => "n/a",
                    HarnessAuthMode::LocalSignIn => match entry.authenticated {
                        Some(true) => "yes",
                        Some(false) => "no",
                        None => "-",
                    },
                };
                let tier = format!("{:?}", entry.tier).to_ascii_lowercase();
                println!(
                    "{:<14} {:<6} {:<28} {:<12} {:<12} {:<5} {}",
                    entry.kind.as_str(),
                    found,
                    clip(path, 28),
                    clip(version, 12),
                    tier,
                    auth,
                    supported_caps_summary(&entry.caps)
                );
                if !entry.remediation.is_empty() {
                    println!("  remediation: {}", entry.remediation);
                }
            }
            Ok(0)
        }
        Command::RepoAdd {
            path,
            name,
            base_ref,
            branch_prefix,
            format,
        } => {
            let repo = client
                .create_repo(
                    &path.to_string_lossy(),
                    name.as_deref(),
                    base_ref.as_deref(),
                    branch_prefix.as_deref(),
                )
                .await?;
            if format == OutputFormat::Json {
                return emit_ok(&repo);
            }
            println!(
                "tidebreak: registered {}  {}  {}",
                repo.id, repo.display_name, repo.root_path
            );
            Ok(0)
        }
        Command::RepoList { format } => {
            let repos = client.list_repos().await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "repos": repos }));
            }
            if repos.is_empty() {
                eprintln!("tidebreak: no repositories registered");
            }
            for repo in repos {
                println!(
                    "{}\t{}\t{}\t{}",
                    repo.id, repo.display_name, repo.root_path, repo.default_base_ref
                );
            }
            Ok(0)
        }
        Command::RepoRm { id, format } => {
            client.delete_repo(id).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "id": id, "removed": true }));
            }
            println!("tidebreak: removed repository {id}");
            Ok(0)
        }
        Command::WsNew {
            repo,
            title,
            base_ref,
            format,
        } => {
            let repo_id = resolve_repo(client, &repo).await?;
            let workspace = client
                .create_workspace(repo_id, title.as_deref(), base_ref.as_deref())
                .await?;
            if format == OutputFormat::Json {
                return emit_ok(&workspace);
            }
            println!(
                "tidebreak: workspace {}  {}  {}",
                workspace.id, workspace.branch_name, workspace.worktree_path
            );
            Ok(0)
        }
        Command::WsList { repo, format } => {
            let repo_id = match repo {
                Some(repo) => Some(resolve_repo(client, &repo).await?),
                None => None,
            };
            let workspaces = client.list_workspaces(repo_id).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "workspaces": workspaces }));
            }
            if workspaces.is_empty() {
                eprintln!("tidebreak: no workspaces");
            }
            for workspace in workspaces {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    workspace.id,
                    workspace.status.as_str(),
                    workspace.branch_name,
                    workspace.title,
                    workspace.worktree_path
                );
            }
            Ok(0)
        }
        Command::WsShow { id, format } => {
            let workspace = client.get_workspace(id).await?;
            let sessions = client.list_workspace_sessions(id).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "workspace": workspace,
                    "sessions": sessions,
                }));
            }
            print_workspace(&workspace);
            if sessions.is_empty() {
                println!("sessions             none");
            } else {
                println!("sessions");
                for session in sessions {
                    println!(
                        "  {}\t{}\t{}\t{}",
                        session.id,
                        session.harness_kind.as_str(),
                        session.lifecycle.as_str(),
                        attention_label(&session.attention)
                    );
                }
            }
            Ok(0)
        }
        Command::WsArchive { id, force, format } => {
            let workspace = client.archive_workspace(id, force).await?;
            if format == OutputFormat::Json {
                return emit_ok(&workspace);
            }
            println!("tidebreak: archived workspace {id}");
            Ok(0)
        }
        Command::SessionStart {
            workspace,
            harness,
            mode,
            format,
        } => {
            let (mode, fallback_note) = resolve_start_mode(client, harness, mode).await?;
            let session = client.create_session(workspace, harness, mode).await?;
            if format == OutputFormat::Json {
                return emit_ok(&session);
            }
            if let Some(note) = fallback_note {
                println!("tidebreak: {note}");
            }
            println!(
                "tidebreak: session {}  {}  {}",
                session.id,
                session.harness_kind.as_str(),
                session.permission_mode.as_str()
            );
            Ok(0)
        }
        Command::SessionShow { id, format } => {
            let (session, turns) = load_session(client, id).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({
                    "session": session,
                    "turns": turns,
                }));
            }
            print_session(&session);
            if turns.is_empty() {
                println!("turns                none");
            } else {
                println!("turns");
                for turn in turns {
                    print_turn_line(&turn);
                }
            }
            Ok(0)
        }
        Command::SessionMode { id, mode, format } => {
            let session = client.set_session_permission_mode(id, mode).await?;
            if format == OutputFormat::Json {
                return emit_ok(&session);
            }
            println!(
                "tidebreak: session {} is now in {} mode",
                session.id, session.permission_mode
            );
            Ok(0)
        }
        Command::SessionReap { id, format } => {
            let session = client.reap_session(id).await?;
            if format == OutputFormat::Json {
                return emit_ok(&session);
            }
            println!(
                "tidebreak: reaped session {}  {}",
                session.id,
                session.lifecycle.as_str()
            );
            Ok(0)
        }
        Command::Run {
            session,
            workspace,
            message,
            on_approval,
            timeout,
            format,
        } => {
            let session = resolve_run_session(client, session, workspace).await?;
            eprintln!("tidebreak: session {session}");
            run_turn(
                client,
                session,
                message.trim(),
                on_approval,
                timeout,
                format,
            )
            .await
        }
        Command::Approvals { session, format } => {
            let approvals = client.list_approvals(session, true).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "approvals": approvals }));
            }
            if approvals.is_empty() {
                eprintln!("tidebreak: no pending approvals");
            }
            for approval in approvals {
                print_approval(&approval);
            }
            Ok(0)
        }
        Command::Approve { id, format } => {
            let approval = client.decide_code_approval(id, true, None).await?;
            if format == OutputFormat::Json {
                return emit_ok(&approval);
            }
            println!("tidebreak: approved {id}");
            Ok(0)
        }
        Command::Deny {
            id,
            feedback,
            format,
        } => {
            let approval = client
                .decide_code_approval(id, false, feedback.as_deref())
                .await?;
            if format == OutputFormat::Json {
                return emit_ok(&approval);
            }
            println!("tidebreak: denied {id}");
            Ok(0)
        }
        Command::Interrupt { session, format } => {
            client.interrupt_session(session).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "session": session, "interrupted": true }));
            }
            println!("tidebreak: interrupted session {session}");
            Ok(0)
        }
        Command::Turns { session, format } => {
            let turns = client.list_session_turns(session).await?;
            if format == OutputFormat::Json {
                return emit(&serde_json::json!({ "turns": turns }));
            }
            if turns.is_empty() {
                eprintln!("tidebreak: no turns");
            }
            for turn in turns {
                print_turn_line(&turn);
            }
            Ok(0)
        }
        Command::Diff {
            workspace,
            turn,
            file,
            format,
        } => {
            let turn_id = resolve_turn(client, workspace, turn).await?;
            let diff = client
                .workspace_diff(workspace, turn_id, file.as_deref())
                .await?;
            if format == OutputFormat::Json {
                return emit_ok(&diff);
            }
            if !diff.diff.is_empty() {
                print!("{}", diff.diff);
                if !diff.diff.ends_with('\n') {
                    println!();
                }
            }
            if diff.truncated {
                eprintln!(
                    "tidebreak: diff truncated  {} files +{} -{}",
                    diff.stat.files, diff.stat.insertions, diff.stat.deletions
                );
            }
            Ok(0)
        }
        Command::Files {
            workspace,
            turn,
            format,
        } => {
            let turn_id = resolve_turn(client, workspace, turn).await?;
            let files = client.workspace_files(workspace, turn_id).await?;
            if format == OutputFormat::Json {
                return emit_ok(&files);
            }
            for file in &files.files {
                println!(
                    "{}\t{}\t+{}\t-{}",
                    file.kind.as_str_display(),
                    file.path,
                    file.insertions,
                    file.deletions
                );
            }
            if files.truncated {
                eprintln!(
                    "tidebreak: file list truncated  {} files +{} -{}",
                    files.stat.files, files.stat.insertions, files.stat.deletions
                );
            }
            Ok(0)
        }
        Command::GitCommit {
            workspace,
            message,
            format,
        } => {
            let commit = client.git_commit(workspace, message.as_deref()).await?;
            if format == OutputFormat::Json {
                return emit_ok(&commit);
            }
            println!(
                "tidebreak: committed {}  {}  +{} -{}",
                commit.sha, commit.message, commit.stat.insertions, commit.stat.deletions
            );
            Ok(0)
        }
        Command::GitPush { workspace, format } => {
            let push = client.git_push(workspace).await?;
            if format == OutputFormat::Json {
                return emit_ok(&push);
            }
            println!("tidebreak: pushed {} to {}", push.branch, push.remote);
            Ok(0)
        }
        Command::GitPr {
            workspace,
            title,
            body,
            format,
        } => {
            let pr = client
                .git_pr(workspace, title.as_deref(), body.as_deref())
                .await?;
            if format == OutputFormat::Json {
                return emit_ok(&pr);
            }
            print_pr(&pr);
            Ok(0)
        }
        Command::GitStatus { workspace, format } => {
            let status = client.git_status(workspace).await?;
            if format == OutputFormat::Json {
                return emit_ok(&status);
            }
            print_pr(&status);
            Ok(0)
        }
        Command::Action {
            name,
            workspace,
            format,
        } => {
            let action = client.run_action(workspace, &name).await?;
            if format == OutputFormat::Json {
                return emit_ok(&action);
            }
            let result = if action.success { "ok" } else { "failed" };
            println!("tidebreak: action {name} {result}");
            if !action.stdout.is_empty() {
                print!("{}", action.stdout);
                if !action.stdout.ends_with('\n') {
                    println!();
                }
            }
            if !action.stderr.is_empty() {
                eprint!("{}", action.stderr);
                if !action.stderr.ends_with('\n') {
                    eprintln!();
                }
            }
            if action.success {
                Ok(0)
            } else {
                Ok(1)
            }
        }
        Command::Watch {
            once,
            timeout,
            format,
        } => watch(client, once, timeout, format).await,
    }
}

/// There is no `GET /code/sessions/{id}`. Walk workspaces to recover the row,
/// then load its turns. A missing session 404s the same way a dedicated
/// route would.
async fn load_session(
    client: &Client,
    id: CodeSessionId,
) -> Result<(CodeSessionSnapshot, Vec<CodeTurnSnapshot>)> {
    let session = find_session(client, id).await?;
    let turns = client.list_session_turns(id).await?;
    Ok((session, turns))
}

async fn find_session(client: &Client, id: CodeSessionId) -> Result<CodeSessionSnapshot> {
    let workspaces = client.list_workspaces(None).await?;
    for workspace in workspaces {
        let sessions = client.list_workspace_sessions(workspace.id).await?;
        if let Some(session) = sessions.into_iter().find(|session| session.id == id) {
            return Ok(session);
        }
    }
    Err(AgentError::msg(format!(
        "not_found: session {id} not found"
    )))
}

async fn resolve_repo(client: &Client, repo: &str) -> Result<RepoId> {
    if let Ok(id) = RepoId::from_str(repo) {
        return Ok(id);
    }
    let wanted = canonicalize_path(repo);
    let repos = client.list_repos().await?;
    let mut matches: Vec<_> = repos
        .into_iter()
        .filter(|candidate| {
            paths_equal(&candidate.root_path, repo) || paths_equal(&candidate.root_path, &wanted)
        })
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0).id),
        0 => Err(AgentError::msg(format!(
            "no registered repository matches {repo:?}"
        ))),
        _ => Err(AgentError::msg(format!(
            "more than one registered repository matches {repo:?}"
        ))),
    }
}

fn paths_equal(left: &str, right: &str) -> bool {
    left == right
}

fn canonicalize_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

async fn resolve_run_session(
    client: &Client,
    session: Option<CodeSessionId>,
    workspace: Option<WorkspaceId>,
) -> Result<CodeSessionId> {
    if let Some(session) = session {
        return Ok(session);
    }
    let workspace = workspace
        .ok_or_else(|| AgentError::msg("code run requires --session <id> or --ws <id>"))?;
    let sessions = client.list_workspace_sessions(workspace).await?;
    pick_active_session(&sessions)
        .ok_or_else(|| AgentError::msg(format!("workspace {workspace} has no active session")))
}

/// Desktop create default: the most autonomous posture the engine honors,
/// walking Allow -> Auto -> Ask -> Plan (decision 0039, amended 2026-08-18).
/// `resolve_start_mode` states whichever posture that is before the turn runs.
fn default_create_permission_mode(caps: Option<&HarnessCaps>) -> PermissionMode {
    match caps {
        Some(caps) if caps.allow_mode == CapLevel::Supported => PermissionMode::Allow,
        Some(caps) if caps.auto_mode == CapLevel::Supported => PermissionMode::Auto,
        Some(caps) if caps.structured_approvals == CapLevel::Supported => PermissionMode::Ask,
        _ => PermissionMode::Plan,
    }
}

async fn resolve_start_mode(
    client: &Client,
    harness: HarnessKind,
    explicit: Option<PermissionMode>,
) -> Result<(PermissionMode, Option<&'static str>)> {
    if let Some(mode) = explicit {
        return Ok((mode, None));
    }
    let report = client.list_harnesses().await?;
    let caps = report
        .harnesses
        .iter()
        .find(|entry| entry.kind == harness)
        .map(|entry| &entry.caps);
    let mode = default_create_permission_mode(caps);
    let note = match mode {
        PermissionMode::Plan => {
            Some("starting in plan mode — approvals unavailable on this engine")
        }
        PermissionMode::Auto => Some(
            "starting in auto mode — this engine has no approval channel; every action proceeds without asking",
        ),
        PermissionMode::Allow => Some(
            "starting in allow mode — this engine's permission system is off; every action runs without asking",
        ),
        PermissionMode::Ask => None,
    };
    Ok((mode, note))
}

fn pick_active_session(sessions: &[CodeSessionSnapshot]) -> Option<CodeSessionId> {
    let usable = |session: &&CodeSessionSnapshot| {
        session.kind == tidebreak_core::CodeSessionKind::Interactive
            && !matches!(
                session.lifecycle,
                CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
            )
    };
    sessions
        .iter()
        .filter(usable)
        .find(|session| session.lifecycle == CodeSessionLifecycle::Running)
        .or_else(|| {
            sessions
                .iter()
                .filter(usable)
                .max_by_key(|session| session.created_at)
        })
        .map(|session| session.id)
}

async fn resolve_turn(
    client: &Client,
    workspace: WorkspaceId,
    turn: Option<TurnRef>,
) -> Result<Option<CodeTurnId>> {
    let Some(turn) = turn else {
        return Ok(None);
    };
    match turn {
        TurnRef::Id(id) => Ok(Some(id)),
        TurnRef::Ordinal(ordinal) => {
            let sessions = client.list_workspace_sessions(workspace).await?;
            let session = pick_active_session(&sessions)
                .or_else(|| sessions.first().map(|session| session.id))
                .ok_or_else(|| AgentError::msg(format!("workspace {workspace} has no sessions")))?;
            let turns = client.list_session_turns(session).await?;
            turns
                .into_iter()
                .find(|turn| turn.ordinal == ordinal)
                .map(|turn| Some(turn.id))
                .ok_or_else(|| AgentError::msg(format!("no turn {ordinal} on session {session}")))
        }
    }
}

/// Which frames `code run` owns. Extracted so the two ownership bugs have
/// fixtures that do not need a live server.
#[derive(Debug)]
struct TurnGate {
    expected: Option<CodeTurnId>,
    ours: bool,
    /// True after we have seen `TurnStarted` for `expected`. Replayed
    /// history of earlier turns is ignored until then; live frames of an
    /// attach-owned turn are accepted without it.
    seen_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameAction {
    Skip,
    Render,
    Terminal(i32),
}

impl TurnGate {
    fn submit() -> Self {
        Self {
            expected: None,
            ours: false,
            seen_start: false,
        }
    }

    /// Attach to a turn that is already running. Own it immediately so live
    /// mid-turn frames are accepted; `TurnStarted` for that id already
    /// happened and only arrives `replayed: true`.
    fn attach(turn: CodeTurnId) -> Self {
        Self {
            expected: Some(turn),
            ours: true,
            seen_start: false,
        }
    }

    fn ours(&self) -> bool {
        self.ours
    }

    /// The turn this submit owns, once bound.
    fn bound_turn(&self) -> Option<CodeTurnId> {
        self.ours.then_some(self.expected).flatten()
    }

    fn on_ran(&mut self, id: CodeTurnId) {
        if self.ours && self.expected == Some(id) {
            return;
        }
        if self.expected.is_some_and(|bound| bound != id) {
            self.ours = false;
            self.seen_start = false;
        }
        self.expected = Some(id);
    }

    /// After Queued, only a later *live* `TurnStarted` binds ownership.
    fn on_queued(&mut self) {
        self.expected = None;
        self.ours = false;
        self.seen_start = false;
    }

    fn will_claim(&self, turn_id: CodeTurnId, replayed: bool) -> bool {
        match self.expected {
            Some(id) if id == turn_id => !self.seen_start,
            None => !replayed,
            Some(_) => false,
        }
    }

    fn on_frame(&mut self, replayed: bool, event: &CodeEvent) -> FrameAction {
        if let CodeEvent::TurnStarted { turn_id } = event {
            let matches_bound = self.expected == Some(*turn_id);
            let live_unbound = self.expected.is_none() && !replayed;
            if matches_bound || live_unbound {
                self.expected = Some(*turn_id);
                self.ours = true;
                self.seen_start = true;
            }
        }

        // Never accept a terminal while this submit has not bound a turn —
        // a live `turn_completed` here belongs to the already-running
        // previous turn, not to a Queued follow-up.
        if self.expected.is_none() || !self.ours {
            return FrameAction::Skip;
        }
        // Replayed history before our `TurnStarted` is some other turn.
        // Live frames of an attach-owned turn are ours even without it.
        if replayed && !self.seen_start {
            return FrameAction::Skip;
        }
        if is_turn_terminal(event) {
            return FrameAction::Terminal(turn_exit_code(event).unwrap_or(1));
        }
        FrameAction::Render
    }
}

/// How long to wait past a completed turn for its checkpoint.
///
/// The server records the checkpoint after publishing the turn's terminal
/// event, so it lands tens of milliseconds later. This is a bound on that
/// gap, not a poll interval — the wait ends the moment the frame arrives.
const CHECKPOINT_TAIL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Read the trailing `checkpoint_recorded` for a turn that just completed.
///
/// Returns the updated `dangling` state. A turn whose checkpoint failed
/// journals a `harness_notice` instead, so that is accepted as an ending
/// too; anything else is passed through untouched. Times out quietly — a
/// missing checkpoint must never fail a turn that already succeeded.
async fn drain_checkpoint(
    client: &Client,
    session: CodeSessionId,
    stream: &mut CodeStream,
    turn_id: CodeTurnId,
    format: OutputFormat,
    mut dangling: bool,
    streamed_text: &mut String,
) -> bool {
    let deadline = tokio::time::Instant::now() + CHECKPOINT_TAIL_WAIT;
    loop {
        let frame = tokio::select! {
            frame = stream.next_session(client, session) => frame,
            () = tokio::time::sleep_until(deadline) => return dangling,
        };
        let Ok(Some((raw, decoded))) = frame else {
            return dangling;
        };
        let done = match &decoded.event {
            CodeEvent::CheckpointRecorded { turn_id: id, .. } => *id == turn_id,
            CodeEvent::HarnessNotice { .. } => true,
            _ => continue,
        };
        if format == OutputFormat::Json {
            emit_line(&raw);
        } else {
            dangling = render_event(
                &decoded.event,
                decoded.replacement == Some(true),
                dangling,
                streamed_text,
            );
        }
        if done {
            return dangling;
        }
    }
}

async fn run_turn(
    client: &Client,
    session: CodeSessionId,
    message: &str,
    on_approval: OnApproval,
    timeout: Option<u64>,
    format: OutputFormat,
) -> Result<i32> {
    // Subscribe first. `POST /turns` waits for the worker to finish the
    // whole turn (or to queue), so a CLI that only reads after the POST
    // returns never sees a live approval and cannot honor `--timeout`.
    let mut stream = CodeStream::open_session(client, session).await?;
    let attach_only = message.is_empty();
    let attach_turn = if attach_only {
        let running = client
            .list_session_turns(session)
            .await?
            .into_iter()
            .rev()
            .find(|turn| turn.status == tidebreak_core::CodeTurnStatus::Running);
        match running {
            Some(turn) => {
                eprintln!("tidebreak: attaching to turn {}", turn.id);
                Some(turn.id)
            }
            None => {
                eprintln!("tidebreak: session {session} has no running turn");
                return Ok(0);
            }
        }
    } else {
        None
    };
    let mut submit = if attach_only {
        None
    } else {
        Some(std::pin::pin!(client.submit_turn(session, message)))
    };
    let mut submit_done = attach_only;
    let mut gate = match attach_turn {
        Some(turn) => TurnGate::attach(turn),
        None => TurnGate::submit(),
    };
    let mut interrupt = Interrupt::watch().await;
    let deadline =
        timeout.map(|secs| tokio::time::Instant::now() + std::time::Duration::from_secs(secs));
    let mut dangling = false;
    let mut streamed_text = String::new();

    let outcome = loop {
        let frame = tokio::select! {
            frame = stream.next_session(client, session) => frame?,
            submitted = async {
                match submit.as_mut() {
                    Some(fut) if !submit_done => fut.await,
                    _ => std::future::pending().await,
                }
            } => {
                submit_done = true;
                match submitted? {
                    SubmitTurnResponse::Ran(turn) => {
                        eprintln!("tidebreak: turn {}", turn.id);
                        gate.on_ran(turn.id);
                    }
                    SubmitTurnResponse::Queued(_) => {
                        eprintln!(
                            "tidebreak: turn queued; waiting for the running turn to finish"
                        );
                        gate.on_queued();
                    }
                }
                continue;
            }
            () = interrupt.fired() => {
                let _ = client.interrupt_session(session).await;
                break Ok(EXIT_INTERRUPTED);
            }
            () = sleep_until(deadline) => {
                let _ = client.interrupt_session(session).await;
                eprintln!(
                    "tidebreak: timed out after {}s waiting for the turn to finish",
                    timeout.unwrap_or(0)
                );
                break Ok(EXIT_TIMEOUT);
            }
        };
        let Some((raw, decoded)) = frame else {
            continue;
        };
        if let CodeEvent::TurnStarted { turn_id } = &decoded.event {
            if gate.will_claim(*turn_id, decoded.replayed == Some(true)) && !gate.ours() {
                eprintln!("tidebreak: turn {turn_id}");
            }
        }
        match gate.on_frame(decoded.replayed == Some(true), &decoded.event) {
            FrameAction::Skip => continue,
            FrameAction::Render => {}
            FrameAction::Terminal(code) => {
                if format == OutputFormat::Json {
                    emit_line(&raw);
                } else {
                    dangling = render_event(
                        &decoded.event,
                        decoded.replacement == Some(true),
                        dangling,
                        &mut streamed_text,
                    );
                }
                // The checkpoint is journaled just after the turn's terminal
                // event, so breaking here dropped it every time: the turn's
                // diffstat never reached a caller reading this stream, and a
                // script that acted on our exit raced a checkpoint that was
                // not durable yet.
                if let CodeEvent::TurnCompleted { .. } = &decoded.event {
                    if let Some(turn_id) = gate.bound_turn() {
                        dangling = drain_checkpoint(
                            client,
                            session,
                            &mut stream,
                            turn_id,
                            format,
                            dangling,
                            &mut streamed_text,
                        )
                        .await;
                    }
                }
                break Ok(code);
            }
        }
        if format == OutputFormat::Json {
            emit_line(&raw);
        } else {
            dangling = render_event(
                &decoded.event,
                decoded.replacement == Some(true),
                dangling,
                &mut streamed_text,
            );
        }
        if let CodeEvent::ApprovalRequested { approval_id, .. } = &decoded.event {
            print_approval_prompt(*approval_id);
            if on_approval == OnApproval::Fail {
                break Ok(EXIT_APPROVAL_PARKED);
            }
        }
    };
    if dangling {
        println!();
    }
    outcome
}

async fn watch(
    client: &Client,
    once: bool,
    timeout: Option<u64>,
    format: OutputFormat,
) -> Result<i32> {
    let mut stream = CodeStream::open_updates(client).await?;
    let mut interrupt = Interrupt::watch().await;
    let deadline =
        timeout.map(|secs| tokio::time::Instant::now() + std::time::Duration::from_secs(secs));
    loop {
        let frame = tokio::select! {
            frame = stream.next_updates(client) => frame?,
            () = interrupt.fired() => return Ok(EXIT_INTERRUPTED),
            () = sleep_until(deadline) => {
                eprintln!(
                    "tidebreak: timed out after {}s waiting for an update",
                    timeout.unwrap_or(0)
                );
                return Ok(EXIT_TIMEOUT);
            }
        };
        let Some((raw, notice)) = frame else {
            continue;
        };
        if format == OutputFormat::Json {
            emit_line(&raw);
        } else {
            render_update(&notice);
        }
        if once {
            return Ok(0);
        }
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn render_event(
    event: &CodeEvent,
    replacement: bool,
    dangling: bool,
    streamed_text: &mut String,
) -> bool {
    match event {
        CodeEvent::AssistantDelta { text } => {
            let text = if replacement {
                reconcile_assistant_text(streamed_text, text)
            } else {
                streamed_text.push_str(text);
                text.clone()
            };
            if text.is_empty() {
                return dangling;
            }
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{text}");
            let _ = stdout.flush();
            return !text.ends_with('\n');
        }
        CodeEvent::AssistantMessage { text, .. } => {
            let text = reconcile_assistant_text(streamed_text, text);
            streamed_text.clear();
            if text.is_empty() {
                return dangling;
            }
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{text}");
            let _ = stdout.flush();
            return !text.ends_with('\n');
        }
        CodeEvent::ReasoningDelta { text } => {
            if !text.is_empty() {
                eprint!("{text}");
            }
        }
        CodeEvent::ToolStarted {
            name,
            detail,
            parent_call_id,
            ..
        } => {
            if parent_call_id.is_none() {
                streamed_text.clear();
            }
            finish_line(dangling);
            eprintln!("tidebreak: tool {name}  {}", tool_detail(detail));
            return false;
        }
        CodeEvent::ToolCompleted {
            outcome, preview, ..
        } => {
            finish_line(dangling);
            let preview = if preview.is_empty() {
                String::new()
            } else {
                format!("  {}", clip(preview, 80))
            };
            eprintln!("tidebreak: tool {}{preview}", outcome_label(*outcome));
            return false;
        }
        CodeEvent::HarnessNotice { level, message } => {
            finish_line(dangling);
            eprintln!("tidebreak: {}: {message}", level_label(*level));
            return false;
        }
        CodeEvent::FileChanged { path, kind, .. } => {
            finish_line(dangling);
            eprintln!("tidebreak: file {} {path}", kind.as_str_display());
            return false;
        }
        CodeEvent::TurnFailed { error, .. } => {
            streamed_text.clear();
            finish_line(dangling);
            eprintln!("tidebreak: turn failed: {}", error.message);
            return false;
        }
        CodeEvent::TurnInterrupted { .. } => {
            streamed_text.clear();
            finish_line(dangling);
            eprintln!("tidebreak: turn interrupted");
            return false;
        }
        CodeEvent::TurnResumed { .. } => {
            finish_line(dangling);
            eprintln!("tidebreak: turn resumed");
            return false;
        }
        CodeEvent::TurnRefused { refusal, .. } => {
            streamed_text.clear();
            finish_line(dangling);
            eprintln!(
                "tidebreak: turn refused ({})",
                refusal.category().unwrap_or("unspecified")
            );
            return false;
        }
        // `code run` already told the user to decide this one. Say when the
        // window closes, or the prompt is the last thing they ever hear.
        CodeEvent::ApprovalResolved {
            approval_id,
            decision: ApprovalDecisionKind::Abandoned,
        } => {
            finish_line(dangling);
            eprintln!(
                "tidebreak: approval {approval_id} went undecided; the engine stopped waiting"
            );
            return false;
        }
        CodeEvent::TurnStarted { .. } => streamed_text.clear(),
        CodeEvent::TurnCompleted { .. } => streamed_text.clear(),
        CodeEvent::SessionStarted { .. }
        | CodeEvent::ApprovalRequested { .. }
        | CodeEvent::ApprovalResolved { .. }
        | CodeEvent::UserSteered { .. }
        | CodeEvent::CheckpointRecorded { .. }
        | CodeEvent::AttentionChanged { .. }
        | _ => {}
    }
    dangling
}

/// Return the part of a complete assistant tail that has not been printed.
fn reconcile_assistant_text(streamed: &mut String, complete: &str) -> String {
    if let Some(suffix) = complete.strip_prefix(streamed.as_str()) {
        let suffix = suffix.to_owned();
        streamed.clear();
        streamed.push_str(complete);
        return suffix;
    }
    if streamed.starts_with(complete) {
        return String::new();
    }
    streamed.clear();
    streamed.push_str(complete);
    complete.to_owned()
}

fn finish_line(dangling: bool) {
    if dangling {
        println!();
    }
}

fn print_approval_prompt(id: CodeApprovalId) {
    eprintln!();
    eprintln!("tidebreak: approval requested  {id}");
    eprintln!("           decide with:  tidebreak code approve {id}");
    eprintln!("                         tidebreak code deny {id} [-m <feedback>]");
    eprintln!();
}

fn render_update(notice: &CodeUpdateNotice) {
    match notice {
        CodeUpdateNotice::Snapshot { sessions } => {
            eprintln!("tidebreak: watch snapshot  {} session(s)", sessions.len());
            for digest in sessions {
                eprintln!("  {}", digest_line(digest));
            }
        }
        CodeUpdateNotice::Digest {
            workspace,
            session,
            lifecycle,
            attention,
            title,
            turn_count,
            pr_state,
            ..
        } => {
            let pr = pr_state
                .as_deref()
                .map(|pr| format!("  pr #{} {}", pr.number, pr.state))
                .unwrap_or_default();
            println!(
                "{}  {session}  {}  {}  {title}  turns={turn_count}{pr}",
                workspace_label(workspace.as_ref()),
                lifecycle.as_str(),
                attention_label(attention)
            );
        }
        // Progress, delivery, and rewrite notices drive desktop surfaces the
        // watch does not render. Terminal activity is coalesced noise here.
        CodeUpdateNotice::TerminalActivity { .. }
        | CodeUpdateNotice::CloneProgress { .. }
        | CodeUpdateNotice::HarnessInstall { .. }
        | CodeUpdateNotice::Delivery
        | CodeUpdateNotice::TurnRewrite { .. } => {}
    }
}

/// A session with no workspace (the in-process engine's) prints a dash.
fn workspace_label(workspace: Option<&WorkspaceId>) -> String {
    workspace.map_or_else(|| "-".to_owned(), ToString::to_string)
}

fn digest_line(digest: &CodeSessionDigest) -> String {
    let pr = digest
        .pr_state
        .as_ref()
        .map(|pr| format!("  pr #{} {}", pr.number, pr.state))
        .unwrap_or_default();
    format!(
        "{}  {}  {}  {}  {}  turns={}{pr}",
        workspace_label(digest.workspace.as_ref()),
        digest.session,
        digest.lifecycle.as_str(),
        attention_label(&digest.attention),
        digest.title,
        digest.turn_count
    )
}

fn print_workspace(workspace: &CodeWorkspaceSnapshot) {
    println!("id                   {}", workspace.id);
    println!("repo                 {}", workspace.repo_id);
    println!("title                {}", or_dash(&workspace.title));
    println!("status               {}", workspace.status.as_str());
    println!("branch               {}", workspace.branch_name);
    println!("worktree             {}", workspace.worktree_path);
    println!("base                 {}", workspace.base_ref);
    match &workspace.pr {
        Some(pr) => {
            let checks = pr.checks_summary.as_deref().unwrap_or("-");
            let url = pr.url.as_deref().unwrap_or("-");
            println!(
                "pr                   #{} {}  {checks}  {url}",
                pr.number, pr.state
            );
        }
        None => println!("pr                   none"),
    }
}

fn print_session(session: &CodeSessionSnapshot) {
    println!("id                   {}", session.id);
    println!(
        "workspace            {}",
        workspace_label(session.workspace_id.as_ref())
    );
    println!("harness              {}", session.harness_kind.as_str());
    println!(
        "version              {}",
        session.harness_version.as_deref().unwrap_or("-")
    );
    println!("mode                 {}", session.permission_mode.as_str());
    println!("lifecycle            {}", session.lifecycle.as_str());
    println!(
        "attention            {}",
        attention_label(&session.attention)
    );
}

fn print_turn_line(turn: &CodeTurnSnapshot) {
    let stat = turn
        .diffstat
        .as_ref()
        .map(|stat| {
            format!(
                "  +{} -{} ({} files)",
                stat.insertions, stat.deletions, stat.files
            )
        })
        .unwrap_or_default();
    let usage = turn
        .usage
        .as_ref()
        .map(|usage| {
            // Cache reads and writes are most of the prompt on every
            // Anthropic-routed harness, and a cache write bills above base
            // input. Printing only `in=` made the most expensive turn in a
            // session read as the cheapest.
            let mut line = format!("  in={} out={}", usage.input_tokens, usage.output_tokens);
            if usage.cache_read_input_tokens > 0 {
                line.push_str(&format!("  cache-read={}", usage.cache_read_input_tokens));
            }
            if usage.cache_creation_input_tokens > 0 {
                line.push_str(&format!(
                    "  cache-write={}",
                    usage.cache_creation_input_tokens
                ));
            }
            line
        })
        .unwrap_or_default();
    println!(
        "{}\t{}\t{}{stat}{usage}",
        turn.ordinal,
        turn.status.as_str(),
        clip(&turn.user_input.replace('\n', " "), 48)
    );
}

fn print_approval(approval: &CodeApprovalSnapshot) {
    println!(
        "{}\t{}\t{}\t{}",
        approval.id,
        approval.state.as_str(),
        approval_kind(&approval.kind),
        approval.session_id
    );
}

fn print_pr(status: &crate::api::code::CodeWorkspacePrSnapshot) {
    println!(
        "dirty={}  unpushed={}  ahead={}  upstream={}",
        status.dirty, status.unpushed, status.ahead, status.has_upstream
    );
    if !status.suggested_commit_message.is_empty() {
        println!("suggested commit     {}", status.suggested_commit_message);
    }
    match &status.pr {
        Some(pr) => {
            let checks = pr.checks_summary.as_deref().unwrap_or("-");
            let url = pr.url.as_deref().unwrap_or("-");
            println!(
                "pr                   #{} {}  {checks}  {url}",
                pr.number, pr.state
            );
        }
        None => println!("pr                   none"),
    }
    let gh = match status.gh_authenticated {
        Some(true) => "yes",
        Some(false) => "signed out",
        None if status.gh_found => "found",
        None => "missing",
    };
    println!("gh                   {gh}");
    if !status.remediation.is_empty() {
        println!("remediation          {}", status.remediation);
    }
}

fn attention_label(attention: &Attention) -> String {
    match &attention.state {
        AttentionState::Working => "working".to_owned(),
        AttentionState::NeedsYou { prompt, .. } => format!("needs_you: {prompt}"),
        AttentionState::Stalled { idle_secs } => format!("stalled {idle_secs}s"),
        AttentionState::DoneUnreviewed => "done_unreviewed".to_owned(),
        AttentionState::Idle => "idle".to_owned(),
        AttentionState::Fenced { .. } => "fenced".to_owned(),
        AttentionState::Manual { note } => format!("manual: {note}"),
    }
}

fn approval_kind(kind: &CodeApprovalKind) -> String {
    match kind {
        CodeApprovalKind::Command { cmd, .. } => format!("command {cmd}"),
        CodeApprovalKind::FileWrite { paths } => format!("write {}", paths.join(",")),
        CodeApprovalKind::Network { summary } => format!("network {summary}"),
        CodeApprovalKind::Other { summary } => summary.clone(),
        // Decision 0018: the listing shows the literal action, never the
        // call's own display-only narration.
        CodeApprovalKind::ToolUse { preview, .. } => format!("tool {}", tool_action_line(preview)),
        CodeApprovalKind::Questions { questions } => format!("questions ({})", questions.len()),
        CodeApprovalKind::Plan { proposed_mode } => format!("plan -> {proposed_mode}"),
    }
}

/// The literal action a tool_use approval asks consent for, as one line.
/// Argument boundaries survive: an element containing a space is quoted so it
/// still reads as one argument.
fn tool_action_line(preview: &tidebreak_core::ToolActionPreview) -> String {
    use tidebreak_core::ToolActionPreview;
    match preview {
        ToolActionPreview::Exec {
            command,
            args,
            cwd,
            files,
            summary: _,
        } => {
            let mut line = std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .map(quote_argument)
                .collect::<Vec<_>>()
                .join(" ");
            if cwd != "." {
                line.push_str(&format!("  (cwd {cwd})"));
            }
            if !files.is_empty() {
                line.push_str(&format!("  (staged {})", files.join(", ")));
            }
            line
        }
        ToolActionPreview::Search { query, summary: _ } => format!("search: {query}"),
        ToolActionPreview::WebSearch {
            query,
            domains,
            start_published_at,
            end_published_at,
            summary: _,
        } => {
            let mut line = format!("web search: {query}");
            if !domains.is_empty() {
                line.push_str(&format!("  (sites {})", domains.join(", ")));
            }
            if let Some(start) = start_published_at {
                line.push_str(&format!("  (published after {start})"));
            }
            if let Some(end) = end_published_at {
                line.push_str(&format!("  (published before {end})"));
            }
            line
        }
        ToolActionPreview::WebExtract { url, summary: _ } => format!("fetch: {url}"),
        ToolActionPreview::WriteFile { path, summary: _ } => format!("write: {path}"),
        ToolActionPreview::DelegateAgent { task, network: _ } => format!("agent: {task}"),
    }
}

fn quote_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tool_detail(detail: &tidebreak_core::ToolDetail) -> String {
    match detail {
        tidebreak_core::ToolDetail::Command { cmd, cwd } => format!("{cmd}  ({cwd})"),
        tidebreak_core::ToolDetail::FileEdit { path }
        | tidebreak_core::ToolDetail::FileRead { path } => path.clone(),
        tidebreak_core::ToolDetail::Search { query } => query.clone(),
        tidebreak_core::ToolDetail::Other { summary } => summary.clone(),
    }
}

fn outcome_label(outcome: tidebreak_core::ToolOutcome) -> &'static str {
    match outcome {
        tidebreak_core::ToolOutcome::Succeeded => "ok",
        tidebreak_core::ToolOutcome::Failed => "failed",
        tidebreak_core::ToolOutcome::Denied => "denied",
    }
}

fn level_label(level: tidebreak_core::HarnessNoticeLevel) -> &'static str {
    match level {
        tidebreak_core::HarnessNoticeLevel::Info => "notice",
        tidebreak_core::HarnessNoticeLevel::Warning => "warning",
        tidebreak_core::HarnessNoticeLevel::Error => "error",
    }
}

trait FileChangeLabel {
    fn as_str_display(&self) -> &'static str;
}

impl FileChangeLabel for tidebreak_core::FileChangeKind {
    fn as_str_display(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }
}

fn or_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn clip(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut clipped: String = value.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

fn emit<T: serde::Serialize>(value: &T) -> Result<i32> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|error| AgentError::msg(format!("could not encode json: {error}")))?
    );
    Ok(0)
}

fn emit_ok<T: serde::Serialize>(value: &T) -> Result<i32> {
    emit(value)
}

fn emit_line(raw: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{raw}");
    let _ = stdout.flush();
}

struct CodeStream {
    socket: crate::api::client::EventSocket,
    last_seq: i64,
}

impl CodeStream {
    async fn open_session(client: &Client, session: CodeSessionId) -> Result<Self> {
        Ok(Self {
            socket: client.open_code_events(session, 0).await?,
            last_seq: 0,
        })
    }

    async fn open_updates(client: &Client) -> Result<Self> {
        Ok(Self {
            socket: client.open_code_updates().await?,
            last_seq: 0,
        })
    }

    async fn next_session(
        &mut self,
        client: &Client,
        session: CodeSessionId,
    ) -> Result<Option<(String, crate::api::code::SequencedCodeEventFrame)>> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => match decode_event_frame(&text) {
                Ok(frame) => {
                    self.last_seq = frame.seq;
                    Ok(Some((text.to_string(), frame)))
                }
                Err(_) => Ok(None),
            },
            Some(Ok(_)) => Ok(None),
            Some(Err(_)) | None => {
                self.reconnect_session(client, session).await?;
                Ok(None)
            }
        }
    }

    async fn next_updates(
        &mut self,
        client: &Client,
    ) -> Result<Option<(String, CodeUpdateNotice)>> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => match decode_update_notice(&text) {
                Ok(notice) => Ok(Some((text.to_string(), notice))),
                Err(_) => Ok(None),
            },
            Some(Ok(_)) => Ok(None),
            Some(Err(_)) | None => {
                self.reconnect_updates(client).await?;
                Ok(None)
            }
        }
    }

    async fn reconnect_session(&mut self, client: &Client, session: CodeSessionId) -> Result<()> {
        let mut last = None;
        for _ in 0..RECONNECT_ATTEMPTS {
            tokio::time::sleep(RECONNECT_DELAY).await;
            match client.open_code_events(session, self.last_seq).await {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::msg(format!(
            "the session event stream closed and could not be reopened{}",
            last.map(|error| format!(": {error}")).unwrap_or_default()
        )))
    }

    async fn reconnect_updates(&mut self, client: &Client) -> Result<()> {
        let mut last = None;
        for _ in 0..RECONNECT_ATTEMPTS {
            tokio::time::sleep(RECONNECT_DELAY).await;
            match client.open_code_updates().await {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::msg(format!(
            "the updates stream closed and could not be reopened{}",
            last.map(|error| format!(": {error}")).unwrap_or_default()
        )))
    }
}

/// Same interrupt watcher `-p` uses: register before waiting, second Ctrl-C
/// exits immediately.
struct Interrupt(tokio::sync::watch::Receiver<bool>);

impl Interrupt {
    async fn watch() -> Self {
        let (fired, seen) = tokio::sync::watch::channel(false);
        let (installed, registered) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut signal = std::pin::pin!(tokio::signal::ctrl_c());
            let first = std::future::poll_fn(|context| {
                std::task::Poll::Ready(signal.as_mut().poll(context))
            })
            .await;
            let _ = installed.send(());
            let interrupted = match first {
                std::task::Poll::Ready(result) => result.is_ok(),
                std::task::Poll::Pending => signal.await.is_ok(),
            };
            if !interrupted {
                return;
            }
            let _ = fired.send(true);
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(EXIT_INTERRUPTED);
            }
        });
        let _ = registered.await;
        Self(seen)
    }

    async fn fired(&mut self) {
        while !*self.0.borrow_and_update() {
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

struct Cursor {
    args: Vec<String>,
    at: usize,
}

impl Cursor {
    fn new(args: Vec<String>) -> Self {
        Self { args, at: 0 }
    }

    fn next(&mut self) -> Option<String> {
        let value = self.args.get(self.at).cloned();
        if value.is_some() {
            self.at += 1;
        }
        value
    }

    fn value(&mut self, flag: &str) -> std::result::Result<String, String> {
        match self.next() {
            Some(value) if !value.starts_with("--") => Ok(value),
            _ => Err(format!("{flag} requires a value")),
        }
    }

    fn positional(&mut self, what: &str) -> std::result::Result<String, String> {
        match self.next() {
            Some(value) if !value.starts_with("--") => Ok(value),
            _ => Err(format!("expected {what}")),
        }
    }
}

struct SharedFlags {
    format: OutputFormat,
}

fn take_format(
    flags: &mut SharedFlags,
    cursor: &mut Cursor,
    flag: &str,
) -> std::result::Result<(), String> {
    match flag {
        "--json" => {
            flags.format = OutputFormat::Json;
            Ok(())
        }
        "--output-format" => {
            let value = cursor.value("--output-format")?;
            flags.format = OutputFormat::parse(&value)
                .ok_or_else(|| "--output-format expects text or json".to_owned())?;
            Ok(())
        }
        _ => Err(format!("unknown argument {flag:?}")),
    }
}

fn parse_doctor(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut refresh = false;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--refresh" => refresh = true,
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    Ok(Command::Doctor {
        refresh,
        format: flags.format,
    })
}

fn parse_repo(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let verb = cursor.positional("a repo subcommand")?;
    match verb.as_str() {
        "add" => {
            let mut path = None;
            let mut name = None;
            let mut base_ref = None;
            let mut branch_prefix = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--name" => name = Some(cursor.value("--name")?),
                    "--base-ref" => base_ref = Some(cursor.value("--base-ref")?),
                    "--branch-prefix" => branch_prefix = Some(cursor.value("--branch-prefix")?),
                    other if other.starts_with("--") => take_format(&mut flags, cursor, other)?,
                    other if path.is_none() => path = Some(other.to_owned()),
                    other => return Err(format!("unexpected repo add argument {other:?}")),
                }
            }
            let path = path.ok_or_else(|| "repo add requires a path".to_owned())?;
            Ok(Command::RepoAdd {
                path: PathBuf::from(path),
                name,
                base_ref,
                branch_prefix,
                format: flags.format,
            })
        }
        "list" => {
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                take_format(&mut flags, cursor, &arg)?;
            }
            Ok(Command::RepoList {
                format: flags.format,
            })
        }
        "rm" => {
            let mut id = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                if arg.starts_with("--") {
                    take_format(&mut flags, cursor, &arg)?;
                } else if id.is_none() {
                    id = Some(parse_repo_id(&arg)?);
                } else {
                    return Err(format!("unexpected repo rm argument {arg:?}"));
                }
            }
            let id = id.ok_or_else(|| "repo rm requires a repository id".to_owned())?;
            Ok(Command::RepoRm {
                id,
                format: flags.format,
            })
        }
        other => Err(format!("unknown repo subcommand {other:?}")),
    }
}

fn parse_ws(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let verb = cursor.positional("a ws subcommand")?;
    match verb.as_str() {
        "new" => {
            let mut repo = None;
            let mut title = None;
            let mut base_ref = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--repo" => repo = Some(cursor.value("--repo")?),
                    "--title" => title = Some(cursor.value("--title")?),
                    "--base-ref" => base_ref = Some(cursor.value("--base-ref")?),
                    other => take_format(&mut flags, cursor, other)?,
                }
            }
            let repo = repo.ok_or_else(|| "ws new requires --repo <id|path>".to_owned())?;
            Ok(Command::WsNew {
                repo,
                title,
                base_ref,
                format: flags.format,
            })
        }
        "list" => {
            let mut repo = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--repo" => repo = Some(cursor.value("--repo")?),
                    other => take_format(&mut flags, cursor, other)?,
                }
            }
            Ok(Command::WsList {
                repo,
                format: flags.format,
            })
        }
        "show" => {
            let (id, format) = take_id_and_format(cursor, "a workspace id", parse_workspace_id)?;
            Ok(Command::WsShow { id, format })
        }
        "archive" => {
            let mut id = None;
            let mut force = false;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--force" => force = true,
                    other if other.starts_with("--") => take_format(&mut flags, cursor, other)?,
                    other if id.is_none() => id = Some(parse_workspace_id(other)?),
                    other => return Err(format!("unexpected ws archive argument {other:?}")),
                }
            }
            let id = id.ok_or_else(|| "ws archive requires a workspace id".to_owned())?;
            Ok(Command::WsArchive {
                id,
                force,
                format: flags.format,
            })
        }
        other => Err(format!("unknown ws subcommand {other:?}")),
    }
}

fn parse_session(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let verb = cursor.positional("a session subcommand")?;
    match verb.as_str() {
        "start" => {
            let mut workspace = None;
            let mut harness = None;
            let mut mode = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
                    "--harness" => harness = Some(parse_harness(&cursor.value("--harness")?)?),
                    "--mode" => mode = Some(parse_mode(&cursor.value("--mode")?)?),
                    other => take_format(&mut flags, cursor, other)?,
                }
            }
            let workspace =
                workspace.ok_or_else(|| "session start requires --ws <id>".to_owned())?;
            let harness =
                harness.ok_or_else(|| "session start requires --harness <kind>".to_owned())?;
            Ok(Command::SessionStart {
                workspace,
                harness,
                mode,
                format: flags.format,
            })
        }
        "show" => {
            let (id, format) = take_id_and_format(cursor, "a session id", parse_session_id)?;
            Ok(Command::SessionShow { id, format })
        }
        "reap" => {
            let (id, format) = take_id_and_format(cursor, "a session id", parse_session_id)?;
            Ok(Command::SessionReap { id, format })
        }
        "mode" => {
            let id = cursor
                .next()
                .ok_or_else(|| "expected a session id".to_owned())
                .and_then(|raw| parse_session_id(&raw))?;
            let raw = cursor
                .next()
                .ok_or_else(|| "expected plan|ask|auto|allow".to_owned())?;
            let mode = PermissionMode::from_str(&raw)
                .ok_or_else(|| format!("unknown permission mode {raw:?}"))?;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                if arg.starts_with("--") {
                    take_format(&mut flags, cursor, &arg)?;
                } else {
                    return Err(format!("unexpected code session mode argument {arg:?}"));
                }
            }
            Ok(Command::SessionMode {
                id,
                mode,
                format: flags.format,
            })
        }
        other => Err(format!("unknown session subcommand {other:?}")),
    }
}

fn parse_run(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut session = None;
    let mut workspace = None;
    let mut on_approval = OnApproval::Wait;
    let mut timeout = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    let mut message_parts = Vec::new();
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--session" => session = Some(parse_session_id(&cursor.value("--session")?)?),
            "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
            "--on-approval" => {
                on_approval = parse_on_approval(&cursor.value("--on-approval")?)?;
            }
            "--timeout" => timeout = Some(parse_timeout(&cursor.value("--timeout")?)?),
            "--json" | "--output-format" => take_format(&mut flags, cursor, &arg)?,
            other if other.starts_with("--") => {
                return Err(format!("unknown code run argument {other:?}"));
            }
            other => message_parts.push(other.to_owned()),
        }
    }
    if session.is_none() && workspace.is_none() {
        return Err("code run requires --session <id> or --ws <id>".to_owned());
    }
    Ok(Command::Run {
        session,
        workspace,
        message: message_parts.join(" "),
        on_approval,
        timeout,
        format: flags.format,
    })
}

fn parse_approvals(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut session = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--session" => session = Some(parse_session_id(&cursor.value("--session")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    Ok(Command::Approvals {
        session,
        format: flags.format,
    })
}

fn parse_approve(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let (id, format) = take_id_and_format(cursor, "an approval id", parse_approval_id)?;
    Ok(Command::Approve { id, format })
}

fn parse_deny(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut id = None;
    let mut feedback = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "-m" | "--message" | "--feedback" => feedback = Some(cursor.value("-m")?),
            other if other.starts_with("--") => take_format(&mut flags, cursor, other)?,
            other if id.is_none() => id = Some(parse_approval_id(other)?),
            other => return Err(format!("unexpected deny argument {other:?}")),
        }
    }
    let id = id.ok_or_else(|| "deny requires an approval id".to_owned())?;
    Ok(Command::Deny {
        id,
        feedback,
        format: flags.format,
    })
}

fn parse_interrupt(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut session = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--session" => session = Some(parse_session_id(&cursor.value("--session")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    let session = session.ok_or_else(|| "interrupt requires --session <id>".to_owned())?;
    Ok(Command::Interrupt {
        session,
        format: flags.format,
    })
}

fn parse_turns(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut session = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--session" => session = Some(parse_session_id(&cursor.value("--session")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    let session = session.ok_or_else(|| "turns requires --session <id>".to_owned())?;
    Ok(Command::Turns {
        session,
        format: flags.format,
    })
}

fn parse_diff(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut workspace = None;
    let mut turn = None;
    let mut file = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
            "--turn" => turn = Some(parse_turn_ref(&cursor.value("--turn")?)?),
            "--file" => file = Some(cursor.value("--file")?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    let workspace = workspace.ok_or_else(|| "diff requires --ws <id>".to_owned())?;
    Ok(Command::Diff {
        workspace,
        turn,
        file,
        format: flags.format,
    })
}

fn parse_files(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut workspace = None;
    let mut turn = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
            "--turn" => turn = Some(parse_turn_ref(&cursor.value("--turn")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    let workspace = workspace.ok_or_else(|| "files requires --ws <id>".to_owned())?;
    Ok(Command::Files {
        workspace,
        turn,
        format: flags.format,
    })
}

fn parse_git(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let verb = cursor.positional("a git subcommand")?;
    match verb.as_str() {
        "commit" => {
            let mut workspace = None;
            let mut message = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
                    "-m" | "--message" => message = Some(cursor.value("-m")?),
                    other => take_format(&mut flags, cursor, other)?,
                }
            }
            let workspace = workspace.ok_or_else(|| "git commit requires --ws <id>".to_owned())?;
            Ok(Command::GitCommit {
                workspace,
                message,
                format: flags.format,
            })
        }
        "push" => {
            let (workspace, format) = take_ws_flag(cursor, "git push")?;
            Ok(Command::GitPush { workspace, format })
        }
        "pr" => {
            let mut workspace = None;
            let mut title = None;
            let mut body = None;
            let mut flags = SharedFlags {
                format: OutputFormat::Text,
            };
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
                    "--title" => title = Some(cursor.value("--title")?),
                    "--body" => body = Some(cursor.value("--body")?),
                    other => take_format(&mut flags, cursor, other)?,
                }
            }
            let workspace = workspace.ok_or_else(|| "git pr requires --ws <id>".to_owned())?;
            Ok(Command::GitPr {
                workspace,
                title,
                body,
                format: flags.format,
            })
        }
        "status" => {
            let (workspace, format) = take_ws_flag(cursor, "git status")?;
            Ok(Command::GitStatus { workspace, format })
        }
        other => Err(format!("unknown git subcommand {other:?}")),
    }
}

fn parse_action(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut name = None;
    let mut workspace = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
            other if other.starts_with("--") => take_format(&mut flags, cursor, other)?,
            other if name.is_none() => name = Some(other.to_owned()),
            other => return Err(format!("unexpected action argument {other:?}")),
        }
    }
    let name = name.ok_or_else(|| "action requires a name".to_owned())?;
    let workspace = workspace.ok_or_else(|| "action requires --ws <id>".to_owned())?;
    Ok(Command::Action {
        name,
        workspace,
        format: flags.format,
    })
}

fn parse_watch(cursor: &mut Cursor) -> std::result::Result<Command, String> {
    let mut once = false;
    let mut timeout = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--once" => once = true,
            "--timeout" => timeout = Some(parse_timeout(&cursor.value("--timeout")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    Ok(Command::Watch {
        once,
        timeout,
        format: flags.format,
    })
}

fn parse_timeout(value: &str) -> std::result::Result<u64, String> {
    let stripped = value.strip_suffix('s').unwrap_or(value);
    stripped
        .parse::<u64>()
        .ok()
        .filter(|secs| *secs > 0)
        .ok_or_else(|| "--timeout expects a positive number of seconds".to_owned())
}

fn take_ws_flag(
    cursor: &mut Cursor,
    context: &str,
) -> std::result::Result<(WorkspaceId, OutputFormat), String> {
    let mut workspace = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        match arg.as_str() {
            "--ws" => workspace = Some(parse_workspace_id(&cursor.value("--ws")?)?),
            other => take_format(&mut flags, cursor, other)?,
        }
    }
    let workspace = workspace.ok_or_else(|| format!("{context} requires --ws <id>"))?;
    Ok((workspace, flags.format))
}

fn take_id_and_format<T>(
    cursor: &mut Cursor,
    what: &str,
    parse_id: fn(&str) -> std::result::Result<T, String>,
) -> std::result::Result<(T, OutputFormat), String> {
    let mut id = None;
    let mut flags = SharedFlags {
        format: OutputFormat::Text,
    };
    while let Some(arg) = cursor.next() {
        if arg.starts_with("--") {
            take_format(&mut flags, cursor, &arg)?;
        } else if id.is_none() {
            id = Some(parse_id(&arg)?);
        } else {
            return Err(format!("unexpected argument {arg:?}"));
        }
    }
    let id = id.ok_or_else(|| format!("expected {what}"))?;
    Ok((id, flags.format))
}

fn parse_repo_id(value: &str) -> std::result::Result<RepoId, String> {
    RepoId::from_str(value).map_err(|_| "expected a repository UUID".to_owned())
}

fn parse_workspace_id(value: &str) -> std::result::Result<WorkspaceId, String> {
    WorkspaceId::from_str(value).map_err(|_| "expected a workspace UUID".to_owned())
}

fn parse_session_id(value: &str) -> std::result::Result<CodeSessionId, String> {
    CodeSessionId::from_str(value).map_err(|_| "expected a session UUID".to_owned())
}

fn parse_approval_id(value: &str) -> std::result::Result<CodeApprovalId, String> {
    CodeApprovalId::from_str(value).map_err(|_| "expected an approval UUID".to_owned())
}

fn parse_turn_ref(value: &str) -> std::result::Result<TurnRef, String> {
    if let Ok(id) = CodeTurnId::from_str(value) {
        return Ok(TurnRef::Id(id));
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|ordinal| *ordinal > 0)
        .map(TurnRef::Ordinal)
        .ok_or_else(|| "--turn expects a positive ordinal or a turn UUID".to_owned())
}

fn parse_harness(value: &str) -> std::result::Result<HarnessKind, String> {
    let token = value.replace('-', "_");
    HarnessKind::from_str(&token)
        .ok_or_else(|| "expected a harness kind: claude_code, codex, opencode, or grok".to_owned())
}

fn parse_mode(value: &str) -> std::result::Result<PermissionMode, String> {
    PermissionMode::from_str(value)
        .ok_or_else(|| "--mode expects plan, ask, auto, or allow".to_owned())
}

fn parse_on_approval(value: &str) -> std::result::Result<OnApproval, String> {
    match value {
        "wait" => Ok(OnApproval::Wait),
        "fail" => Ok(OnApproval::Fail),
        _ => Err("--on-approval expects wait or fail".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn id() -> String {
        uuid::Uuid::nil().to_string()
    }

    #[test]
    fn a_tool_use_listing_shows_the_literal_action_not_the_narration() {
        let kind = CodeApprovalKind::ToolUse {
            preview: tidebreak_core::ToolActionPreview::Exec {
                command: "rm".into(),
                args: vec!["-rf".into(), "two words".into()],
                cwd: "work".into(),
                files: vec!["notes.md".into()],
                summary: Some("Cleaning temporary caches".into()),
            },
            offered_grants: Vec::new(),
        };
        let line = approval_kind(&kind);
        assert_eq!(
            line,
            "tool rm -rf 'two words'  (cwd work)  (staged notes.md)"
        );
        assert!(!line.contains("Cleaning"));
    }

    #[test]
    fn every_verb_parses_its_required_shape() {
        let ws = id();
        let session = id();
        let approval = id();
        let repo = id();

        assert!(matches!(
            parse(args(&["doctor", "--refresh", "--json"])).unwrap(),
            Command::Doctor {
                refresh: true,
                format: OutputFormat::Json
            }
        ));
        assert!(matches!(
            parse(args(&["repo", "add", "/tmp/proj", "--name", "proj"])).unwrap(),
            Command::RepoAdd { .. }
        ));
        assert!(matches!(
            parse(args(&["repo", "list"])).unwrap(),
            Command::RepoList { .. }
        ));
        assert!(matches!(
            parse(args(&["repo", "rm", &repo])).unwrap(),
            Command::RepoRm { .. }
        ));
        assert!(matches!(
            parse(args(&[
                "ws",
                "new",
                "--repo",
                "/tmp/proj",
                "--title",
                "fix"
            ]))
            .unwrap(),
            Command::WsNew { .. }
        ));
        assert!(matches!(
            parse(args(&["ws", "list", "--repo", &repo])).unwrap(),
            Command::WsList { .. }
        ));
        assert!(matches!(
            parse(args(&["ws", "show", &ws])).unwrap(),
            Command::WsShow { .. }
        ));
        assert!(matches!(
            parse(args(&["ws", "archive", &ws, "--force"])).unwrap(),
            Command::WsArchive { force: true, .. }
        ));
        match parse(args(&[
            "session",
            "start",
            "--ws",
            &ws,
            "--harness",
            "claude-code",
        ]))
        .unwrap()
        {
            Command::SessionStart {
                harness,
                mode,
                format,
                ..
            } => {
                assert_eq!(harness, HarnessKind::ClaudeCode);
                assert_eq!(mode, None);
                assert_eq!(format, OutputFormat::Text);
            }
            other => panic!("{other:?}"),
        }
        match parse(args(&[
            "session",
            "start",
            "--ws",
            &ws,
            "--harness",
            "grok",
            "--mode",
            "ask",
        ]))
        .unwrap()
        {
            Command::SessionStart { mode, harness, .. } => {
                assert_eq!(harness, HarnessKind::Grok);
                assert_eq!(mode, Some(PermissionMode::Ask));
            }
            other => panic!("{other:?}"),
        }
        match parse(args(&[
            "run",
            "--session",
            &session,
            "--on-approval",
            "fail",
            "--json",
            "ship",
            "it",
        ]))
        .unwrap()
        {
            Command::Run {
                message,
                on_approval,
                format,
                ..
            } => {
                assert_eq!(message, "ship it");
                assert_eq!(on_approval, OnApproval::Fail);
                assert_eq!(format, OutputFormat::Json);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(args(&["approve", &approval])).unwrap(),
            Command::Approve { .. }
        ));
        match parse(args(&["deny", &approval, "-m", "no"])).unwrap() {
            Command::Deny { feedback, .. } => assert_eq!(feedback.as_deref(), Some("no")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(args(&["interrupt", "--session", &session])).unwrap(),
            Command::Interrupt { .. }
        ));
        assert!(matches!(
            parse(args(&["turns", "--session", &session])).unwrap(),
            Command::Turns { .. }
        ));
        match parse(args(&[
            "diff", "--ws", &ws, "--turn", "2", "--file", "a.rs",
        ]))
        .unwrap()
        {
            Command::Diff {
                turn: Some(TurnRef::Ordinal(2)),
                file,
                ..
            } => assert_eq!(file.as_deref(), Some("a.rs")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(args(&["git", "commit", "--ws", &ws, "-m", "wip"])).unwrap(),
            Command::GitCommit { .. }
        ));
        assert!(matches!(
            parse(args(&["action", "lint", "--ws", &ws])).unwrap(),
            Command::Action { .. }
        ));
        match parse(args(&["watch", "--json", "--once", "--timeout", "5"])).unwrap() {
            Command::Watch {
                once,
                timeout,
                format,
            } => {
                assert!(once);
                assert_eq!(timeout, Some(5));
                assert_eq!(format, OutputFormat::Json);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refusals_match_the_existing_walker() {
        assert!(parse(args(&[])).is_err());
        assert!(parse(args(&["nope"])).is_err());
        assert!(parse(args(&["repo"])).is_err());
        assert!(parse(args(&["repo", "add"])).is_err());
        assert!(parse(args(&["repo", "rm", "not-a-uuid"])).is_err());
        assert!(parse(args(&["ws", "new"])).is_err());
        assert!(parse(args(&["session", "start", "--ws", &id()])).is_err());
        assert!(parse(args(&["run", "hello"])).is_err());
        assert!(parse(args(&["run"])).is_err());
        assert!(parse(args(&[
            "run",
            "--session",
            &id(),
            "--on-approval",
            "yolo",
            "x"
        ]))
        .is_err());
        assert!(parse(args(&["interrupt"])).is_err());
        assert!(parse(args(&["diff"])).is_err());
        assert!(parse(args(&["git", "push"])).is_err());
        assert!(parse(args(&["action", "lint"])).is_err());
        assert!(parse(args(&["doctor", "--wat"])).is_err());
        assert!(parse(args(&["watch", "--output-format", "yaml"])).is_err());
        assert!(parse(args(&["watch", "--timeout", "0"])).is_err());
        assert!(parse(args(&["run", "--session", &id(), "--timeout", "nope", "x"])).is_err());
        assert!(parse(args(&[
            "session",
            "start",
            "--ws",
            &id(),
            "--harness",
            "nope"
        ]))
        .is_err());
    }

    #[test]
    fn json_is_accepted_as_a_flag_or_as_output_format() {
        let parsed = parse(args(&["repo", "list", "--output-format", "json"])).unwrap();
        assert!(matches!(
            parsed,
            Command::RepoList {
                format: OutputFormat::Json
            }
        ));
        let parsed = parse(args(&["repo", "list", "--json"])).unwrap();
        assert!(matches!(
            parsed,
            Command::RepoList {
                format: OutputFormat::Json
            }
        ));
    }

    #[test]
    fn run_resolves_a_workspace_instead_of_a_session() {
        let ws = id();
        match parse(args(&["run", "--ws", &ws, "hello"])).unwrap() {
            Command::Run {
                session,
                workspace,
                message,
                on_approval,
                ..
            } => {
                assert!(session.is_none());
                assert!(workspace.is_some());
                assert_eq!(message, "hello");
                assert_eq!(on_approval, OnApproval::Wait);
            }
            other => panic!("{other:?}"),
        }
        match parse(args(&["run", "--session", &id()])).unwrap() {
            Command::Run { message, .. } => assert!(message.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn run_timeout_and_watch_once_parse() {
        let session = id();
        match parse(args(&[
            "run",
            "--session",
            &session,
            "--timeout",
            "30s",
            "go",
        ]))
        .unwrap()
        {
            Command::Run { timeout, .. } => assert_eq!(timeout, Some(30)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn omitted_mode_follows_the_desktop_doctor_default() {
        fn caps(
            structured_approvals: CapLevel,
            plan_mode: CapLevel,
            auto_mode: CapLevel,
            allow_mode: CapLevel,
        ) -> HarnessCaps {
            HarnessCaps {
                resume: CapLevel::Supported,
                streaming_deltas: CapLevel::Supported,
                structured_approvals,
                mid_turn_steering: CapLevel::Unknown,
                plan_mode,
                auto_mode,
                allow_mode,
                reasoning_levels: CapLevel::Unknown,
                native_file_change_events: CapLevel::Unknown,
                native_interrupt: CapLevel::Supported,
                image_input: CapLevel::Unknown,
                slash_commands: CapLevel::Unknown,
                durable_parks: CapLevel::Unsupported,
                user_questions: CapLevel::Unsupported,
                standing_grants: CapLevel::Unsupported,
                mid_turn_resume: CapLevel::Unsupported,
                transcript: CapLevel::Unsupported,
                memory_loopback: CapLevel::Unsupported,
            }
        }
        // Every engine honors Allow, so every engine starts there.
        assert_eq!(
            default_create_permission_mode(Some(&caps(
                CapLevel::Supported,
                CapLevel::Supported,
                CapLevel::Supported,
                CapLevel::Supported,
            ))),
            PermissionMode::Allow
        );
        // Ask is the fallback for an engine with an approval channel and no
        // more autonomous posture, not the default.
        assert_eq!(
            default_create_permission_mode(Some(&caps(
                CapLevel::Supported,
                CapLevel::Supported,
                CapLevel::Unsupported,
                CapLevel::Unsupported,
            ))),
            PermissionMode::Ask
        );
        assert_eq!(
            default_create_permission_mode(Some(&caps(
                CapLevel::Unsupported,
                CapLevel::Supported,
                CapLevel::Unsupported,
                CapLevel::Unsupported,
            ))),
            PermissionMode::Plan
        );
        assert_eq!(
            default_create_permission_mode(Some(&caps(
                CapLevel::Unsupported,
                CapLevel::Unsupported,
                CapLevel::Supported,
                CapLevel::Unsupported,
            ))),
            PermissionMode::Auto
        );
        assert_eq!(
            default_create_permission_mode(Some(&caps(
                CapLevel::Unknown,
                CapLevel::Unknown,
                CapLevel::Unknown,
                CapLevel::Unknown,
            ))),
            PermissionMode::Plan
        );
        assert_eq!(default_create_permission_mode(None), PermissionMode::Plan);
    }

    fn turn(n: u128) -> CodeTurnId {
        CodeTurnId::from(uuid::Uuid::from_u128(n))
    }

    fn completed() -> CodeEvent {
        CodeEvent::TurnCompleted {
            usage: Default::default(),
            checkpoint: None,
            stop_reason: None,
        }
    }

    #[test]
    fn a_queued_submit_does_not_exit_on_the_previous_turns_completion() {
        let mut gate = TurnGate::submit();
        assert_eq!(gate.on_frame(false, &completed()), FrameAction::Skip);
        gate.on_queued();
        assert_eq!(gate.on_frame(false, &completed()), FrameAction::Skip);
        let ours = turn(2);
        assert_eq!(
            gate.on_frame(true, &CodeEvent::TurnStarted { turn_id: ours }),
            FrameAction::Skip,
            "replayed TurnStarted after Queued is not a later live start"
        );
        assert_eq!(
            gate.on_frame(false, &CodeEvent::TurnStarted { turn_id: ours }),
            FrameAction::Render
        );
        assert_eq!(gate.on_frame(false, &completed()), FrameAction::Terminal(0));
    }

    /// The tail drain needs the turn it just finished, and must not claim one
    /// on a stream this submit does not own — otherwise it would wait out the
    /// checkpoint window on somebody else's turn.
    #[test]
    fn bound_turn_is_only_reported_for_a_turn_we_own() {
        let ours = turn(1);
        let mut gate = TurnGate::submit();
        assert_eq!(gate.bound_turn(), None, "nothing bound yet");

        // `on_ran` names the turn, but ownership binds on its live start.
        gate.on_ran(ours);
        assert_eq!(
            gate.bound_turn(),
            None,
            "named but not started: no tail to wait for yet"
        );
        assert_eq!(
            gate.on_frame(false, &CodeEvent::TurnStarted { turn_id: ours }),
            FrameAction::Render
        );
        assert_eq!(gate.bound_turn(), Some(ours));
        assert_eq!(gate.on_frame(false, &completed()), FrameAction::Terminal(0));
        assert_eq!(
            gate.bound_turn(),
            Some(ours),
            "still ours after the terminal frame, so the checkpoint can be drained"
        );

        // An attach-owned turn is ours immediately.
        let attached = turn(2);
        assert_eq!(TurnGate::attach(attached).bound_turn(), Some(attached));
    }

    #[test]
    fn attach_owns_the_running_turn_without_waiting_for_its_start() {
        let id = turn(7);
        let mut gate = TurnGate::attach(id);
        assert_eq!(
            gate.on_frame(true, &completed()),
            FrameAction::Skip,
            "replayed terminal of an earlier turn is not this attach"
        );
        assert_eq!(
            gate.on_frame(false, &CodeEvent::AssistantDelta { text: "hi".into() }),
            FrameAction::Render
        );
        assert_eq!(gate.on_frame(false, &completed()), FrameAction::Terminal(0));
    }

    #[test]
    fn a_replacement_tail_only_renders_text_not_already_printed() {
        let mut streamed = "first second ".to_owned();
        assert_eq!(
            reconcile_assistant_text(&mut streamed, "first second third"),
            "third"
        );
        assert_eq!(streamed, "first second third");
        assert_eq!(
            reconcile_assistant_text(&mut streamed, "first second third"),
            ""
        );
    }

    #[test]
    fn a_bounded_replacement_does_not_repeat_a_longer_printed_prefix() {
        let mut streamed = "first second third".to_owned();
        assert_eq!(reconcile_assistant_text(&mut streamed, "first second "), "");
        assert_eq!(streamed, "first second third");
    }

    #[test]
    fn pick_active_session_prefers_running_then_newest_usable() {
        assert!(pick_active_session(&[]).is_none());
    }
}
