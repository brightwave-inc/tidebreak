//! Post-turn pull-request fact detection (decision 77).
//!
//! After a turn closes, this module reads the turn's journaled shell
//! commands, recognizes `gh pr create` and `git push`, and confirms each
//! against GitHub before writing a `code_pull_request` fact and its workspace
//! attribution. It also checks a workspace checkout changed by the turn when
//! the checkout is clean, still on the workspace branch, and its current
//! commit exactly matches an open pull request head. The transcript and
//! checkout are evidence, never the fact: no row is written without a
//! confirming `gh` read.
//!
//! Everything here is best-effort. A detector failure never fails the turn,
//! and a miss is corrected by the reconcile sweep's exact-tier matching.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::debug;

use tidebreak_core::db::code::{
    get_turn, get_workspace, insert_pull_request_attribution, list_recent_events,
    promote_attribution_to_authored, save_pull_request_fact,
};
use tidebreak_core::{
    CodeEvent, CodePullRequestAttribution, CodePullRequestDiscovery, CodePullRequestFact,
    CodePullRequestId, CodePullRequestRelation, CodePullRequestState, CodeSession, CodeSessionId,
    CodeTurnId, DbStore, OwnerId, ToolDetail, ToolOutcome, WorkspaceId,
};
use tidebreak_shell_policy::simple_command_argvs;

use super::delivery::{git_read, parse_repository_input, repository_target_from_path};
use super::gh::{list_pull_requests_for_head_raw, view_pull_request_raw};
use crate::routes::code::types::CodeGitHubRepositoryTarget;

/// Journal tail read per turn. A turn longer than this loses its earliest
/// commands to the detector; the reconcile sweep is the backstop.
const DETECTOR_EVENT_WINDOW: u64 = 400;
/// Command lines parsed per turn.
const MAX_COMMANDS_PER_TURN: usize = 200;
/// Confirming GitHub reads per turn, creates before pushes.
const MAX_CONFIRM_READS_PER_TURN: usize = 4;

/// One journaled shell command with its completion, joined on `call_id`.
struct RecordedCommand {
    seq: i64,
    cmd: String,
    cwd: String,
    parent_call_id: Option<String>,
    succeeded: bool,
}

/// A recognized `gh pr create` invocation.
#[derive(Debug, PartialEq, Eq)]
struct CreateAct {
    /// `--repo`/`-R` value, when given.
    repo_flag: Option<String>,
    /// `--head`/`-H` value, when given.
    head_flag: Option<String>,
}

/// A recognized `git push` invocation.
#[derive(Debug, PartialEq, Eq)]
struct PushAct {
    /// `git -C <path>` override, when given.
    cwd_override: Option<String>,
    /// First non-option operand after `push`, when given.
    remote: Option<String>,
    /// Destination branch from the last refspec operand, when given.
    branch: Option<String>,
}

