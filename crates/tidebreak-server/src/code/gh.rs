//! Git commit/push and `gh` pull-request operations for a workspace.
//!
//! Every subprocess is bounded and non-interactive. Arguments are an argv
//! array, never a shell string. `gh` credentials are observed, never stored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::time::timeout;

use tidebreak_core::{Diffstat, PullRequestDigest, QuickAction, WorkspaceId};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(120);
const GH_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_CHARS: usize = 4_096;
const PR_CACHE_TTL: Duration = Duration::from_secs(20);

/// Brief in-memory cache of a workspace PR digest.
#[derive(Debug, Default)]
pub(crate) struct PrDigestCache {
    entries: std::sync::Mutex<HashMap<WorkspaceId, CachedPr>>,
}

#[derive(Debug, Clone)]
struct CachedPr {
    fetched_at: Instant,
    digest: PullRequestDigest,
}

impl PrDigestCache {
    pub(crate) fn get(&self, id: WorkspaceId) -> Option<PullRequestDigest> {
        let guard = self.entries.lock().expect("pr cache");
        let entry = guard.get(&id)?;
        if entry.fetched_at.elapsed() > PR_CACHE_TTL {
            return None;
        }
        Some(entry.digest.clone())
    }

    pub(crate) fn put(&self, id: WorkspaceId, digest: PullRequestDigest) {
        self.entries.lock().expect("pr cache").insert(
            id,
            CachedPr {
                fetched_at: Instant::now(),
                digest,
            },
        );
    }

    pub(crate) fn invalidate(&self, id: WorkspaceId) {
        self.entries.lock().expect("pr cache").remove(&id);
    }
}

/// Failure from a git, `gh`, or quick-action operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GhError {
    #[error("nothing to commit")]
    NothingToCommit,
    #[error("{0}")]
    AuthFailed(String),
    #[error("{0}")]
    PushFailed(String),
    #[error("{instructions}")]
    GhAbsent { instructions: String },
    #[error("{instructions}")]
    GhSignedOut { instructions: String },
    #[error("{0}")]
    User(String),
    #[error("{0}")]
    Internal(String),
}

impl GhError {
    fn user(message: impl Into<String>) -> Self {
        Self::User(message.into())
    }
}

impl From<String> for GhError {
    fn from(message: String) -> Self {
        Self::Internal(message)
    }
}

/// Result of staging and committing the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitOutcome {
    pub sha: String,
    pub message: String,
    pub stat: Diffstat,
}

/// Result of pushing the workspace branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PushOutcome {
    pub branch: String,
    pub remote: String,
}

/// Live git + `gh` observation for the workspace PR card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceGitStatus {
    pub dirty: bool,
    pub unpushed: bool,
    pub ahead: u64,
    pub has_upstream: bool,
    pub suggested_commit_message: String,
    pub pr: Option<PullRequestDigest>,
    pub gh_found: bool,
    pub gh_authenticated: Option<bool>,
    pub remediation: String,
}

/// Outcome of one named quick action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionOutcome {
    pub name: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Observed `gh` availability. Tokens are never read or stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GhObservation {
    pub found: bool,
    pub authenticated: Option<bool>,
    pub binary: Option<PathBuf>,
    pub remediation: String,
}

/// Stage every change in `worktree` and create one commit.
pub(crate) async fn commit_all(
    worktree: &Path,
    title: &str,
    message: Option<&str>,
) -> Result<CommitOutcome, GhError> {
    if !has_uncommitted_work(worktree).await? {
        return Err(GhError::NothingToCommit);
    }
    git(worktree, &["add", "-A"], GIT_TIMEOUT).await?;
    let stat = cached_diffstat(worktree).await?;
    if stat.files == 0 && stat.insertions == 0 && stat.deletions == 0 {
        return Err(GhError::NothingToCommit);
    }
    let message = match message.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.to_owned(),
        None => generate_commit_message(title, &stat),
    };
    git(worktree, &["commit", "-m", &message], GIT_TIMEOUT)
        .await
        .map_err(|err| classify_git(err, "commit"))?;
    let sha = git(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT).await?;
    Ok(CommitOutcome { sha, message, stat })
}