/// Scan one closed turn's commands and changed checkout for confirmed facts.
///
/// Never returns an error and never touches the turn: every failure is a
/// debug log. Runs after the turn's terminal event is journaled, so the
/// tail walk finds the whole turn above its `TurnStarted` marker.
///
/// A confirmed act marks the workspace hot (issue 2799). The agent's own
/// push moves the head the watch assesses, and nothing else dirties the row
/// for it: without the mark the next assessment reads a pre-push head, calls
/// its own fix turn a repeat, and parks the watch.
pub(crate) async fn sweep_turn_for_pull_request_acts(
    db: &DbStore,
    session: &CodeSession,
    turn_id: CodeTurnId,
    gh_search_path: Option<&str>,
    hot: Option<&super::pr_refresh::HotPullRequests>,
) {
    let events =
        match list_recent_events(db, &session.owner, session.id, DETECTOR_EVENT_WINDOW).await {
            Ok(events) => events,
            Err(err) => {
                debug!(session = %session.id, "pr fact detector could not read the journal: {err}");
                return;
            }
        };

    // Newest first: completions arrive before their starts. The first record
    // per call id wins, which is the completion's corrected detail when the
    // engine sent one.
    let mut previews: HashMap<String, String> = HashMap::new();
    let mut outcomes: HashMap<String, bool> = HashMap::new();
    let mut commands: HashMap<String, RecordedCommand> = HashMap::new();
    for sequenced in &events {
        match &sequenced.event {
            CodeEvent::TurnStarted { turn_id: started } if *started == turn_id => break,
            CodeEvent::ToolCompleted {
                call_id,
                outcome,
                preview,
                detail,
                parent_call_id,
            } => {
                previews
                    .entry(call_id.clone())
                    .or_insert_with(|| preview.clone());
                outcomes
                    .entry(call_id.clone())
                    .or_insert(matches!(outcome, ToolOutcome::Succeeded));
                if let Some(ToolDetail::Command { cmd, cwd }) = detail {
                    commands.entry(call_id.clone()).or_insert(RecordedCommand {
                        seq: sequenced.seq,
                        cmd: cmd.clone(),
                        cwd: cwd.clone(),
                        parent_call_id: parent_call_id.clone(),
                        succeeded: false,
                    });
                }
            }
            CodeEvent::ToolStarted {
                call_id,
                detail: ToolDetail::Command { cmd, cwd },
                parent_call_id,
                ..
            } => {
                commands.entry(call_id.clone()).or_insert(RecordedCommand {
                    seq: sequenced.seq,
                    cmd: cmd.clone(),
                    cwd: cwd.clone(),
                    parent_call_id: parent_call_id.clone(),
                    succeeded: false,
                });
            }
            _ => {}
        }
    }

    let mut recorded: Vec<(String, RecordedCommand)> = commands
        .into_iter()
        .map(|(call_id, mut command)| {
            command.succeeded = outcomes.get(&call_id).copied().unwrap_or(false);
            (call_id, command)
        })
        .collect();
    recorded.sort_by_key(|(_, command)| command.seq);

    let mut confirms = 0usize;
    let mut confirmed_any = false;
    let mut seen_creates: HashSet<String> = HashSet::new();
    let mut seen_pushes: HashSet<String> = HashSet::new();
    let mut pushes: Vec<(RecordedCommand, PushAct)> = Vec::new();

    // Creates first: an authored claim outranks a contributed one, and the
    // confirm budget should never be spent on pushes before creates.
    for (call_id, command) in recorded.into_iter().take(MAX_COMMANDS_PER_TURN) {
        // A command the engine reports as failed cannot have created or
        // pushed anything; confirming it against the host could match an
        // older pull request on the same branch and mis-attribute it.
        if !command.succeeded {
            continue;
        }
        let Some(argvs) = simple_command_argvs(&command.cmd) else {
            continue;
        };
        for argv in &argvs {
            if let Some(create) = parse_create(argv) {
                let key = format!(
                    "{}\u{1f}{}\u{1f}{}",
                    command.cwd,
                    create.repo_flag.as_deref().unwrap_or(""),
                    create.head_flag.as_deref().unwrap_or("")
                );
                if !seen_creates.insert(key) {
                    continue;
                }
                if confirms >= MAX_CONFIRM_READS_PER_TURN {
                    continue;
                }
                confirms += 1;
                confirmed_any |= confirm_create(
                    db,
                    session,
                    &command,
                    &create,
                    previews.get(&call_id).map(String::as_str),
                    gh_search_path,
                )
                .await;
            } else if let Some(push) = parse_push(argv) {
                let key = format!(
                    "{}\u{1f}{}\u{1f}{}",
                    push.cwd_override.as_deref().unwrap_or(&command.cwd),
                    push.remote.as_deref().unwrap_or(""),
                    push.branch.as_deref().unwrap_or("")
                );
                if !seen_pushes.insert(key) {
                    continue;
                }
                pushes.push((
                    RecordedCommand {
                        seq: command.seq,
                        cmd: command.cmd.clone(),
                        cwd: command.cwd.clone(),
                        parent_call_id: command.parent_call_id.clone(),
                        succeeded: true,
                    },
                    push,
                ));
            }
        }
    }

    for (command, push) in pushes {
        if confirms >= MAX_CONFIRM_READS_PER_TURN {
            break;
        }
        confirms += 1;
        confirmed_any |= confirm_push(db, session, &command, &push, gh_search_path).await;
    }

    // A long turn can push its create or push command out of the bounded
    // journal tail. Other GitHub clients can also open the pull request
    // without producing either command. The checkpoint records whether this
    // turn changed the workspace checkout. A clean checkout still on the
    // workspace branch whose local HEAD exactly matches an open host head is
    // enough to recover that missed tie without attributing a read-only review
    // or an unpushed edit.
    if confirms < MAX_CONFIRM_READS_PER_TURN {
        confirmed_any |= confirm_changed_checkout(db, session, turn_id, gh_search_path).await;
    }

    // The turn moved this workspace's pull request. Nothing else marks it:
    // route mutations dirty the row for the user's own actions, and the
    // agent's push is neither. Left unmarked, the watch's next assessment
    // reads the head from before the fix turn pushed (issue 2799).
    if confirmed_any {
        if let (Some(hot), Some(workspace_id)) = (hot, session.workspace_id) {
            hot.mark(&session.owner, workspace_id);
        }
    }
}

/// Confirm the changed workspace checkout against an open pull request head.
///
/// Reports whether a fact landed, so the caller can mark the workspace hot.
async fn confirm_changed_checkout(
    db: &DbStore,
    session: &CodeSession,
    turn_id: CodeTurnId,
    gh_search_path: Option<&str>,
) -> bool {
    let turn = match get_turn(db, &session.owner, turn_id).await {
        Ok(Some(turn)) => turn,
        Ok(None) => return false,
        Err(err) => {
            debug!("pr fact detector could not read the turn checkpoint: {err}");
            return false;
        }
    };
    if !turn
        .diffstat
        .as_ref()
        .is_some_and(|diffstat| diffstat.files > 0)
    {
        return false;
    }

    let Some(workspace_id) = session.workspace_id else {
        return false;
    };
    let workspace = match get_workspace(db, &session.owner, workspace_id).await {
        Ok(Some(workspace)) if !workspace.is_remote() => workspace,
        Ok(_) => return false,
        Err(err) => {
            debug!("pr fact detector could not read the changed workspace: {err}");
            return false;
        }
    };
    let checkout = Path::new(&workspace.worktree_path);
    match git_read(checkout, &["status", "--porcelain"]).await {
        Ok(status) if status.is_empty() => {}
        Ok(_) => return false,
        Err(err) => {
            debug!("pr fact detector could not inspect the changed checkout: {err}");
            return false;
        }
    }

    let Some(branch) = current_branch(checkout).await else {
        return false;
    };
    if branch != workspace.branch_name {
        return false;
    }
    let head = match git_read(checkout, &["rev-parse", "HEAD"]).await {
        Ok(head) if !head.is_empty() => head,
        Ok(_) => return false,
        Err(err) => {
            debug!("pr fact detector could not read the changed checkout head: {err}");
            return false;
        }
    };
    let target = match repository_target_from_path(checkout).await {
        Ok(target) => target,
        Err(err) => {
            debug!("pr fact detector could not resolve the changed checkout: {err}");
            return false;
        }
    };
    let values = match list_pull_requests_for_head_raw(
        &target.host,
        &target.owner,
        &target.name,
        &branch,
        gh_search_path,
    )
    .await
    {
        Ok(values) => values,
        Err(err) => {
            debug!("pr fact detector could not confirm the changed checkout: {err}");
            return false;
        }
    };
    let matching = values
        .into_iter()
        .filter(|value| {
            value
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| state.eq_ignore_ascii_case("open"))
                && value
                    .get("headRefName")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate == branch)
                && value
                    .get("headRefOid")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate == head)
        })
        .collect();
    let Some(value) = newest_by_created(matching) else {
        return false;
    };
    record_confirmed_fact(
        db,
        &session.owner,
        workspace_id,
        Some(session.id),
        None,
        &target,
        &value,
        CodePullRequestRelation::Contributed,
        CodePullRequestDiscovery::Command,
    )
    .await
    .is_some()
}

/// Confirm one `gh pr create` against the host and mint an authored fact.
///
/// Reports whether a fact landed, so the caller can mark the workspace hot.
async fn confirm_create(
    db: &DbStore,
    session: &CodeSession,
    command: &RecordedCommand,
    create: &CreateAct,
    preview: Option<&str>,
    gh_search_path: Option<&str>,
) -> bool {
    let Some(workspace_id) = session.workspace_id else {
        return false;
    };
    let sniffed = preview.and_then(sniff_pull_request_url);
    let target = match resolve_create_target(create, sniffed.as_ref(), &command.cwd).await {
        Some(target) => target,
        None => {
            debug!("pr fact detector could not resolve a repository for a create");
            return false;
        }
    };
    let value = if let Some((_, number)) = &sniffed {
        match view_pull_request_raw(
            &target.host,
            &target.owner,
            &target.name,
            *number,
            gh_search_path,
        )
        .await
        {
            Ok(value) => Some(value),
            Err(err) => {
                debug!("pr fact detector could not confirm a created pull request: {err}");
                None
            }
        }
    } else {
        let head = match &create.head_flag {
            Some(head) => Some(head.clone()),
            None => current_branch(Path::new(&command.cwd)).await,
        };
        let Some(head) = head else {
            debug!("pr fact detector could not resolve the created head branch");
            return false;
        };
        match list_pull_requests_for_head_raw(
            &target.host,
            &target.owner,
            &target.name,
            &head,
            gh_search_path,
        )
        .await
        {
            Ok(values) => newest_by_created(values),
            Err(err) => {
                debug!("pr fact detector could not list pull requests for a head: {err}");
                None
            }
        }
    };
    let Some(value) = value else { return false };
    record_confirmed_fact(
        db,
        &session.owner,
        workspace_id,
        Some(session.id),
        command.parent_call_id.clone(),
        &target,
        &value,
        CodePullRequestRelation::Authored,
        CodePullRequestDiscovery::Command,
    )
    .await
    .is_some()
}