/// Push the workspace branch to `origin` and set upstream.
pub(crate) async fn push_branch(worktree: &Path, branch: &str) -> Result<PushOutcome, GhError> {
    git(
        worktree,
        &["push", "-u", "origin", branch],
        GIT_PUSH_TIMEOUT,
    )
    .await
    .map_err(|err| classify_git(err, "push"))?;
    Ok(PushOutcome {
        branch: branch.to_owned(),
        remote: "origin".into(),
    })
}

/// Inspect the worktree and, when `gh` can, refresh the PR digest.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn workspace_git_status(
    worktree: &Path,
    workspace_id: WorkspaceId,
    title: &str,
    branch: &str,
    base_ref: &str,
    persisted: Option<PullRequestDigest>,
    cache: &PrDigestCache,
    gh_search_path: Option<&str>,
) -> Result<WorkspaceGitStatus, GhError> {
    let inspect = inspect_git(worktree, base_ref, title).await?;
    let gh = observe_gh(gh_search_path).await;
    let mut pr = persisted;
    if gh.found && gh.authenticated == Some(true) {
        if let Some(cached) = cache.get(workspace_id) {
            pr = Some(cached);
        } else if let Some(fresh) = load_pr_digest(worktree, &gh, gh_search_path).await? {
            cache.put(workspace_id, fresh.clone());
            pr = Some(fresh);
        }
    }
    Ok(WorkspaceGitStatus {
        dirty: inspect.dirty,
        unpushed: inspect.unpushed,
        ahead: inspect.ahead,
        has_upstream: inspect.has_upstream,
        suggested_commit_message: inspect.suggested_commit_message,
        pr,
        gh_found: gh.found,
        gh_authenticated: gh.authenticated,
        remediation: if gh.found && gh.authenticated == Some(true) {
            String::new()
        } else {
            manual_pr_instructions(worktree, branch, title, None, &inspect.diffstat, &gh)
        },
    })
}

/// Create a pull request from the workspace branch. Never merges.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_pull_request(
    worktree: &Path,
    workspace_id: WorkspaceId,
    title: &str,
    branch: &str,
    base_ref: &str,
    requested_title: Option<&str>,
    requested_body: Option<&str>,
    cache: &PrDigestCache,
    gh_search_path: Option<&str>,
) -> Result<PullRequestDigest, GhError> {
    let inspect = inspect_git(worktree, base_ref, title).await?;
    let gh = observe_gh(gh_search_path).await;
    require_gh(&gh, worktree, branch, title, None, &inspect.diffstat)?;
    let pr_title = requested_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| generate_pr_title(title, branch));
    let pr_body = requested_body
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(inspect.suggested_pr_body);
    let binary = gh.binary.clone().ok_or_else(|| GhError::GhAbsent {
        instructions: manual_pr_instructions(
            worktree,
            branch,
            title,
            Some(&pr_body),
            &inspect.diffstat,
            &gh,
        ),
    })?;
    let base = gh_base_branch(base_ref);
    let stdout = run_gh(
        worktree,
        &binary,
        &[
            "pr", "create", "--title", &pr_title, "--body", &pr_body, "--base", base, "--head",
            branch,
        ],
        GH_TIMEOUT,
    )
    .await
    .map_err(|err| {
        classify_gh(
            err,
            worktree,
            branch,
            title,
            Some(&pr_body),
            &inspect.diffstat,
        )
    })?;
    cache.invalidate(workspace_id);
    if let Some(digest) = load_pr_digest(worktree, &gh, gh_search_path).await? {
        cache.put(workspace_id, digest.clone());
        return Ok(digest);
    }
    let url = stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(ToOwned::to_owned);
    let number = url.as_deref().and_then(pr_number_from_url).unwrap_or(0);
    let digest = PullRequestDigest {
        number,
        url,
        state: "open".into(),
        checks_summary: None,
    };
    cache.put(workspace_id, digest.clone());
    Ok(digest)
}

/// Run one named quick action in the worktree. Output is not journaled.
pub(crate) async fn run_named_action(
    worktree: &Path,
    actions: &[QuickAction],
    name: &str,
) -> Result<ActionOutcome, GhError> {
    let action = actions
        .iter()
        .find(|action| action.name == name)
        .ok_or_else(|| GhError::user(format!("no quick action named {name}")))?;
    Ok(run_action(worktree, action).await)
}

/// Run every `auto_run_on_create` action after setup. Failures are ignored.
pub(crate) async fn run_auto_create_actions(worktree: &Path, actions: &[QuickAction]) {
    for action in actions.iter().filter(|action| action.auto_run_on_create) {
        let outcome = run_action(worktree, action).await;
        if !outcome.success {
            tracing::warn!(
                action = %action.name,
                timed_out = outcome.timed_out,
                "code-mode: auto-run quick action did not succeed"
            );
        }
    }
}

/// Deterministic commit subject + shortstat body. No model calls.
pub(crate) fn generate_commit_message(title: &str, stat: &Diffstat) -> String {
    let subject = title.trim();
    let subject = if subject.is_empty() {
        "Update workspace"
    } else {
        subject
    };
    format!("{subject}\n\n{}", format_shortstat(stat))
}

pub(crate) fn generate_pr_title(title: &str, branch: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        branch.rsplit('/').next().unwrap_or(branch).to_owned()
    } else {
        title.to_owned()
    }
}

pub(crate) fn generate_pr_body(commits: &[String], stat: &Diffstat) -> String {
    let mut body = String::from("## Commits\n\n");
    if commits.is_empty() {
        body.push_str("No commits on this branch yet.\n");
    } else {
        for commit in commits {
            body.push_str("- ");
            body.push_str(commit);
            body.push('\n');
        }
    }
    body.push_str("\n## Diff\n\n");
    body.push_str(&format_shortstat(stat));
    body.push('\n');
    body
}

/// Branch name `gh pr create --base` expects: strip a remote prefix.
pub(crate) fn gh_base_branch(base_ref: &str) -> &str {
    let trimmed = base_ref.trim();
    trimmed
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| trimmed.strip_prefix("origin/"))
        .unwrap_or(trimmed)
}

pub(crate) fn format_shortstat(stat: &Diffstat) -> String {
    format!(
        "{} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        stat.files,
        if stat.files == 1 { "" } else { "s" },
        stat.insertions,
        if stat.insertions == 1 { "" } else { "s" },
        stat.deletions,
        if stat.deletions == 1 { "" } else { "s" },
    )
}

struct GitInspect {
    dirty: bool,
    unpushed: bool,
    ahead: u64,
    has_upstream: bool,
    suggested_commit_message: String,
    suggested_pr_body: String,
    diffstat: Diffstat,
}

async fn inspect_git(worktree: &Path, base_ref: &str, title: &str) -> Result<GitInspect, GhError> {
    let dirty = has_uncommitted_work(worktree).await?;
    let (has_upstream, ahead_of_upstream) = match git(
        worktree,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(_) => {
            let count = git(
                worktree,
                &["rev-list", "--count", "@{u}..HEAD"],
                GIT_TIMEOUT,
            )
            .await?;
            (true, parse_count(&count))
        }
        Err(_) => (false, 0),
    };
    let range = format!("{base_ref}..HEAD");
    let ahead = parse_count(&git(worktree, &["rev-list", "--count", &range], GIT_TIMEOUT).await?);
    let unpushed = if has_upstream {
        ahead_of_upstream > 0
    } else {
        ahead > 0
    };
    let working_stat = if dirty {
        working_tree_diffstat(worktree).await?
    } else {
        Diffstat {
            files: 0,
            insertions: 0,
            deletions: 0,
            truncated: false,
        }
    };
    let branch_stat = parse_shortstat(
        &git(worktree, &["diff", "--shortstat", &range], GIT_TIMEOUT)
            .await
            .unwrap_or_default(),
    );
    let commits = commit_subjects(worktree, &range).await?;
    Ok(GitInspect {
        dirty,
        unpushed,
        ahead,
        has_upstream,
        suggested_commit_message: generate_commit_message(title, &working_stat),
        suggested_pr_body: generate_pr_body(&commits, &branch_stat),
        diffstat: branch_stat,
    })
}