/// Confirm one `git push` against the host and mint a contributed fact when
/// the pushed branch is a pull request's head.
///
/// Reports whether a fact landed, so the caller can mark the workspace hot.
async fn confirm_push(
    db: &DbStore,
    session: &CodeSession,
    command: &RecordedCommand,
    push: &PushAct,
    gh_search_path: Option<&str>,
) -> bool {
    let Some(workspace_id) = session.workspace_id else {
        return false;
    };
    let cwd = push
        .cwd_override
        .clone()
        .unwrap_or_else(|| command.cwd.clone());
    let cwd = Path::new(&cwd);
    let target = match resolve_push_target(push, cwd).await {
        Some(target) => target,
        None => {
            debug!("pr fact detector could not resolve a repository for a push");
            return false;
        }
    };
    let branch = match &push.branch {
        Some(branch) => Some(branch.clone()),
        None => current_branch(cwd).await,
    };
    let Some(branch) = branch else {
        debug!("pr fact detector could not resolve the pushed branch");
        return false;
    };
    let values = match list_pull_requests_for_head_raw(
        &target.host,
        &target.owner,
        &target.name,
        &branch,
        gh_search_path,
    )
    .await
    {
        Ok(values) => values,
        Err(err) => {
            debug!("pr fact detector could not list pull requests for a push: {err}");
            return false;
        }
    };
    let matching: Vec<Value> = values
        .into_iter()
        .filter(|value| {
            value
                .get("headRefName")
                .and_then(Value::as_str)
                .is_some_and(|head| head == branch)
        })
        .collect();
    let open = matching.iter().find(|value| {
        value
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("open"))
    });
    let value = match open {
        Some(value) => Some(value.clone()),
        None => newest_by_created(matching),
    };
    let Some(value) = value else { return false };
    record_confirmed_fact(
        db,
        &session.owner,
        workspace_id,
        Some(session.id),
        command.parent_call_id.clone(),
        &target,
        &value,
        CodePullRequestRelation::Contributed,
        CodePullRequestDiscovery::Command,
    )
    .await
    .is_some()
}

/// Write one confirmed observation: upsert the fact, claim the attribution,
/// and upgrade an existing contributed row when the relation is authored.
///
/// Shared with the user-initiated create and push paths, which confirm the
/// same way (decision 77).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_confirmed_fact(
    db: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
    session_id: Option<CodeSessionId>,
    parent_call_id: Option<String>,
    target: &CodeGitHubRepositoryTarget,
    value: &Value,
    relation: CodePullRequestRelation,
    discovered_via: CodePullRequestDiscovery,
) -> Option<CodePullRequestId> {
    let now = Utc::now();
    let fact = fact_from_gh_value(owner, target, value, now)?;
    let id = match save_pull_request_fact(db, &fact).await {
        Ok(id) => id,
        Err(err) => {
            debug!("pr fact upsert failed: {err}");
            return None;
        }
    };
    let claimed = insert_pull_request_attribution(
        db,
        &CodePullRequestAttribution {
            owner: owner.clone(),
            pull_request_id: id,
            workspace_id,
            relation,
            discovered_via,
            session_id,
            parent_call_id,
            created_at: now,
        },
    )
    .await;
    match claimed {
        Ok(_) => {
            if relation == CodePullRequestRelation::Authored {
                if let Err(err) = promote_attribution_to_authored(db, owner, id, workspace_id).await
                {
                    debug!("pr fact attribution promotion failed: {err}");
                }
            }
        }
        Err(err) => debug!("pr fact attribution claim failed: {err}"),
    }
    Some(id)
}