async fn has_uncommitted_work(worktree: &Path) -> Result<bool, GhError> {
    let status = git(worktree, &["status", "--porcelain"], GIT_TIMEOUT).await?;
    Ok(!status.is_empty())
}

async fn cached_diffstat(worktree: &Path) -> Result<Diffstat, GhError> {
    let text = git(worktree, &["diff", "--cached", "--shortstat"], GIT_TIMEOUT).await?;
    Ok(parse_shortstat(&text))
}

async fn working_tree_diffstat(worktree: &Path) -> Result<Diffstat, GhError> {
    let tracked = parse_shortstat(
        &git(worktree, &["diff", "--shortstat", "HEAD"], GIT_TIMEOUT)
            .await
            .unwrap_or_default(),
    );
    let status = git(worktree, &["status", "--porcelain"], GIT_TIMEOUT).await?;
    let mut untracked_files = 0_u32;
    let mut untracked_insertions = 0_u32;
    for line in status.lines() {
        if let Some(path) = line.strip_prefix("?? ") {
            untracked_files += 1;
            if let Ok(bytes) = tokio::fs::read(worktree.join(path.trim())).await {
                untracked_insertions +=
                    u32::try_from(String::from_utf8_lossy(&bytes).lines().count())
                        .unwrap_or(u32::MAX);
            }
        }
    }
    Ok(Diffstat {
        files: tracked.files.saturating_add(untracked_files),
        insertions: tracked.insertions.saturating_add(untracked_insertions),
        deletions: tracked.deletions,
        truncated: tracked.truncated,
    })
}

async fn commit_subjects(worktree: &Path, range: &str) -> Result<Vec<String>, GhError> {
    let text = git(worktree, &["log", "--format=%h %s", range], GIT_TIMEOUT)
        .await
        .unwrap_or_default();
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub(crate) fn parse_shortstat(text: &str) -> Diffstat {
    let mut files = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    for part in text.split(',') {
        let part = part.trim();
        let mut words = part.split_whitespace();
        let Some(n) = words.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let rest = words.collect::<Vec<_>>().join(" ");
        if rest.starts_with("file") {
            files = n;
        } else if rest.contains("insertion") {
            insertions = n;
        } else if rest.contains("deletion") {
            deletions = n;
        }
    }
    Diffstat {
        files,
        insertions,
        deletions,
        truncated: false,
    }
}

fn parse_count(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}

async fn observe_gh(search_path: Option<&str>) -> GhObservation {
    let Some(binary) = find_gh(search_path) else {
        return GhObservation {
            found: false,
            authenticated: None,
            binary: None,
            remediation: "gh is not installed. Install the GitHub CLI from https://cli.github.com/ and sign in with `gh auth login` in a terminal. Tidebreak does not store GitHub credentials.".into(),
        };
    };
    match run_gh(Path::new("."), &binary, &["auth", "status"], GH_TIMEOUT).await {
        Ok(_) => GhObservation {
            found: true,
            authenticated: Some(true),
            binary: Some(binary),
            remediation: String::new(),
        },
        Err(_) => GhObservation {
            found: true,
            authenticated: Some(false),
            binary: Some(binary),
            remediation: "gh is installed but not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into(),
        },
    }
}

fn require_gh(
    gh: &GhObservation,
    worktree: &Path,
    branch: &str,
    title: &str,
    body: Option<&str>,
    stat: &Diffstat,
) -> Result<(), GhError> {
    if !gh.found {
        return Err(GhError::GhAbsent {
            instructions: manual_pr_instructions(worktree, branch, title, body, stat, gh),
        });
    }
    if gh.authenticated != Some(true) {
        return Err(GhError::GhSignedOut {
            instructions: manual_pr_instructions(worktree, branch, title, body, stat, gh),
        });
    }
    Ok(())
}

fn manual_pr_instructions(
    worktree: &Path,
    branch: &str,
    title: &str,
    body: Option<&str>,
    stat: &Diffstat,
    gh: &GhObservation,
) -> String {
    let pr_title = generate_pr_title(title, branch);
    let pr_body = body
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| generate_pr_body(&[], stat));
    format!(
        "{header}\n\nCreate the pull request from a terminal:\n\n  cd {worktree}\n  git push -u origin {branch}\n  gh pr create --title {title} --body {body}\n",
        header = gh.remediation,
        worktree = worktree.display(),
        title = shell_single_quote(&pr_title),
        body = shell_single_quote(&pr_body),
    )
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn load_pr_digest(
    worktree: &Path,
    gh: &GhObservation,
    _search_path: Option<&str>,
) -> Result<Option<PullRequestDigest>, GhError> {
    let Some(binary) = gh.binary.as_ref() else {
        return Ok(None);
    };
    let view = run_gh(
        worktree,
        binary,
        &["pr", "view", "--json", "number,url,state"],
        GH_TIMEOUT,
    )
    .await;
    let Ok(json) = view else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    let number = parsed.get("number").and_then(|value| value.as_u64());
    let Some(number) = number else {
        return Ok(None);
    };
    let url = parsed
        .get("url")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let state = parsed
        .get("state")
        .and_then(|value| value.as_str())
        .unwrap_or("open")
        .to_ascii_lowercase();
    let checks = run_gh(worktree, binary, &["pr", "checks"], GH_TIMEOUT)
        .await
        .unwrap_or_default();
    Ok(Some(PullRequestDigest {
        number,
        url,
        state,
        checks_summary: summarize_checks(&checks),
    }))
}

pub(crate) fn summarize_checks(output: &str) -> Option<String> {
    if output.trim().is_empty() {
        return Some("no checks".into());
    }
    let mut passing = 0_u32;
    let mut failing = 0_u32;
    let mut pending = 0_u32;
    let mut other = 0_u32;
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains('\t') {
            // skip header-ish lines that name the columns
            if lower.starts_with("name\t") || lower.starts_with("check\t") {
                continue;
            }
        } else if lower.trim().is_empty() {
            continue;
        }
        if lower.contains("pass") || lower.contains("success") {
            passing += 1;
        } else if lower.contains("fail") || lower.contains("error") {
            failing += 1;
        } else if lower.contains("pend") || lower.contains("progress") || lower.contains("queued") {
            pending += 1;
        } else {
            other += 1;
        }
    }
    if passing + failing + pending + other == 0 {
        return Some("no checks".into());
    }
    Some(format!(
        "{passing} passing, {pending} pending, {failing} failing"
    ))
}

fn pr_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|value| value.parse().ok())
}