/// Resolve where a create landed: the `--repo` flag, then the preview URL,
/// then the command's working directory's origin remote.
async fn resolve_create_target(
    create: &CreateAct,
    sniffed: Option<&(CodeGitHubRepositoryTarget, u64)>,
    cwd: &str,
) -> Option<CodeGitHubRepositoryTarget> {
    if let Some(flag) = &create.repo_flag {
        return parse_repository_input(flag).ok();
    }
    if let Some((target, _)) = sniffed {
        return Some(target.clone());
    }
    repository_target_from_path(Path::new(cwd)).await.ok()
}

/// Resolve where a push landed: a URL operand directly, a named remote via
/// the checkout, or the checkout's origin.
async fn resolve_push_target(push: &PushAct, cwd: &Path) -> Option<CodeGitHubRepositoryTarget> {
    match &push.remote {
        Some(remote) if remote.contains("://") || remote.starts_with("git@") => {
            parse_repository_input(remote).ok()
        }
        Some(remote) => {
            let url = git_read(cwd, &["remote", "get-url", remote]).await.ok()?;
            parse_repository_input(&url).ok()
        }
        None => repository_target_from_path(cwd).await.ok(),
    }
}

/// The checkout's current branch, or `None` when detached or unreadable.
async fn current_branch(cwd: &Path) -> Option<String> {
    let branch = git_read(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()?;
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }
    Some(branch)
}

/// Recognize `gh pr create`, tolerating flags anywhere after the subcommand.
fn parse_create(argv: &[String]) -> Option<CreateAct> {
    if argv.len() < 3 || argv[0] != "gh" || argv[1] != "pr" || argv[2] != "create" {
        return None;
    }
    let mut repo_flag = None;
    let mut head_flag = None;
    let mut index = 3;
    while index < argv.len() {
        let word = argv[index].as_str();
        match word {
            "--repo" | "-R" => {
                repo_flag = argv.get(index + 1).cloned();
                index += 2;
            }
            "--head" | "-H" => {
                head_flag = argv.get(index + 1).cloned();
                index += 2;
            }
            _ => {
                if let Some(value) = word.strip_prefix("--repo=") {
                    repo_flag = Some(value.to_owned());
                } else if let Some(value) = word.strip_prefix("--head=") {
                    head_flag = Some(value.to_owned());
                }
                index += 1;
            }
        }
    }
    Some(CreateAct {
        repo_flag,
        head_flag,
    })
}

/// Recognize `git push`, tolerating `-C`/`-c` before the subcommand.
///
/// Options after `push` are skipped without consuming values, so an
/// option-valued form like `-o ci.skip` can shift the operand read; the
/// confirming host read is what keeps a misread from minting anything.
fn parse_push(argv: &[String]) -> Option<PushAct> {
    if argv.first().map(String::as_str) != Some("git") {
        return None;
    }
    let mut cwd_override = None;
    let mut index = 1;
    while index < argv.len() {
        let word = argv[index].as_str();
        if word == "-C" {
            cwd_override = argv.get(index + 1).cloned();
            index += 2;
        } else if word == "-c" {
            index += 2;
        } else if word.starts_with('-') {
            index += 1;
        } else {
            break;
        }
    }
    if argv.get(index).map(String::as_str) != Some("push") {
        return None;
    }
    index += 1;
    let mut operands: Vec<&str> = Vec::new();
    while index < argv.len() {
        let word = argv[index].as_str();
        if word == "-o" || word == "--push-option" {
            index += 2;
            continue;
        }
        if word.starts_with('-') {
            index += 1;
            continue;
        }
        operands.push(word);
        index += 1;
    }
    let remote = operands.first().map(|word| (*word).to_owned());
    let branch = operands.get(1..).and_then(|refspecs| {
        refspecs.last().map(|refspec| {
            let refspec = refspec.trim_start_matches('+');
            let dest = refspec.split_once(':').map_or(refspec, |(_, dest)| dest);
            dest.trim_start_matches("refs/heads/").to_owned()
        })
    });
    let branch = branch.filter(|branch| !branch.is_empty());
    Some(PushAct {
        cwd_override,
        remote,
        branch,
    })
}

/// Find a pull-request URL in tool output: `https://<host>/<owner>/<repo>/pull/<n>`.
fn sniff_pull_request_url(text: &str) -> Option<(CodeGitHubRepositoryTarget, u64)> {
    for start in text.match_indices("https://").map(|(index, _)| index) {
        let candidate = &text[start + "https://".len()..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '>')
            .unwrap_or(candidate.len());
        let candidate = candidate[..end].trim_end_matches(['.', ',', ';']);
        let mut parts = candidate.split('/');
        let (Some(host), Some(owner), Some(repo), Some(marker), Some(number)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        if marker != "pull" || host.is_empty() || owner.is_empty() || repo.is_empty() {
            continue;
        }
        let Ok(number) = number.parse::<u64>() else {
            continue;
        };
        if number == 0 {
            continue;
        }
        return Some((
            CodeGitHubRepositoryTarget {
                host: host.to_owned(),
                owner: owner.to_owned(),
                name: repo.to_owned(),
            },
            number,
        ));
    }
    None
}

/// Newest entry by `createdAt`, for a head-branch list.
fn newest_by_created(values: Vec<Value>) -> Option<Value> {
    values.into_iter().max_by_key(|value| {
        value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|parsed| parsed.with_timezone(&Utc))
            .unwrap_or_else(|| DateTime::<Utc>::MIN_UTC)
    })
}

/// Build a fact from one `gh` pull-request JSON object.
///
/// `None` when the object lacks an identity (number, url, or title): a
/// partial host response mints nothing.
fn fact_from_gh_value(
    owner: &OwnerId,
    target: &CodeGitHubRepositoryTarget,
    value: &Value,
    now: DateTime<Utc>,
) -> Option<CodePullRequestFact> {
    let number = value.get("number").and_then(Value::as_u64)?;
    if number == 0 {
        return None;
    }
    let url = value.get("url").and_then(Value::as_str)?.to_owned();
    let title = value.get("title").and_then(Value::as_str)?.to_owned();
    let merged_at = timestamp(value, "mergedAt");
    let state = match value
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("open") => CodePullRequestState::Open,
        Some("merged") => CodePullRequestState::Merged,
        Some("closed") if merged_at.is_some() => CodePullRequestState::Merged,
        Some("closed") => CodePullRequestState::Closed,
        _ => return None,
    };
    Some(CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: target.host.clone(),
        repo_owner: target.owner.clone(),
        repo_name: target.name.clone(),
        number,
        url,
        title,
        state,
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        head_branch: value
            .get("headRefName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        base_branch: value
            .get("baseRefName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        head_sha: value
            .get("headRefOid")
            .and_then(Value::as_str)
            .map(str::to_owned),
        created_at: timestamp(value, "createdAt").unwrap_or(now),
        updated_at: timestamp(value, "updatedAt").unwrap_or(now),
        merged_at,
        closed_at: timestamp(value, "closedAt"),
        first_seen_at: now,
        last_seen_at: now,
        live: None,
    })
}

/// Parse `(host, repo_owner, repo_name, number)` out of a pull request's own
/// web URL. The URL is the one repository-qualified field a digest carries,
/// which makes it the join key between a digest read and the decision-62
/// fact row (decision 66). `None` for anything that is not the plain
/// `https://host/owner/name/pull/N` shape GitHub and GHES use — another
/// forge simply does not join.
pub(crate) fn pull_request_identity_from_url(url: &str) -> Option<(String, String, String, u64)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let name = parts.next()?;
    let marker = parts.next()?;
    let number: u64 = parts.next()?.parse().ok()?;
    if host.is_empty() || owner.is_empty() || name.is_empty() || marker != "pull" || number == 0 {
        return None;
    }
    Some((host.to_owned(), owner.to_owned(), name.to_owned(), number))
}