fn find_gh(search_path: Option<&str>) -> Option<PathBuf> {
    let path = search_path
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("gh");
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

async fn run_action(worktree: &Path, action: &QuickAction) -> ActionOutcome {
    let script = action.command.trim();
    if script.is_empty() {
        return ActionOutcome {
            name: action.name.clone(),
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut command = Command::new(shell);
    command
        .arg("-lc")
        .arg(script)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ActionOutcome {
                name: action.name.clone(),
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: bound_text(&format!("failed to spawn quick action: {err}")),
                timed_out: false,
            };
        }
    };
    match timeout(ACTION_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => ActionOutcome {
            name: action.name.clone(),
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: bound_text(&String::from_utf8_lossy(&output.stdout)),
            stderr: bound_text(&String::from_utf8_lossy(&output.stderr)),
            timed_out: false,
        },
        Ok(Err(err)) => ActionOutcome {
            name: action.name.clone(),
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: bound_text(&format!("quick action failed: {err}")),
            timed_out: false,
        },
        Err(_) => ActionOutcome {
            name: action.name.clone(),
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: "quick action timed out".into(),
            timed_out: true,
        },
    }
}

fn bound_text(text: &str) -> String {
    let mut owned = text.to_owned();
    if owned.chars().count() > MAX_OUTPUT_CHARS {
        owned = owned.chars().take(MAX_OUTPUT_CHARS).collect();
    }
    owned
}

fn classify_git(err: String, op: &str) -> GhError {
    let bounded = bound_text(&err);
    if is_git_auth_failure(&bounded) {
        return GhError::AuthFailed(format!(
            "git could not authenticate to the remote. Configure your own git credentials (a credential helper, SSH key, or token) in a terminal, then try again. Tidebreak does not store git credentials.\n\n{bounded}"
        ));
    }
    if op == "push" {
        return GhError::PushFailed(format!("git push failed:\n{bounded}"));
    }
    GhError::user(format!("git {op} failed: {bounded}"))
}

/// Auth only. "Could not read from remote repository" is also printed for a
/// missing or unresolvable remote URL, so it is not an auth signature.
fn is_git_auth_failure(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("authentication")
        || lower.contains("permission denied")
        || lower.contains("403")
        || lower.contains("terminal prompts disabled")
        || lower.contains("publickey")
}

fn classify_gh(
    err: String,
    worktree: &Path,
    branch: &str,
    title: &str,
    body: Option<&str>,
    stat: &Diffstat,
) -> GhError {
    let lower = err.to_ascii_lowercase();
    if lower.contains("not logged") || lower.contains("not signed") || lower.contains("auth") {
        let gh = GhObservation {
            found: true,
            authenticated: Some(false),
            binary: None,
            remediation: "gh is installed but not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into(),
        };
        return GhError::GhSignedOut {
            instructions: manual_pr_instructions(worktree, branch, title, body, stat, &gh),
        };
    }
    GhError::user(format!("gh failed: {err}"))
}

async fn git(cwd: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|err| format!("git {} failed: {err}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

async fn run_gh(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    if args
        .iter()
        .any(|arg| *arg == "merge" || *arg == "--merge" || *arg == "--auto" || *arg == "graphql")
        || args.windows(2).any(|pair| pair == ["api", "graphql"])
    {
        return Err("refusing to run a merge or GraphQL gh command".into());
    }
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn gh: {err}"))?;
    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("gh {} timed out", args.join(" ")))?
        .map_err(|err| format!("gh {} failed: {err}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() || args == ["pr", "checks"] {
        // `gh pr checks` exits non-zero when checks are pending or failing;
        // the table is still the digest we want.
        if output.status.success() || !stdout.is_empty() {
            return Ok(stdout);
        }
    }
    Err(if stderr.is_empty() { stdout } else { stderr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_paired_repos() -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new().unwrap();
        let bare = dir.path().join("origin.git");
        let work = dir.path().join("work");
        run(
            dir.path(),
            &["git", "init", "--bare", bare.to_str().unwrap()],
        );
        std::fs::create_dir_all(&work).unwrap();
        run(&work, &["git", "init", "-b", "main"]);
        run(&work, &["git", "config", "user.email", "dev@example.com"]);
        run(&work, &["git", "config", "user.name", "Dev"]);
        std::fs::write(work.join("README.md"), "hello\n").unwrap();
        run(&work, &["git", "add", "README.md"]);
        run(&work, &["git", "commit", "-m", "init"]);
        run(
            &work,
            &["git", "remote", "add", "origin", bare.to_str().unwrap()],
        );
        run(&work, &["git", "push", "-u", "origin", "main"]);
        (dir, work, bare)
    }

    fn run(cwd: &Path, args: &[&str]) {
        let status = StdCommand::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "{args:?} failed in {}", cwd.display());
    }

    fn write_executable(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn gh_base_branch_strips_a_remote_prefix() {
        assert_eq!(gh_base_branch("origin/develop"), "develop");
        assert_eq!(gh_base_branch("refs/remotes/origin/develop"), "develop");
        assert_eq!(gh_base_branch("main"), "main");
    }

    #[test]
    fn generated_commit_message_is_deterministic() {
        let stat = Diffstat {
            files: 3,
            insertions: 12,
            deletions: 4,
            truncated: false,
        };
        assert_eq!(
            generate_commit_message("first change", &stat),
            "first change\n\n3 files changed, 12 insertions(+), 4 deletions(-)"
        );
        assert_eq!(
            generate_commit_message("first change", &stat),
            generate_commit_message("first change", &stat)
        );
    }

    #[test]
    fn shortstat_parser_reads_git_phrasing() {
        let stat = parse_shortstat(" 1 file changed, 1 insertion(+), 1 deletion(-)");
        assert_eq!(
            stat,
            Diffstat {
                files: 1,
                insertions: 1,
                deletions: 1,
                truncated: false,
            }
        );
        let only_add = parse_shortstat(" 2 files changed, 8 insertions(+)");
        assert_eq!(only_add.insertions, 8);
        assert_eq!(only_add.deletions, 0);
    }

    #[test]
    fn checks_summary_counts_buckets() {
        let table = "lint\tpass\t1s\thttps://example.test/lint\ntest\tpending\t0\thttps://example.test/test\nfmt\tfail\t2s\thttps://example.test/fmt\n";
        assert_eq!(
            summarize_checks(table).as_deref(),
            Some("1 passing, 1 pending, 1 failing")
        );
        assert_eq!(summarize_checks("").as_deref(), Some("no checks"));
    }

    #[tokio::test]
    async fn commit_refuses_a_clean_tree_and_commits_when_dirty() {
        let (_dir, work, _bare) = init_paired_repos();
        let err = commit_all(&work, "first change", None).await.unwrap_err();
        assert!(matches!(err, GhError::NothingToCommit));

        std::fs::write(work.join("extra.txt"), "line\n").unwrap();
        let committed = commit_all(&work, "first change", None).await.unwrap();
        assert_eq!(
            committed.message,
            "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)"
        );
        assert_eq!(committed.stat.files, 1);
        assert!(!committed.sha.is_empty());

        let again = commit_all(&work, "first change", None).await.unwrap_err();
        assert!(matches!(again, GhError::NothingToCommit));
    }

    #[test]
    fn classify_git_treats_only_auth_signatures_as_auth() {
        let repo_missing = "fatal: '../no-such-remote' does not appear to be a git repository\n\
fatal: Could not read from remote repository.\n";
        assert!(
            matches!(
                classify_git(repo_missing.into(), "push"),
                GhError::PushFailed(_)
            ),
            "unresolvable remote must not be git_auth_failed"
        );

        for auth in [
            "fatal: Authentication failed for 'https://example.test/repo.git'",
            "Permission denied (publickey).",
            "The requested URL returned error: 403",
            "fatal: could not read Username for 'https://example.test': terminal prompts disabled",
        ] {
            assert!(
                matches!(classify_git(auth.into(), "push"), GhError::AuthFailed(_)),
                "expected auth for {auth}"
            );
        }
    }

    #[tokio::test]
    async fn push_of_an_unresolvable_remote_is_not_auth() {
        let (_dir, work, _bare) = init_paired_repos();
        run(&work, &["git", "checkout", "-b", "tidebreak/push-fail"]);
        std::fs::write(work.join("extra.txt"), "line\n").unwrap();
        commit_all(&work, "first change", Some("msg"))
            .await
            .unwrap();
        run(
            &work,
            &["git", "remote", "set-url", "origin", "../no-such-remote"],
        );
        let err = push_branch(&work, "tidebreak/push-fail").await.unwrap_err();
        assert!(
            !matches!(err, GhError::AuthFailed(_)),
            "non-auth push classified as auth: {err}"
        );
        match err {
            GhError::PushFailed(message) => {
                let lower = message.to_ascii_lowercase();
                assert!(
                    lower.contains("does not appear") || lower.contains("repository"),
                    "{message}"
                );
            }
            other => panic!("expected PushFailed, got {other}"),
        }
    }

    #[tokio::test]
    async fn push_sends_the_branch_to_a_bare_origin() {
        let (_dir, work, bare) = init_paired_repos();
        run(&work, &["git", "checkout", "-b", "tidebreak/first-change"]);
        std::fs::write(work.join("extra.txt"), "line\n").unwrap();
        commit_all(&work, "first change", Some("custom message"))
            .await
            .unwrap();
        let pushed = push_branch(&work, "tidebreak/first-change").await.unwrap();
        assert_eq!(pushed.remote, "origin");
        let listed = git(
            &bare,
            &["branch", "--list", "tidebreak/first-change"],
            GIT_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(listed.contains("tidebreak/first-change"), "{listed}");
    }

    #[tokio::test]
    async fn gh_absent_and_signed_out_are_typed() {
        let empty = TempDir::new().unwrap();
        let absent = observe_gh(Some(empty.path().to_str().unwrap())).await;
        assert!(!absent.found);
        assert!(absent.remediation.contains("gh is not installed"));

        let shim_dir = TempDir::new().unwrap();
        write_executable(
            &shim_dir.path().join("gh"),
            "#!/bin/sh\nif [ \"$1\" = auth ]; then echo signed out >&2; exit 1; fi\necho unexpected >&2; exit 3\n",
        );
        let signed_out = observe_gh(Some(shim_dir.path().to_str().unwrap())).await;
        assert!(signed_out.found);
        assert_eq!(signed_out.authenticated, Some(false));
        assert!(signed_out.remediation.contains("gh auth login"));
    }

    #[tokio::test]
    async fn create_pr_uses_view_and_checks_and_never_merges() {
        let (_dir, work, _bare) = init_paired_repos();
        run(&work, &["git", "checkout", "-b", "tidebreak/first-change"]);
        std::fs::write(work.join("extra.txt"), "line\n").unwrap();
        commit_all(&work, "first change", None).await.unwrap();
        push_branch(&work, "tidebreak/first-change").await.unwrap();

        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = merge ] || [ "$2" = merge ]; then echo merge-forbidden >&2; exit 2; fi
for arg in "$@"; do
  if [ "$arg" = --auto ]; then echo auto-forbidden >&2; exit 2; fi
done
if [ "$1" = auth ]; then echo logged in; exit 0; fi
if [ "$1" = pr ] && [ "$2" = create ]; then
  echo https://github.com/example/demo/pull/12
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  echo '{{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN"}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = checks ]; then
  printf 'lint\tpass\t1s\thttps://example.test/lint\n'
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
                log = log.display()
            ),
        );
        let cache = PrDigestCache::default();
        let digest = create_pull_request(
            &work,
            WorkspaceId::new(),
            "first change",
            "tidebreak/first-change",
            "origin/main",
            None,
            None,
            &cache,
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(digest.number, 12);
        assert_eq!(
            digest.url.as_deref(),
            Some("https://github.com/example/demo/pull/12")
        );
        assert_eq!(digest.state, "open");
        assert_eq!(
            digest.checks_summary.as_deref(),
            Some("1 passing, 0 pending, 0 failing")
        );
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("pr create"), "{logged}");
        assert!(
            logged.contains("--base main"),
            "gh pr create must target the workspace base, not the host default: {logged}"
        );
        assert!(!logged.contains("--base origin/main"), "{logged}");
        assert!(logged.contains("pr view"), "{logged}");
        assert!(logged.contains("pr checks"), "{logged}");
        assert!(!logged.contains("merge"), "{logged}");
        assert!(!logged.contains("--auto"), "{logged}");
        assert!(!logged.contains("graphql"), "{logged}");
    }

    #[tokio::test]
    async fn quick_action_bounds_output_and_times_out() {
        let dir = TempDir::new().unwrap();
        let long = QuickAction {
            name: "noise".into(),
            command: format!("printf '%{}s' '' | tr ' ' x", MAX_OUTPUT_CHARS + 80),
            auto_run_on_create: false,
        };
        let noisy = run_action(dir.path(), &long).await;
        assert!(noisy.success, "{}", noisy.stderr);
        assert_eq!(noisy.stdout.chars().count(), MAX_OUTPUT_CHARS);
        assert!(!noisy.timed_out);

        let sleeper = QuickAction {
            name: "sleep".into(),
            command: "sleep 20".into(),
            auto_run_on_create: false,
        };
        let timed = run_action(dir.path(), &sleeper).await;
        assert!(timed.timed_out);
        assert!(!timed.success);
        assert!(timed.stderr.contains("timed out"));
    }
}