/// Project one fact row into the digest vocabulary every consumer already
/// reads. The snapshot fields fill the identity; the live tier (decision 66)
/// fills checks, review, and mergeability when a read has written it.
pub(crate) fn digest_from_fact(fact: &CodePullRequestFact) -> tidebreak_core::PullRequestDigest {
    let live = fact.live.as_ref();
    tidebreak_core::PullRequestDigest {
        number: fact.number,
        url: Some(fact.url.clone()),
        state: fact.state.as_str().to_owned(),
        title: Some(fact.title.clone()),
        checks_summary: live.and_then(|live| live.checks_summary.clone()),
        // The live tier stores the summary and the check list, not the
        // counts; rows written with a check list re-derive them, and rows
        // old enough to carry only the summary stay uncounted.
        check_counts: live
            .and_then(|live| live.checks.as_deref())
            .map(tidebreak_core::PullRequestCheckCounts::from_checks),
        checks: live.and_then(|live| live.checks.clone()),
        draft: Some(fact.draft),
        merged: Some(fact.state == CodePullRequestState::Merged),
        review_decision: live.and_then(|live| live.review_decision.clone()),
        mergeable: live.and_then(|live| live.mergeable.clone()),
        merge_state_status: live.and_then(|live| live.merge_state_status.clone()),
        head_branch: Some(fact.head_branch.clone()),
        base_branch: Some(fact.base_branch.clone()),
        head_sha: fact.head_sha.clone(),
        auto_merge_enabled: live.and_then(|live| live.auto_merge_enabled),
        in_merge_queue: live.and_then(|live| live.in_merge_queue),
    }
}

fn timestamp(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn create_matches_repo_and_head_flags() {
        let act = parse_create(&argv(&[
            "gh",
            "pr",
            "create",
            "--repo",
            "acme/tools",
            "--title",
            "x",
        ]))
        .unwrap();
        assert_eq!(act.repo_flag.as_deref(), Some("acme/tools"));
        assert_eq!(act.head_flag, None);

        let act = parse_create(&argv(&[
            "gh",
            "pr",
            "create",
            "--repo=acme/tools",
            "-H",
            "feat",
        ]))
        .unwrap();
        assert_eq!(act.repo_flag.as_deref(), Some("acme/tools"));
        assert_eq!(act.head_flag.as_deref(), Some("feat"));

        assert!(parse_create(&argv(&["gh", "pr", "view", "12"])).is_none());
        assert!(parse_create(&argv(&["gh", "pr", "comment", "12", "--body", "x"])).is_none());
    }

    #[test]
    fn push_extracts_remote_and_branch() {
        let act = parse_push(&argv(&["git", "push", "-u", "origin", "feat/x"])).unwrap();
        assert_eq!(act.remote.as_deref(), Some("origin"));
        assert_eq!(act.branch.as_deref(), Some("feat/x"));

        let act = parse_push(&argv(&["git", "push"])).unwrap();
        assert_eq!(act.remote, None);
        assert_eq!(act.branch, None);

        let act = parse_push(&argv(&["git", "push", "fork", "local:refs/heads/remote"])).unwrap();
        assert_eq!(act.remote.as_deref(), Some("fork"));
        assert_eq!(act.branch.as_deref(), Some("remote"));

        let act = parse_push(&argv(&[
            "git",
            "-C",
            "/tmp/clone",
            "push",
            "origin",
            "+topic",
        ]))
        .unwrap();
        assert_eq!(act.cwd_override.as_deref(), Some("/tmp/clone"));
        assert_eq!(act.branch.as_deref(), Some("topic"));

        assert!(parse_push(&argv(&["git", "pull"])).is_none());
        assert!(parse_push(&argv(&["cargo", "push"])).is_none());
    }

    #[test]
    fn compound_commands_surface_both_acts() {
        let argvs = simple_command_argvs("git push -u origin feat && gh pr create --fill").unwrap();
        assert_eq!(argvs.len(), 2);
        assert!(parse_push(&argvs[0]).is_some());
        assert!(parse_create(&argvs[1]).is_some());
    }

    #[test]
    fn substituted_repo_flags_never_resolve() {
        // The extractor surfaces a command substitution — the inner command
        // becomes its own argv — rather than refusing the line. Whatever it
        // yields, a recognized create whose flag carries substitution text
        // must fail repository resolution, so the candidate mints nothing.
        let Some(argvs) = simple_command_argvs("gh pr create --repo $(cat target)") else {
            return;
        };
        for argv in &argvs {
            if let Some(create) = parse_create(argv) {
                if let Some(flag) = create.repo_flag {
                    assert!(
                        parse_repository_input(&flag).is_err(),
                        "{flag:?} resolved to a repository"
                    );
                }
            }
        }
    }

    #[test]
    fn url_sniff_reads_gh_create_output() {
        let (target, number) = sniff_pull_request_url(
            "Creating pull request\nhttps://github.com/acme/tools/pull/412\n",
        )
        .unwrap();
        assert_eq!(target.host, "github.com");
        assert_eq!(target.owner, "acme");
        assert_eq!(target.name, "tools");
        assert_eq!(number, 412);

        assert!(sniff_pull_request_url("https://github.com/acme/tools/issues/9").is_none());
        assert!(sniff_pull_request_url("no url here").is_none());
        let (target, number) =
            sniff_pull_request_url("see https://ghe.corp.example/org/app/pull/7.").unwrap();
        assert_eq!(target.host, "ghe.corp.example");
        assert_eq!(number, 7);
    }

    #[test]
    fn fact_normalizes_merged_state() {
        let owner = OwnerId::local();
        let target = CodeGitHubRepositoryTarget {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "tools".into(),
        };
        let now = Utc::now();
        let value = serde_json::json!({
            "number": 12,
            "url": "https://github.com/acme/tools/pull/12",
            "title": "Fix",
            "state": "CLOSED",
            "isDraft": false,
            "author": {"login": "octocat"},
            "headRefName": "feat",
            "baseRefName": "main",
            "headRefOid": "abc123",
            "createdAt": "2026-08-22T10:00:00Z",
            "updatedAt": "2026-08-22T11:00:00Z",
            "mergedAt": "2026-08-22T11:00:00Z",
            "closedAt": "2026-08-22T11:00:00Z",
        });
        let fact = fact_from_gh_value(&owner, &target, &value, now).unwrap();
        assert_eq!(fact.state, CodePullRequestState::Merged);
        assert_eq!(fact.number, 12);
        assert_eq!(fact.author.as_deref(), Some("octocat"));
        assert_eq!(fact.head_branch, "feat");

        let partial = serde_json::json!({"number": 12, "state": "OPEN"});
        assert!(fact_from_gh_value(&owner, &target, &partial, now).is_none());
    }

    #[test]
    fn identity_parses_only_plain_pull_urls() {
        assert_eq!(
            pull_request_identity_from_url("https://github.com/acme/tools/pull/412"),
            Some((
                "github.com".to_owned(),
                "acme".to_owned(),
                "tools".to_owned(),
                412
            ))
        );
        assert_eq!(
            pull_request_identity_from_url("https://ghe.corp.example/acme/tools/pull/7/files"),
            Some((
                "ghe.corp.example".to_owned(),
                "acme".to_owned(),
                "tools".to_owned(),
                7
            ))
        );
        assert!(
            pull_request_identity_from_url("https://github.com/acme/tools/issues/412").is_none()
        );
        assert!(pull_request_identity_from_url("https://github.com/acme/tools/pull/0").is_none());
        assert!(pull_request_identity_from_url("not a url").is_none());
    }

    #[test]
    fn fact_digests_carry_the_live_tier_when_present() {
        let owner = OwnerId::local();
        let now = Utc::now();
        let mut fact = CodePullRequestFact {
            id: CodePullRequestId::new(),
            owner,
            host: "github.com".into(),
            repo_owner: "acme".into(),
            repo_name: "tools".into(),
            number: 412,
            url: "https://github.com/acme/tools/pull/412".into(),
            title: "First".into(),
            state: CodePullRequestState::Open,
            draft: false,
            author: None,
            head_branch: "feat/x".into(),
            base_branch: "main".into(),
            head_sha: Some("aaa111".into()),
            created_at: now,
            updated_at: now,
            merged_at: None,
            closed_at: None,
            first_seen_at: now,
            last_seen_at: now,
            live: None,
        };
        let bare = digest_from_fact(&fact);
        assert_eq!(bare.number, 412);
        assert!(bare.checks.is_none());
        assert!(bare.merge_state_status.is_none());

        fact.live = Some(tidebreak_core::CodePullRequestLiveState {
            checks_summary: Some("1 pending".into()),
            checks: Some(vec![tidebreak_core::PullRequestCheck {
                name: "ci".into(),
                bucket: tidebreak_core::PullRequestCheckBucket::Pending,
                detail: None,
                url: None,
            }]),
            review_decision: Some("review_required".into()),
            mergeable: Some("mergeable".into()),
            merge_state_status: Some("blocked".into()),
            auto_merge_enabled: Some(true),
            in_merge_queue: Some(false),
            observed_at: now,
        });
        let enriched = digest_from_fact(&fact);
        assert_eq!(enriched.merge_state_status.as_deref(), Some("blocked"));
        assert_eq!(enriched.review_decision.as_deref(), Some("review_required"));
        assert_eq!(enriched.checks.as_ref().unwrap().len(), 1);
        assert_eq!(enriched.auto_merge_enabled, Some(true));
    }
}
