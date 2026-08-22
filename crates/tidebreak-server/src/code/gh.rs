//! Git commit/push and `gh` pull-request operations for a workspace.
//!
//! Every subprocess is bounded and non-interactive. Arguments are an argv
//! array, never a shell string. `gh` credentials are observed, never stored.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use tidebreak_core::{Diffstat, PullRequestDigest, QuickAction, WorkspaceId};
use tidebreak_harness::{filter_child_env, probe_shell, HostEnv};

use super::setup_script::spawn_workspace_script;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(120);
const GH_TIMEOUT: Duration = Duration::from_secs(30);
const PR_DIGEST_TIMEOUT: Duration = Duration::from_secs(60);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_CHARS: usize = 4_096;
const PR_CACHE_TTL: Duration = Duration::from_secs(20);
const GH_OBSERVATION_TTL: Duration = Duration::from_secs(30);
pub(crate) const GH_UNAVAILABLE_PREFIX: &str = "gh_unavailable: ";
pub(crate) const PR_HEAD_CHANGED_PREFIX: &str = "pr_head_changed: ";

static GH_LAUNCH: OnceLock<GhLaunch> = OnceLock::new();
static GH_OBSERVATION: OnceLock<AsyncMutex<Option<CachedGhObservation>>> = OnceLock::new();

#[derive(Debug, Clone)]
struct GhLaunch {
    binary: PathBuf,
    login_env: Option<Arc<Vec<(OsString, OsString)>>>,
}

#[derive(Debug, Clone)]
struct CachedGhObservation {
    observed_at: Instant,
    observation: GhObservation,
}

impl CachedGhObservation {
    fn get(&self, force_refresh: bool, request_started: Instant) -> Option<GhObservation> {
        let fresh = if force_refresh {
            self.observed_at >= request_started
        } else {
            self.observed_at.elapsed() <= GH_OBSERVATION_TTL
        };
        fresh.then(|| self.observation.clone())
    }
}

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
    MergeBlocked(String),
    #[error("{0}")]
    User(String),
    #[error("{0}")]
    Internal(String),
}

impl GhError {
    fn user(message: impl Into<String>) -> Self {
        Self::User(message.into())
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::User(format!("{GH_UNAVAILABLE_PREFIX}{}", message.into()))
    }

    fn pull_request_head_changed(message: impl Into<String>) -> Self {
        Self::User(format!("{PR_HEAD_CHANGED_PREFIX}{}", message.into()))
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

/// Resolve `owner/repo` to a clone URL.
///
/// When `gh` is signed in, the URL is whatever `gh repo view --json url`
/// reports. Otherwise the HTTPS GitHub URL is constructed. Credentials are
/// never read or stored.
pub(crate) async fn resolve_github_clone_url(
    owner_repo: &str,
    search_path: Option<&str>,
) -> Result<String, GhError> {
    let gh = observe_gh(search_path).await;
    if gh.found && gh.authenticated == Some(true) {
        let binary = gh.binary.as_ref().ok_or_else(|| GhError::GhAbsent {
            instructions: gh.remediation.clone(),
        })?;
        let json = run_gh(
            Path::new("."),
            binary,
            &["repo", "view", owner_repo, "--json", "url"],
            GH_TIMEOUT,
        )
        .await
        .map_err(|err| GhError::user(format!("could not resolve {owner_repo}: {err}")))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
        let url = parsed
            .get("url")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| GhError::user(format!("gh did not report a url for {owner_repo}")))?;
        return Ok(github_clone_url(url));
    }
    Ok(format!("https://github.com/{owner_repo}.git"))
}

fn github_clone_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.ends_with(".git") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.git")
    }
}

/// Observed `gh` availability. Tokens are never read or stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GhObservation {
    pub found: bool,
    pub authenticated: Option<bool>,
    pub viewer_login: Option<String>,
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
        title: None,
        checks_summary: None,
        checks: None,
        draft: None,
        merged: None,
        review_decision: None,
        mergeable: None,
        merge_state_status: None,
        head_branch: None,
        base_branch: None,
        head_sha: None,
        auto_merge_enabled: None,
        in_merge_queue: None,
    };
    cache.put(workspace_id, digest.clone());
    Ok(digest)
}

/// Merge strategy for the user-initiated merge operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    fn flag(self) -> &'static str {
        match self {
            Self::Squash => "--squash",
            Self::Merge => "--merge",
            Self::Rebase => "--rebase",
        }
    }
}

/// Merge the workspace PR, or enable auto-merge on it. This is the only path
/// allowed to run `gh pr merge`, and it exists solely for the user-initiated
/// merge endpoint — agent and automation paths go through [`run_gh`], which
/// refuses merge argv outright.
pub(crate) async fn merge_pull_request(
    worktree: &Path,
    workspace_id: WorkspaceId,
    method: MergeMethod,
    auto: bool,
    cache: &PrDigestCache,
    gh_search_path: Option<&str>,
) -> Result<(), GhError> {
    let gh = observe_gh(gh_search_path).await;
    let binary = require_gh_binary(&gh)?;
    let mut args = vec!["pr", "merge", method.flag()];
    if auto {
        args.push("--auto");
    }
    run_gh_user_merge(worktree, &binary, &args, GH_TIMEOUT)
        .await
        .map_err(classify_merge_error)?;
    cache.invalidate(workspace_id);
    Ok(())
}

/// Mark the workspace's own draft pull request ready for review.
///
/// The worktree's branch is what `gh` resolves the pull request from, the same
/// way the merge operation does, so this needs no repository coordinates —
/// that is the difference from [`mark_pull_request_ready`], which serves the
/// repository-qualified delivery surface.
///
/// It stays on the general runner. Readying a draft is a user state change but
/// not a merge, and widening the merge-only runner to carry it would undo the
/// point of having two runners (decision 42).
pub(crate) async fn mark_workspace_pull_request_ready(
    worktree: &Path,
    workspace_id: WorkspaceId,
    cache: &PrDigestCache,
    gh_search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(gh_search_path).await;
    let binary = require_gh_binary(&observation)?;
    run_gh(worktree, &binary, &["pr", "ready"], GH_TIMEOUT)
        .await
        .map_err(|error| classify_observed_gh(error, &observation))?;
    cache.invalidate(workspace_id);
    Ok(())
}

/// Turn a `gh pr merge` failure into something the PR card can show.
pub(crate) fn classify_merge_error(err: String) -> GhError {
    let bounded = bound_text(&err);
    let lower = bounded.to_ascii_lowercase();
    // Sign-out markers must be specific: a bare `auth` substring also matches
    // host messages that name the pull request author, which would send a
    // blocked merge to the sign-in remediation instead.
    if is_gh_signed_out_error(&bounded) {
        return GhError::GhSignedOut {
            instructions: "gh is installed but not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into(),
        };
    }
    if lower.contains("head branch was modified")
        || lower.contains("head commit") && lower.contains("match")
    {
        return GhError::pull_request_head_changed(
            "the pull request head changed; refresh it before merging",
        );
    }
    if is_gh_unavailable_error(&bounded) {
        return GhError::unavailable(format!("gh pr merge is unavailable: {bounded}"));
    }
    if lower.contains("not mergeable")
        || lower.contains("merge conflict")
        || lower.contains("branch protection")
        || lower.contains("protected branch")
        || lower.contains("base branch policy")
        || lower.contains("required status check")
        || lower.contains("review is required")
        || lower.contains("reviews are required")
        || lower.contains("draft")
        || lower.contains("already been merged")
        || lower.contains("clean status")
    {
        return GhError::MergeBlocked(format!("the pull request cannot be merged: {bounded}"));
    }
    GhError::User(format!("gh pr merge failed: {bounded}"))
}

/// PR comments read live from the host. Never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrComments {
    pub number: u64,
    pub comments: Vec<tidebreak_core::PullRequestComment>,
}

/// Load issue comments, review bodies, and inline review comments for the
/// workspace PR. Inline comments come from the REST endpoint because
/// `gh pr view --json` does not carry file/line positions.
pub(crate) async fn load_pr_comments(
    worktree: &Path,
    gh_search_path: Option<&str>,
) -> Result<PrComments, GhError> {
    let gh = observe_gh(gh_search_path).await;
    let binary = require_gh_binary(&gh)?;
    let view = run_gh(
        worktree,
        &binary,
        &["pr", "view", "--json", "number,comments,reviews"],
        GH_TIMEOUT,
    )
    .await
    .map_err(|err| GhError::user(format!("could not read PR comments: {err}")))?;
    let (number, mut comments) = parse_pr_view_comments(&view);
    let Some(number) = number else {
        return Err(GhError::user(
            "no pull request found for this branch".to_owned(),
        ));
    };
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments");
    // Inline comments are additive: a failed REST read still returns the
    // conversation-tab comments rather than failing the whole request.
    if let Ok(json) = run_gh(worktree, &binary, &["api", &endpoint], GH_TIMEOUT).await {
        comments.extend(parse_review_comments(&json));
    }
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(PrComments { number, comments })
}

fn require_gh_binary(gh: &GhObservation) -> Result<PathBuf, GhError> {
    if !gh.found {
        return Err(GhError::GhAbsent {
            instructions: gh.remediation.clone(),
        });
    }
    match gh.authenticated {
        Some(true) => {}
        Some(false) => {
            return Err(GhError::GhSignedOut {
                instructions: gh.remediation.clone(),
            });
        }
        None => {
            return Err(GhError::unavailable(gh.remediation.clone()));
        }
    }
    gh.binary.clone().ok_or_else(|| GhError::GhAbsent {
        instructions: gh.remediation.clone(),
    })
}

/// Parse `gh pr view --json number,comments,reviews`: issue comments plus
/// review bodies. Missing or malformed entries are skipped, never fatal.
pub(crate) fn parse_pr_view_comments(
    json: &str,
) -> (Option<u64>, Vec<tidebreak_core::PullRequestComment>) {
    use tidebreak_core::{PullRequestComment, PullRequestCommentKind};

    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let number = parsed.get("number").and_then(|value| value.as_u64());
    let mut comments = Vec::new();
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(|inner| inner.as_str())
            .filter(|inner| !inner.is_empty())
            .map(ToOwned::to_owned)
    };
    let login = |value: &serde_json::Value| {
        value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(|inner| inner.as_str())
            .filter(|inner| !inner.is_empty())
            .map(ToOwned::to_owned)
    };
    let avatar = |value: &serde_json::Value| {
        value
            .get("author")
            .and_then(|author| author.get("avatarUrl").or_else(|| author.get("avatar_url")))
            .and_then(|inner| inner.as_str())
            .filter(|inner| !inner.is_empty())
            .map(ToOwned::to_owned)
    };
    let id = |value: &serde_json::Value| json_id(value.get("id"));
    for item in parsed
        .get("comments")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        let Some(body) = text(item, "body") else {
            continue;
        };
        comments.push(PullRequestComment {
            kind: PullRequestCommentKind::Issue,
            id: id(item),
            author: login(item),
            avatar_url: avatar(item),
            url: text(item, "url"),
            created_at: text(item, "createdAt"),
            body,
            review_state: None,
            path: None,
            line: None,
        });
    }
    for item in parsed
        .get("reviews")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        // A review submitted without a body (a bare approve) carries nothing
        // to read; the decision already rides the digest.
        let Some(body) = text(item, "body") else {
            continue;
        };
        comments.push(PullRequestComment {
            kind: PullRequestCommentKind::Review,
            id: id(item),
            author: login(item),
            avatar_url: avatar(item),
            url: text(item, "url"),
            created_at: text(item, "submittedAt").or_else(|| text(item, "createdAt")),
            body,
            review_state: text(item, "state").map(|state| state.to_ascii_lowercase()),
            path: None,
            line: None,
        });
    }
    (number, comments)
}

/// Parse the REST `pulls/{n}/comments` array: inline review comments with
/// file path and line. Missing fields are tolerated.
pub(crate) fn parse_review_comments(json: &str) -> Vec<tidebreak_core::PullRequestComment> {
    use tidebreak_core::{PullRequestComment, PullRequestCommentKind};

    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let mut comments = Vec::new();
    for item in parsed.as_array().into_iter().flatten() {
        let Some(body) = item
            .get("body")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let text = |key: &str| {
            item.get(key)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        };
        comments.push(PullRequestComment {
            kind: PullRequestCommentKind::Inline,
            id: json_id(item.get("id")),
            author: item
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            avatar_url: item
                .get("user")
                .and_then(|user| user.get("avatar_url"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            url: text("html_url"),
            created_at: text("created_at"),
            body: body.to_owned(),
            review_state: None,
            path: text("path"),
            line: item
                .get("line")
                .and_then(|value| value.as_u64())
                .or_else(|| item.get("original_line").and_then(|value| value.as_u64())),
        });
    }
    comments
}

fn json_id(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(serde_json::Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
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

pub(crate) async fn observe_gh(search_path: Option<&str>) -> GhObservation {
    observe_gh_with_cache(search_path, false).await
}

pub(crate) async fn refresh_gh_observation(search_path: Option<&str>) -> GhObservation {
    observe_gh_with_cache(search_path, true).await
}

async fn observe_gh_with_cache(search_path: Option<&str>, force_refresh: bool) -> GhObservation {
    let request_started = Instant::now();
    if search_path.is_some() {
        return observe_gh_uncached(search_path).await;
    }

    let cache = GH_OBSERVATION.get_or_init(|| AsyncMutex::new(None));
    let mut cached = cache.lock().await;
    if let Some(observation) = cached
        .as_ref()
        .and_then(|entry| entry.get(force_refresh, request_started))
    {
        return observation;
    }

    let observation = observe_gh_uncached(None).await;
    *cached = Some(CachedGhObservation {
        observed_at: Instant::now(),
        observation: observation.clone(),
    });
    observation
}

async fn observe_gh_uncached(search_path: Option<&str>) -> GhObservation {
    let Some(binary) = resolve_gh_binary(search_path).await else {
        return GhObservation {
            found: false,
            authenticated: None,
            viewer_login: None,
            binary: None,
            remediation: "gh is not installed. Install the GitHub CLI from https://cli.github.com/ and sign in with `gh auth login` in a terminal. Tidebreak does not store GitHub credentials.".into(),
        };
    };

    let status = run_gh(
        Path::new("."),
        &binary,
        &["auth", "status", "--json", "hosts"],
        GH_TIMEOUT,
    )
    .await;
    match status {
        Ok(raw) => match parse_auth_status(&raw) {
            Some(status) if status.authenticated => GhObservation {
                found: true,
                authenticated: Some(true),
                viewer_login: status.viewer_login,
                binary: Some(binary),
                remediation: String::new(),
            },
            Some(_) => signed_out_observation(binary),
            None => unavailable_observation(
                binary,
                "gh returned an unreadable authentication status. Retry the request or run `gh auth status` in a terminal.",
            ),
        },
        Err(message) if message.to_ascii_lowercase().contains("unknown flag") => {
            match run_gh(Path::new("."), &binary, &["auth", "status"], GH_TIMEOUT).await {
                Ok(_) => GhObservation {
                    found: true,
                    authenticated: Some(true),
                    viewer_login: None,
                    binary: Some(binary),
                    remediation: String::new(),
                },
                Err(message) if is_gh_signed_out_error(&message) => signed_out_observation(binary),
                Err(message) => unavailable_observation(binary, &message),
            }
        }
        Err(message) if is_gh_signed_out_error(&message) => signed_out_observation(binary),
        Err(message) => unavailable_observation(binary, &message),
    }
}

async fn resolve_gh_binary(search_path: Option<&str>) -> Option<PathBuf> {
    if let Some(search_path) = search_path {
        return find_gh(Some(search_path));
    }
    if let Some(launch) = GH_LAUNCH.get() {
        return Some(launch.binary.clone());
    }
    let launch = match find_gh(None) {
        Some(binary) => GhLaunch {
            binary,
            login_env: None,
        },
        None => {
            let capture = probe_shell(&HostEnv::from_process(), "gh").await.ok()?;
            GhLaunch {
                binary: capture.binary,
                login_env: Some(Arc::new(capture.env)),
            }
        }
    };
    let _ = GH_LAUNCH.set(launch);
    GH_LAUNCH.get().map(|launch| launch.binary.clone())
}

fn signed_out_observation(binary: PathBuf) -> GhObservation {
    GhObservation {
        found: true,
        authenticated: Some(false),
        viewer_login: None,
        binary: Some(binary),
        remediation: "gh is installed but not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into(),
    }
}

fn unavailable_observation(binary: PathBuf, message: &str) -> GhObservation {
    GhObservation {
        found: true,
        authenticated: None,
        viewer_login: None,
        binary: Some(binary),
        remediation: format!(
            "gh is installed, but Tidebreak could not check its authentication: {}",
            bound_text(message)
        ),
    }
}

fn is_gh_signed_out_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not logged")
        || lower.contains("not signed")
        || lower.contains("signed out")
        || lower.contains("gh auth login")
        || lower.contains("token is invalid")
        || lower.contains("authentication token")
        || lower.contains("http 401")
}

fn is_gh_unavailable_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("failed to spawn gh")
        || lower.contains("connection reset")
        || lower.contains("could not resolve host")
        || lower.contains("network is unreachable")
        || lower.contains("http 502")
        || lower.contains("http 503")
        || lower.contains("http 504")
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedGhAuthStatus {
    authenticated: bool,
    viewer_login: Option<String>,
}

fn parse_auth_status(raw: &str) -> Option<ParsedGhAuthStatus> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let accounts = parsed
        .get("hosts")?
        .as_object()?
        .values()
        .flat_map(|value| value.as_array().into_iter().flatten().collect::<Vec<_>>());
    let mut authenticated = false;
    let mut fallback = None;
    for account in accounts {
        if account.get("state").and_then(serde_json::Value::as_str) != Some("success") {
            continue;
        }
        authenticated = true;
        let login = account
            .get("login")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|login| !login.is_empty())
            .map(ToOwned::to_owned);
        if account.get("active").and_then(serde_json::Value::as_bool) == Some(true) {
            return Some(ParsedGhAuthStatus {
                authenticated,
                viewer_login: login,
            });
        }
        if fallback.is_none() {
            fallback = login;
        }
    }
    Some(ParsedGhAuthStatus {
        authenticated,
        viewer_login: fallback,
    })
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
    match gh.authenticated {
        Some(true) => {}
        Some(false) => {
            return Err(GhError::GhSignedOut {
                instructions: manual_pr_instructions(worktree, branch, title, body, stat, gh),
            });
        }
        None => {
            return Err(GhError::unavailable(gh.remediation.clone()));
        }
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
        "{header}\n\nCreate the pull request from a terminal:\n\n{commands}",
        header = gh.remediation,
        commands = manual_pr_commands(worktree, branch, &pr_title, &pr_body),
    )
}

#[cfg(not(windows))]
fn manual_pr_commands(worktree: &Path, branch: &str, title: &str, body: &str) -> String {
    format!(
        "  cd {worktree}\n  git push -u origin {branch}\n  gh pr create --title {title} --body {body}\n",
        worktree = shell_single_quote(&worktree.to_string_lossy()),
        branch = shell_single_quote(branch),
        title = shell_single_quote(title),
        body = shell_single_quote(body),
    )
}

#[cfg(windows)]
fn manual_pr_commands(worktree: &Path, branch: &str, title: &str, body: &str) -> String {
    format!(
        "  Set-Location -LiteralPath {worktree}\n  git push -u origin {branch}\n  gh pr create --title {title} --body {body}\n",
        worktree = powershell_single_quote(&worktree.to_string_lossy()),
        branch = powershell_single_quote(branch),
        title = powershell_single_quote(title),
        body = powershell_single_quote(body),
    )
}

#[cfg(not(windows))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

const PR_VIEW_FIELDS: &str = "number,url,state,title,isDraft,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRefName,headRefOid,baseRefName";
const PR_DIGEST_HEAD_ATTEMPTS: usize = 2;

#[derive(Debug)]
struct PrViewSnapshot {
    digest: PullRequestDigest,
    head_oid: Option<String>,
}

async fn load_pr_digest(
    worktree: &Path,
    gh: &GhObservation,
    _search_path: Option<&str>,
) -> Result<Option<PullRequestDigest>, GhError> {
    let Some(binary) = gh.binary.as_ref() else {
        return Ok(None);
    };
    // The old loader could spend one GH_TIMEOUT on the initial view and one
    // on its parallel checks/queue reads. Keep that same total wall-clock
    // bound even though a changed head now causes a bounded retry.
    Ok(timeout(
        PR_DIGEST_TIMEOUT,
        load_head_consistent_pr_digest(worktree, binary),
    )
    .await
    .ok()
    .flatten())
}

async fn load_head_consistent_pr_digest(
    worktree: &Path,
    binary: &Path,
) -> Option<PullRequestDigest> {
    let mut snapshot = load_pr_view_snapshot(worktree, binary).await?;

    for _ in 0..PR_DIGEST_HEAD_ATTEMPTS {
        let number = snapshot.digest.number;
        let open = snapshot.digest.state == "open";
        let checks_read = run_gh(worktree, binary, &["pr", "checks"], GH_TIMEOUT);
        let queue_read = async {
            if open {
                load_merge_queue_state(worktree, binary, number).await
            } else {
                Some(false)
            }
        };
        let (checks_table, in_merge_queue) = tokio::join!(checks_read, queue_read);

        let Some(verified) = load_pr_view_snapshot(worktree, binary).await else {
            return Some(conservative_pr_digest(snapshot.digest));
        };
        if same_pr_head(&snapshot, &verified) {
            let checks = parse_pr_checks(&checks_table.unwrap_or_default());
            let mut digest = verified.digest;
            digest.checks_summary = summarize_checks(&checks);
            digest.checks = (!checks.is_empty()).then_some(checks);
            digest.in_merge_queue = if digest.state == "open" {
                in_merge_queue
            } else {
                Some(false)
            };
            return Some(digest);
        }

        snapshot = verified;
    }

    Some(conservative_pr_digest(snapshot.digest))
}

async fn load_pr_view_snapshot(worktree: &Path, binary: &Path) -> Option<PrViewSnapshot> {
    let json = run_gh(
        worktree,
        binary,
        &["pr", "view", "--json", PR_VIEW_FIELDS],
        GH_TIMEOUT,
    )
    .await
    .ok()?;
    pr_view_snapshot_from_json(&json, "")
}

fn same_pr_head(before: &PrViewSnapshot, after: &PrViewSnapshot) -> bool {
    before.digest.number == after.digest.number
        && before
            .head_oid
            .as_deref()
            .zip(after.head_oid.as_deref())
            .is_some_and(|(before, after)| before == after)
}

fn conservative_pr_digest(mut digest: PullRequestDigest) -> PullRequestDigest {
    digest.checks_summary = None;
    digest.checks = None;
    digest.mergeable = None;
    digest.merge_state_status = None;
    digest.in_merge_queue = None;
    digest
}

async fn load_merge_queue_state(worktree: &Path, binary: &Path, number: u64) -> Option<bool> {
    let endpoint = format!("repos/{{owner}}/{{repo}}/issues/{number}/timeline?per_page=100");
    let events = run_gh(
        worktree,
        binary,
        &[
            "api",
            &endpoint,
            "--paginate",
            "--jq",
            ".[] | select(.event == \"added_to_merge_queue\" or .event == \"removed_from_merge_queue\") | .event",
        ],
        GH_TIMEOUT,
    )
    .await
    .ok()?;
    Some(in_merge_queue_from_timeline_events(&events))
}

fn in_merge_queue_from_timeline_events(events: &str) -> bool {
    events
        .lines()
        .map(str::trim)
        .rfind(|event| matches!(*event, "added_to_merge_queue" | "removed_from_merge_queue"))
        == Some("added_to_merge_queue")
}

/// Parse one `gh pr view --json` payload plus a `gh pr checks` table into the
/// stored digest. Every field beyond the number is optional and tolerated
/// missing.
#[cfg(test)]
pub(crate) fn digest_from_view_json(json: &str, checks_table: &str) -> Option<PullRequestDigest> {
    pr_view_snapshot_from_json(json, checks_table).map(|snapshot| snapshot.digest)
}

fn pr_view_snapshot_from_json(json: &str, checks_table: &str) -> Option<PrViewSnapshot> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let number = parsed.get("number").and_then(|value| value.as_u64())?;
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
    let title = parsed
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let checks = parse_pr_checks(checks_table);
    let lower_token = |key: &str| {
        parsed
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
    };
    let branch = |key: &str| {
        parsed
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    let head_oid = branch("headRefOid");
    let digest = PullRequestDigest {
        number,
        url,
        merged: Some(state == "merged"),
        state,
        title,
        checks_summary: summarize_checks(&checks),
        checks: if checks.is_empty() {
            None
        } else {
            Some(checks)
        },
        draft: parsed.get("isDraft").and_then(|value| value.as_bool()),
        review_decision: lower_token("reviewDecision"),
        mergeable: lower_token("mergeable"),
        merge_state_status: lower_token("mergeStateStatus"),
        head_branch: branch("headRefName"),
        base_branch: branch("baseRefName"),
        head_sha: head_oid.clone(),
        auto_merge_enabled: Some(
            parsed
                .get("autoMergeRequest")
                .is_some_and(|value| !value.is_null()),
        ),
        in_merge_queue: None,
    };
    Some(PrViewSnapshot { digest, head_oid })
}

pub(crate) fn parse_pr_checks(output: &str) -> Vec<tidebreak_core::PullRequestCheck> {
    use tidebreak_core::{PullRequestCheck, PullRequestCheckBucket};

    let mut checks = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("name\t") || lower.starts_with("check\t") {
            continue;
        }
        let columns: Vec<&str> = trimmed.split('\t').collect();
        let name = columns.first().copied().unwrap_or(trimmed).trim();
        if name.is_empty() {
            continue;
        }
        let status = columns.get(1).copied().unwrap_or(trimmed);
        let status_lower = status.to_ascii_lowercase();
        let bucket = if status_lower.contains("skip")
            || status_lower.contains("neutral")
            || status_lower.contains("cancel")
        {
            PullRequestCheckBucket::Skipped
        } else if status_lower.contains("pass") || status_lower.contains("success") {
            PullRequestCheckBucket::Pass
        } else if status_lower.contains("fail") || status_lower.contains("error") {
            PullRequestCheckBucket::Fail
        } else if status_lower.contains("pend")
            || status_lower.contains("progress")
            || status_lower.contains("queued")
        {
            PullRequestCheckBucket::Pending
        } else if lower.contains("skip") || lower.contains("neutral") || lower.contains("cancel") {
            PullRequestCheckBucket::Skipped
        } else if lower.contains("pass") || lower.contains("success") {
            PullRequestCheckBucket::Pass
        } else if lower.contains("fail") || lower.contains("error") {
            PullRequestCheckBucket::Fail
        } else {
            PullRequestCheckBucket::Pending
        };
        let detail = columns
            .get(1)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let url = columns
            .iter()
            .map(|value| value.trim())
            .find(|value| value.starts_with("http://") || value.starts_with("https://"))
            .map(ToOwned::to_owned);
        checks.push(PullRequestCheck {
            name: name.to_owned(),
            bucket,
            detail,
            url,
        });
    }
    checks
}

pub(crate) fn summarize_checks(checks: &[tidebreak_core::PullRequestCheck]) -> Option<String> {
    use tidebreak_core::PullRequestCheckBucket;

    if checks.is_empty() {
        return Some("no checks".into());
    }
    let mut passing = 0_u32;
    let mut failing = 0_u32;
    let mut pending = 0_u32;
    let mut skipped = 0_u32;
    for check in checks {
        match check.bucket {
            PullRequestCheckBucket::Pass => passing += 1,
            PullRequestCheckBucket::Fail => failing += 1,
            PullRequestCheckBucket::Pending => pending += 1,
            PullRequestCheckBucket::Skipped => skipped += 1,
        }
    }
    let mut summary = format!("{passing} passing, {pending} pending, {failing} failing");
    if skipped > 0 {
        summary.push_str(&format!(", {skipped} skipped"));
    }
    Some(summary)
}

fn pr_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|value| value.parse().ok())
}

#[cfg(windows)]
fn find_gh(search_path: Option<&str>) -> Option<PathBuf> {
    let path = search_path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    find_windows_gh(&path, std::env::var_os("PATHEXT").as_deref())
}

#[cfg(not(windows))]
fn find_gh(search_path: Option<&str>) -> Option<PathBuf> {
    let path = search_path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("gh");
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(windows)]
fn find_windows_gh(path: &std::ffi::OsStr, pathext: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let configured = pathext
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .filter_map(|value| {
                    if value.starts_with('.') {
                        launchable_windows_extension(value).then(|| value.to_owned())
                    } else {
                        let value = format!(".{value}");
                        launchable_windows_extension(&value).then_some(value)
                    }
                })
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| [".COM", ".exe"].map(str::to_owned).to_vec());
    let mut extensions = vec![".exe".to_owned()];
    for extension in configured {
        if !extensions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&extension))
        {
            extensions.push(extension);
        }
    }
    for dir in std::env::split_paths(path) {
        for extension in &extensions {
            let candidate = dir.join(format!("gh{extension}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn launchable_windows_extension(extension: &str) -> bool {
    [".COM", ".EXE"]
        .iter()
        .any(|supported| extension.eq_ignore_ascii_case(supported))
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
    let child = match spawn_workspace_script(worktree, script) {
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
        || lower.contains("401")
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
    if is_gh_signed_out_error(&err) {
        let gh = GhObservation {
            found: true,
            authenticated: Some(false),
            viewer_login: None,
            binary: None,
            remediation: "gh is installed but not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into(),
        };
        return GhError::GhSignedOut {
            instructions: manual_pr_instructions(worktree, branch, title, body, stat, &gh),
        };
    }
    if is_gh_unavailable_error(&err) {
        return GhError::unavailable(format!("gh is unavailable: {}", bound_text(&err)));
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

/// General `gh` runner for creation, status, and comment reads — every
/// agent-driven or automated path. It hard-refuses merge and auto-merge
/// arguments so no automation path can ever merge a PR; the one allowed merge
/// entry point is [`run_gh_user_merge`], reachable only from the dedicated
/// user-initiated merge operation. GraphQL is refused on every runner.
pub(crate) async fn run_gh(
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
    spawn_gh(cwd, binary, args, limit).await
}

/// Mark one repository-qualified draft pull request ready for review.
///
/// This is a user-initiated state change, but not a merge. It stays on the
/// general runner so the merge-only runner remains incapable of doing
/// anything else.
pub(crate) async fn mark_pull_request_ready(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    run_gh(
        Path::new("."),
        &binary,
        &["pr", "ready", &number, "--repo", &repository],
        GH_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| classify_observed_gh(error, &observation))
}

/// Close one repository-qualified pull request without merging it.
///
/// Like `mark_pull_request_ready`, this is a user-initiated state change that
/// is not a merge, so it stays on the general runner.
pub(crate) async fn close_pull_request_target(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    run_gh(
        Path::new("."),
        &binary,
        &["pr", "close", &number, "--repo", &repository],
        GH_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| classify_observed_gh(error, &observation))
}

/// Reopen one repository-qualified pull request that was closed unmerged.
pub(crate) async fn reopen_pull_request_target(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    run_gh(
        Path::new("."),
        &binary,
        &["pr", "reopen", &number, "--repo", &repository],
        GH_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| classify_observed_gh(error, &observation))
}

/// Post one issue comment on a repository-qualified pull request.
///
/// The body reaches `gh` as an argv value, never a shell string, so backticks
/// and newlines in a review note are inert.
pub(crate) async fn comment_on_pull_request_target(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    body: &str,
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    run_gh(
        Path::new("."),
        &binary,
        &[
            "pr",
            "comment",
            &number,
            "--repo",
            &repository,
            "--body",
            body,
        ],
        GH_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| classify_observed_gh(error, &observation))
}

/// Repository-qualified merge for a PR that may not have a local Tidebreak
/// workspace. The runner still admits only `gh pr merge` argv.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn merge_pull_request_target(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    method: MergeMethod,
    auto: bool,
    expected_head_sha: &str,
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    let mut args = vec![
        "pr".to_owned(),
        "merge".to_owned(),
        number,
        "--repo".to_owned(),
        repository,
        method.flag().to_owned(),
        "--match-head-commit".to_owned(),
        expected_head_sha.to_owned(),
    ];
    if auto {
        args.push("--auto".to_owned());
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_gh_user_merge(Path::new("."), &binary, &borrowed, GH_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(classify_merge_error)
}

pub(crate) async fn rerun_failed_jobs_with_observation(
    observation: &GhObservation,
    host: &str,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GhError> {
    let binary = require_gh_binary(observation)?;
    let endpoint = format!("repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs");
    let mut args = vec![
        "api".to_owned(),
        "--method".to_owned(),
        "POST".to_owned(),
        endpoint,
    ];
    if host != "github.com" {
        args.extend(["--hostname".to_owned(), host.to_owned()]);
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_gh(Path::new("."), &binary, &borrowed, GH_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|error| classify_observed_gh(error, observation))
}

pub(crate) fn cli_repository(host: &str, owner: &str, repo: &str) -> String {
    if host == "github.com" {
        format!("{owner}/{repo}")
    } else {
        format!("{host}/{owner}/{repo}")
    }
}

/// Fields a pull-request fact snapshot needs (decision 62). Narrower than the
/// delivery list fields: no checks, review, or mergeability — those stay
/// live-only.
pub(crate) const PR_FACT_FIELDS: &str = "number,url,title,state,isDraft,author,headRefName,headRefOid,baseRefName,createdAt,updatedAt,mergedAt,closedAt";

/// Read one repository-qualified pull request's fact fields, as raw JSON.
pub(crate) async fn view_pull_request_raw(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    search_path: Option<&str>,
) -> Result<serde_json::Value, GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let number = number.to_string();
    let raw = run_gh(
        Path::new("."),
        &binary,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &repository,
            "--json",
            PR_FACT_FIELDS,
        ],
        GH_TIMEOUT,
    )
    .await
    .map_err(|error| classify_observed_gh(error, &observation))?;
    serde_json::from_str(&raw)
        .map_err(|error| GhError::Internal(format!("could not parse pull request: {error}")))
}

/// List a repository's pull requests whose head is one branch, as raw JSON.
///
/// `--state all` so a push confirmed just after a merge still resolves; the
/// caller picks among the handful of results.
pub(crate) async fn list_pull_requests_for_head_raw(
    host: &str,
    owner: &str,
    repo: &str,
    head_branch: &str,
    search_path: Option<&str>,
) -> Result<Vec<serde_json::Value>, GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let repository = cli_repository(host, owner, repo);
    let raw = run_gh(
        Path::new("."),
        &binary,
        &[
            "pr",
            "list",
            "--repo",
            &repository,
            "--head",
            head_branch,
            "--state",
            "all",
            "--limit",
            "5",
            "--json",
            PR_FACT_FIELDS,
        ],
        GH_TIMEOUT,
    )
    .await
    .map_err(|error| classify_observed_gh(error, &observation))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| GhError::Internal(format!("could not parse pull requests: {error}")))?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

fn classify_observed_gh(error: String, observation: &GhObservation) -> GhError {
    if observation.authenticated == Some(false) || is_gh_signed_out_error(&error) {
        return GhError::GhSignedOut {
            instructions: if observation.remediation.is_empty() {
                "gh is not signed in. Run `gh auth login` in a terminal, then try again. Tidebreak does not store GitHub credentials.".into()
            } else {
                observation.remediation.clone()
            },
        };
    }
    if observation.authenticated.is_none() || is_gh_unavailable_error(&error) {
        return GhError::unavailable(
            if observation.authenticated.is_none() && !observation.remediation.is_empty() {
                observation.remediation.clone()
            } else {
                format!("gh is unavailable: {}", bound_text(&error))
            },
        );
    }
    GhError::user(format!("gh failed: {error}"))
}

/// Runner reserved for the user-initiated merge endpoint. It runs only
/// `gh pr merge …` argv — anything else, including GraphQL, is refused — so
/// the ability to merge stays scoped to the one operation the user asks for.
async fn run_gh_user_merge(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    if args.len() < 2 || args[0] != "pr" || args[1] != "merge" {
        return Err("the merge runner only runs gh pr merge".into());
    }
    if args.contains(&"graphql") {
        return Err("refusing to run a GraphQL gh command".into());
    }
    spawn_gh(cwd, binary, args, limit).await
}

async fn spawn_gh(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    let login_env = GH_LAUNCH
        .get()
        .filter(|launch| launch.binary == binary)
        .and_then(|launch| launch.login_env.as_deref().map(Vec::as_slice));
    spawn_gh_with_login_env(cwd, binary, login_env, args, limit).await
}

async fn spawn_gh_with_login_env(
    cwd: &Path,
    binary: &Path,
    login_env: Option<&[(OsString, OsString)]>,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    let mut command = Command::new(binary);
    if let Some(login_env) = login_env {
        command.env_clear();
        for (key, value) in filter_child_env(login_env.iter().cloned()) {
            command.env(key, value);
        }
    }
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

#[cfg(all(test, unix))]
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
        let table = "lint\tpass\t1s\thttps://example.test/lint\ntest\tpending\t0\thttps://example.test/test\nfmt\tfail\t2s\thttps://example.test/fmt\nrelease\tskipping\t0\thttps://example.test/release\n";
        let checks = parse_pr_checks(table);
        assert_eq!(checks.len(), 4);
        assert_eq!(checks[0].name, "lint");
        assert_eq!(
            summarize_checks(&checks).as_deref(),
            Some("1 passing, 1 pending, 1 failing, 1 skipped")
        );
        assert_eq!(summarize_checks(&[]).as_deref(), Some("no checks"));
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
            "The requested URL returned error: 401",
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

    #[test]
    fn auth_status_prefers_the_active_successful_login() {
        let raw = r#"{
            "hosts": {
                "github.example.com": [
                    {"active": false, "login": "fallback", "state": "success"}
                ],
                "github.com": [
                    {"active": false, "login": "stale", "state": "failure"},
                    {"active": true, "login": "active-user", "state": "success"}
                ]
            }
        }"#;
        assert_eq!(
            parse_auth_status(raw),
            Some(ParsedGhAuthStatus {
                authenticated: true,
                viewer_login: Some("active-user".into()),
            })
        );
    }

    #[tokio::test]
    async fn failure_only_json_auth_status_is_signed_out() {
        let shim_dir = TempDir::new().unwrap();
        write_executable(
            &shim_dir.path().join("gh"),
            r#"#!/bin/sh
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  printf '%s\n' '{"hosts":{"github.com":[{"active":true,"login":"stale-user","state":"failure"}]}}'
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
        );

        let observed = observe_gh(Some(shim_dir.path().to_str().unwrap())).await;
        assert!(observed.found);
        assert_eq!(observed.authenticated, Some(false));
        assert_eq!(observed.viewer_login, None);
        assert!(observed.remediation.contains("gh auth login"));
    }

    #[tokio::test]
    async fn unreadable_auth_status_is_unavailable_not_signed_out() {
        let shim_dir = TempDir::new().unwrap();
        write_executable(
            &shim_dir.path().join("gh"),
            "#!/bin/sh\nif [ \"$1\" = auth ]; then echo not-json; exit 0; fi\nexit 3\n",
        );

        let observed = observe_gh(Some(shim_dir.path().to_str().unwrap())).await;
        assert!(observed.found);
        assert_eq!(observed.authenticated, None);
        assert!(!observed.remediation.contains("gh auth login"));
        assert!(observed.remediation.contains("unreadable"));
    }

    #[tokio::test]
    async fn captured_login_environment_reaches_the_gh_child() {
        let shim_dir = TempDir::new().unwrap();
        let binary = shim_dir.path().join("gh");
        write_executable(
            &binary,
            "#!/bin/sh\nprintf '%s|%s' \"$GH_CONFIG_DIR\" \"${TIDEBREAK_PRIVATE-unset}\"\n",
        );
        let login_env = vec![
            (
                OsString::from("GH_CONFIG_DIR"),
                OsString::from("/shell/config"),
            ),
            (
                OsString::from("TIDEBREAK_PRIVATE"),
                OsString::from("must-not-leak"),
            ),
        ];

        let output = spawn_gh_with_login_env(
            shim_dir.path(),
            &binary,
            Some(&login_env),
            &["version"],
            GH_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(output, "/shell/config|unset");
    }

    #[tokio::test]
    async fn rerun_with_an_observation_does_not_repeat_authentication() {
        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        let binary = shim_dir.path().join("gh");
        write_executable(
            &binary,
            &format!(
                "#!/bin/sh\necho \"$@\" >> {}\n[ \"$1\" = api ] && exit 0\nexit 3\n",
                log.display()
            ),
        );
        let observation = GhObservation {
            found: true,
            authenticated: Some(true),
            viewer_login: Some("tester".into()),
            binary: Some(binary),
            remediation: String::new(),
        };

        let (first, second) = tokio::join!(
            rerun_failed_jobs_with_observation(&observation, "github.com", "acme", "app", 10,),
            rerun_failed_jobs_with_observation(&observation, "github.com", "acme", "app", 11,),
        );
        first.unwrap();
        second.unwrap();

        let logged = std::fs::read_to_string(log).unwrap();
        assert_eq!(logged.matches("api --method POST").count(), 2, "{logged}");
        assert!(!logged.contains("auth status"), "{logged}");
    }

    #[test]
    fn observation_cache_keeps_negative_results_and_coalesces_forced_refreshes() {
        let observed_at = Instant::now();
        let cached = CachedGhObservation {
            observed_at,
            observation: signed_out_observation(PathBuf::from("/tmp/gh")),
        };

        assert_eq!(
            cached.get(false, Instant::now()).unwrap().authenticated,
            Some(false)
        );
        assert_eq!(
            cached
                .get(
                    true,
                    observed_at.checked_sub(Duration::from_millis(1)).unwrap(),
                )
                .unwrap()
                .authenticated,
            Some(false)
        );
        assert!(cached
            .get(true, observed_at + Duration::from_millis(1))
            .is_none());
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
if [ "$1" = auth ]; then echo '{{"hosts":{{"github.com":[{{"active":true,"login":"tester","state":"success"}}]}}}}'; exit 0; fi
if [ "$1" = pr ] && [ "$2" = create ]; then
  echo https://github.com/example/demo/pull/12
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  echo '{{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN","headRefOid":"aaaaaaaa"}}'
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
        // The view field list legitimately names mergeable/autoMergeRequest;
        // what may never appear is a merge invocation or flag.
        assert!(!logged.contains("pr merge"), "{logged}");
        assert!(!logged.contains("--merge"), "{logged}");
        assert!(!logged.contains("--auto"), "{logged}");
        assert!(!logged.contains("graphql"), "{logged}");

        // The general runner refuses merge argv before spawning anything —
        // creation and status paths cannot merge even if handed the argv.
        for argv in [
            &["pr", "merge", "--squash"][..],
            &["pr", "merge", "--auto", "--squash"][..],
            &["api", "graphql"][..],
        ] {
            let refused = run_gh(&work, &shim_dir.path().join("gh"), argv, GH_TIMEOUT)
                .await
                .unwrap_err();
            assert!(refused.contains("refusing"), "{argv:?}: {refused}");
        }
        let after = std::fs::read_to_string(&log).unwrap();
        assert_eq!(logged, after, "a refused command must never reach gh");
    }

    #[tokio::test]
    async fn readying_a_draft_runs_pr_ready_and_cannot_reach_the_merge_runner() {
        // Readying a draft is a user state change but not a merge, so it rides
        // the general runner (decision 42). Two things have to hold: it really
        // runs `gh pr ready` in the worktree, and the runner it uses still
        // refuses merge argv — otherwise widening the app to ready a draft
        // would have quietly widened it to merge.
        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = auth ]; then echo '{{"hosts":{{"github.com":[{{"active":true,"login":"tester","state":"success"}}]}}}}'; exit 0; fi
if [ "$1" = pr ] && [ "$2" = ready ]; then exit 0; fi
echo unexpected "$@" >&2
exit 3
"#,
                log = log.display()
            ),
        );
        let work = TempDir::new().unwrap();
        let cache = PrDigestCache::default();
        let workspace = WorkspaceId::new();
        mark_workspace_pull_request_ready(
            work.path(),
            workspace,
            &cache,
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .expect("gh pr ready should run");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("pr ready"), "{logged}");
        assert!(!logged.contains("merge"), "{logged}");

        let refused = run_gh(
            work.path(),
            &shim_dir.path().join("gh"),
            &["pr", "merge", "--squash"],
            GH_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(refused.contains("refusing"), "{refused}");
    }

    #[tokio::test]
    async fn only_the_user_merge_runner_may_merge_and_it_runs_nothing_else() {
        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = auth ]; then echo '{{"hosts":{{"github.com":[{{"active":true,"login":"tester","state":"success"}}]}}}}'; exit 0; fi
if [ "$1" = pr ] && [ "$2" = merge ]; then exit 0; fi
echo unexpected "$@" >&2
exit 3
"#,
                log = log.display()
            ),
        );
        let binary = shim_dir.path().join("gh");
        let dir = TempDir::new().unwrap();

        // The merge runner refuses everything that is not `gh pr merge`,
        // GraphQL included.
        for argv in [
            &["pr", "view"][..],
            &["pr", "create"][..],
            &["api", "graphql"][..],
            &["pr", "merge", "graphql"][..],
        ] {
            let refused = run_gh_user_merge(dir.path(), &binary, argv, GH_TIMEOUT)
                .await
                .unwrap_err();
            assert!(
                refused.contains("only runs gh pr merge") || refused.contains("refusing"),
                "{argv:?}: {refused}"
            );
        }
        assert!(!log.exists(), "a refused argv must never spawn gh");

        // The dedicated merge operation is the one path that runs it.
        let cache = PrDigestCache::default();
        merge_pull_request(
            dir.path(),
            WorkspaceId::new(),
            MergeMethod::Squash,
            false,
            &cache,
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
        merge_pull_request(
            dir.path(),
            WorkspaceId::new(),
            MergeMethod::Merge,
            true,
            &cache,
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("pr merge --squash"), "{logged}");
        assert!(logged.contains("pr merge --merge --auto"), "{logged}");
    }

    #[tokio::test]
    async fn repository_merge_matches_the_reviewed_head_atomically() {
        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = auth ]; then echo '{{"hosts":{{"github.com":[{{"active":true,"login":"tester","state":"success"}}]}}}}'; exit 0; fi
if [ "$1" = pr ] && [ "$2" = merge ]; then exit 0; fi
exit 3
"#,
                log = log.display()
            ),
        );

        merge_pull_request_target(
            "github.com",
            "acme",
            "app",
            42,
            MergeMethod::Squash,
            false,
            "abcdef123456",
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();

        let logged = std::fs::read_to_string(log).unwrap();
        assert!(
            logged
                .contains("pr merge 42 --repo acme/app --squash --match-head-commit abcdef123456"),
            "{logged}"
        );
        assert_eq!(logged.matches("pr merge").count(), 1, "{logged}");
        assert!(!logged.contains("pr view"), "{logged}");
    }

    #[test]
    fn merge_failures_map_to_blocked_signed_out_or_user() {
        for blocked in [
            "X Pull request #12 is not mergeable: the base branch policy prohibits the merge.",
            "GraphQL: Pull request is not mergeable (mergePullRequest)",
            "X this branch has merge conflicts with the base branch",
            "X 2 reviews are required by reviewers with write access",
            // "author" contains "auth"; the sign-out markers must not eat it.
            "X review is required from someone other than the pull request author",
        ] {
            assert!(
                matches!(
                    classify_merge_error(blocked.into()),
                    GhError::MergeBlocked(_)
                ),
                "expected MergeBlocked for {blocked}"
            );
        }
        assert!(matches!(
            classify_merge_error("HTTP 401: authentication required".into()),
            GhError::GhSignedOut { .. }
        ));
        assert!(matches!(
            classify_merge_error("Head branch was modified. Review and try again.".into()),
            GhError::User(message) if message.starts_with(PR_HEAD_CHANGED_PREFIX)
        ));
        assert!(matches!(
            classify_merge_error("gh pr merge timed out".into()),
            GhError::User(message) if message.starts_with(GH_UNAVAILABLE_PREFIX)
        ));
        assert!(matches!(
            classify_merge_error("the author of this pull request cannot approve it".into(),),
            GhError::User(_)
        ));
        assert!(matches!(
            classify_merge_error("something else entirely".into()),
            GhError::User(_)
        ));
    }

    #[test]
    fn rich_pr_view_json_maps_to_digest() {
        let json = r#"{
            "number": 12,
            "url": "https://github.com/example/demo/pull/12",
            "state": "OPEN",
            "title": "feat: first change",
            "isDraft": true,
            "reviewDecision": "CHANGES_REQUESTED",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "BLOCKED",
            "autoMergeRequest": {"enabledAt": "2026-08-17T00:00:00Z"},
            "headRefName": "tidebreak/first-change",
            "headRefOid": "aaaaaaaa",
            "baseRefName": "main"
        }"#;
        let digest = digest_from_view_json(json, "lint\tpass\t1s\n").unwrap();
        assert_eq!(digest.number, 12);
        assert_eq!(digest.state, "open");
        assert_eq!(digest.draft, Some(true));
        assert_eq!(digest.merged, Some(false));
        assert_eq!(digest.review_decision.as_deref(), Some("changes_requested"));
        assert_eq!(digest.mergeable.as_deref(), Some("mergeable"));
        assert_eq!(digest.merge_state_status.as_deref(), Some("blocked"));
        assert_eq!(
            digest.head_branch.as_deref(),
            Some("tidebreak/first-change")
        );
        assert_eq!(digest.base_branch.as_deref(), Some("main"));
        assert_eq!(digest.auto_merge_enabled, Some(true));
        assert_eq!(digest.in_merge_queue, None);
        assert_eq!(
            digest.checks_summary.as_deref(),
            Some("1 passing, 0 pending, 0 failing")
        );

        // Older gh output without the extra fields still yields a digest, and
        // a merged state is reflected in both `state` and `merged`.
        let sparse = digest_from_view_json(
            r#"{"number": 7, "state": "MERGED", "autoMergeRequest": null}"#,
            "",
        )
        .unwrap();
        assert_eq!(sparse.state, "merged");
        assert_eq!(sparse.merged, Some(true));
        assert_eq!(sparse.draft, None);
        assert_eq!(sparse.review_decision, None);
        assert_eq!(sparse.auto_merge_enabled, Some(false));
        assert_eq!(sparse.in_merge_queue, None);
        assert!(digest_from_view_json("not json", "").is_none());
    }

    #[test]
    fn pr_view_snapshot_retains_head_oid_for_consistency_checks() {
        let snapshot = pr_view_snapshot_from_json(
            r#"{
                "number": 12,
                "state": "OPEN",
                "mergeable": "MERGEABLE",
                "mergeStateStatus": "CLEAN",
                "headRefOid": "abcdef123456"
            }"#,
            "lint\tpass\t1s\n",
        )
        .unwrap();

        assert_eq!(snapshot.head_oid.as_deref(), Some("abcdef123456"));
        assert_eq!(snapshot.digest.mergeable.as_deref(), Some("mergeable"));
        assert_eq!(
            snapshot.digest.checks_summary.as_deref(),
            Some("1 passing, 0 pending, 0 failing")
        );
    }

    #[tokio::test]
    async fn pr_digest_retries_when_the_head_changes_during_auxiliary_reads() {
        let work = TempDir::new().unwrap();
        let shim_dir = TempDir::new().unwrap();
        let view_count = shim_dir.path().join("view-count");
        let checks_count = shim_dir.path().join("checks-count");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  count=0
  if [ -f {view_count} ]; then count=$(sed -n '1p' {view_count}); fi
  count=$((count + 1))
  printf '%s\n' "$count" > {view_count}
  if [ "$count" -eq 1 ]; then
    echo '{{"number":12,"state":"OPEN","title":"old head","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","headRefOid":"aaaaaaaa"}}'
  else
    echo '{{"number":12,"state":"OPEN","title":"new head","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","headRefOid":"bbbbbbbb"}}'
  fi
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = checks ]; then
  count=0
  if [ -f {checks_count} ]; then count=$(sed -n '1p' {checks_count}); fi
  count=$((count + 1))
  printf '%s\n' "$count" > {checks_count}
  if [ "$count" -eq 1 ]; then
    printf 'old-head-check\tpass\t1s\thttps://example.test/old\n'
  else
    printf 'new-head-check\tfail\t1s\thttps://example.test/new\n'
  fi
  exit 0
fi
if [ "$1" = api ]; then
  echo added_to_merge_queue
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
                view_count = shell_single_quote(&view_count.to_string_lossy()),
                checks_count = shell_single_quote(&checks_count.to_string_lossy()),
            ),
        );
        let gh = GhObservation {
            found: true,
            authenticated: Some(true),
            viewer_login: None,
            binary: Some(shim_dir.path().join("gh")),
            remediation: String::new(),
        };

        let digest = load_pr_digest(work.path(), &gh, None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(digest.title.as_deref(), Some("new head"));
        assert_eq!(digest.mergeable.as_deref(), Some("conflicting"));
        assert_eq!(digest.merge_state_status.as_deref(), Some("dirty"));
        assert_eq!(
            digest.checks_summary.as_deref(),
            Some("0 passing, 0 pending, 1 failing")
        );
        assert_eq!(
            digest
                .checks
                .as_deref()
                .and_then(|checks| checks.first())
                .map(|check| check.name.as_str()),
            Some("new-head-check")
        );
        assert_eq!(digest.in_merge_queue, Some(true));
        assert_eq!(std::fs::read_to_string(view_count).unwrap().trim(), "3");
        assert_eq!(std::fs::read_to_string(checks_count).unwrap().trim(), "2");
    }

    #[tokio::test]
    async fn pr_digest_stays_conservative_when_the_head_keeps_changing() {
        let work = TempDir::new().unwrap();
        let shim_dir = TempDir::new().unwrap();
        let view_count = shim_dir.path().join("view-count");
        write_executable(
            &shim_dir.path().join("gh"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  count=0
  if [ -f {view_count} ]; then count=$(sed -n '1p' {view_count}); fi
  count=$((count + 1))
  printf '%s\n' "$count" > {view_count}
  echo "{{\"number\":12,\"state\":\"OPEN\",\"title\":\"head $count\",\"mergeable\":\"MERGEABLE\",\"mergeStateStatus\":\"CLEAN\",\"headRefOid\":\"head-$count\"}}"
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = checks ]; then
  printf 'lint\tpass\t1s\thttps://example.test/lint\n'
  exit 0
fi
if [ "$1" = api ]; then
  echo added_to_merge_queue
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
                view_count = shell_single_quote(&view_count.to_string_lossy()),
            ),
        );
        let gh = GhObservation {
            found: true,
            authenticated: Some(true),
            viewer_login: None,
            binary: Some(shim_dir.path().join("gh")),
            remediation: String::new(),
        };

        let digest = load_pr_digest(work.path(), &gh, None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(digest.title.as_deref(), Some("head 3"));
        assert_eq!(digest.mergeable, None);
        assert_eq!(digest.merge_state_status, None);
        assert_eq!(digest.checks_summary, None);
        assert_eq!(digest.checks, None);
        assert_eq!(digest.in_merge_queue, None);
        assert_eq!(std::fs::read_to_string(view_count).unwrap().trim(), "3");
    }

    #[test]
    fn merge_queue_timeline_uses_the_latest_transition() {
        assert!(in_merge_queue_from_timeline_events(
            "added_to_merge_queue\n"
        ));
        assert!(!in_merge_queue_from_timeline_events(
            "added_to_merge_queue\nremoved_from_merge_queue\n"
        ));
        assert!(in_merge_queue_from_timeline_events(
            "removed_from_merge_queue\nadded_to_merge_queue\n"
        ));
        assert!(!in_merge_queue_from_timeline_events(""));
    }

    #[test]
    fn pr_comments_parse_issue_review_and_inline_shapes() {
        use tidebreak_core::PullRequestCommentKind;

        let view = r#"{
            "number": 12,
            "comments": [
                {"id": "IC_kwDO1", "author": {"login": "alice", "avatarUrl": "https://avatars.githubusercontent.com/u/1"}, "url": "https://github.com/example/app/pull/12#issuecomment-1", "createdAt": "2026-08-16T10:00:00Z", "body": "looks close"},
                {"body": ""}
            ],
            "reviews": [
                {"author": {"login": "bob"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-08-16T11:00:00Z", "body": "please split this"},
                {"author": {"login": "carol"}, "state": "APPROVED", "body": ""}
            ]
        }"#;
        let (number, comments) = parse_pr_view_comments(view);
        assert_eq!(number, Some(12));
        assert_eq!(comments.len(), 2, "empty bodies are dropped: {comments:?}");
        assert_eq!(comments[0].kind, PullRequestCommentKind::Issue);
        assert_eq!(comments[0].id.as_deref(), Some("IC_kwDO1"));
        assert_eq!(comments[0].author.as_deref(), Some("alice"));
        assert_eq!(
            comments[0].avatar_url.as_deref(),
            Some("https://avatars.githubusercontent.com/u/1")
        );
        assert_eq!(
            comments[0].url.as_deref(),
            Some("https://github.com/example/app/pull/12#issuecomment-1")
        );
        assert_eq!(comments[0].body, "looks close");
        assert_eq!(comments[1].kind, PullRequestCommentKind::Review);
        assert_eq!(
            comments[1].review_state.as_deref(),
            Some("changes_requested")
        );
        assert_eq!(
            comments[1].created_at.as_deref(),
            Some("2026-08-16T11:00:00Z")
        );

        let rest = r#"[
            {"id": 99, "html_url": "https://github.com/example/app/pull/12#discussion_r99", "user": {"login": "bob", "avatar_url": "https://avatars.githubusercontent.com/u/2"}, "created_at": "2026-08-16T12:00:00Z", "body": "rename this", "path": "src/lib.rs", "line": 42},
            {"user": {"login": "bob"}, "body": "outdated hunk", "path": "src/old.rs", "line": null, "original_line": 7},
            {"body": ""}
        ]"#;
        let inline = parse_review_comments(rest);
        assert_eq!(inline.len(), 2);
        assert_eq!(inline[0].kind, PullRequestCommentKind::Inline);
        assert_eq!(inline[0].id.as_deref(), Some("99"));
        assert_eq!(
            inline[0].url.as_deref(),
            Some("https://github.com/example/app/pull/12#discussion_r99")
        );
        assert_eq!(inline[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(inline[0].line, Some(42));
        assert_eq!(inline[1].line, Some(7), "falls back to original_line");
        assert_eq!(parse_review_comments("surprise!").len(), 0);
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

    #[tokio::test]
    async fn resolve_github_uses_gh_url_when_signed_in() {
        let shim_dir = TempDir::new().unwrap();
        write_executable(
            &shim_dir.path().join("gh"),
            r#"#!/bin/sh
if [ "$1" = auth ]; then echo '{"hosts":{"github.com":[{"active":true,"login":"tester","state":"success"}]}}'; exit 0; fi
if [ "$1" = repo ] && [ "$2" = view ]; then
  echo '{"url":"https://github.com/acme/demo"}'
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
        );
        let url = resolve_github_clone_url("acme/demo", Some(shim_dir.path().to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(url, "https://github.com/acme/demo.git");
    }

    #[tokio::test]
    async fn resolve_github_constructs_https_when_gh_is_absent() {
        let empty = TempDir::new().unwrap();
        let url = resolve_github_clone_url("acme/demo", Some(empty.path().to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(url, "https://github.com/acme/demo.git");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use tidebreak_harness::ProcessTreeChild;
    use tokio::time::{sleep, timeout, Instant};
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    const DESCENDANT_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

    fn assert_windows_path_eq(actual: Option<PathBuf>, expected: &Path) {
        let actual = actual.expect("expected an executable path");
        assert!(
            actual
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy()),
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn github_cli_discovery_applies_windows_executable_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh.exe");
        std::fs::write(&gh, b"synthetic executable").unwrap();
        let search_path = std::env::join_paths([dir.path()]).unwrap();

        assert_windows_path_eq(find_gh(search_path.to_str()), &gh);
    }

    #[test]
    fn github_cli_discovery_skips_unlaunchable_pathext_entries() {
        let unlaunchable = tempfile::tempdir().unwrap();
        std::fs::write(unlaunchable.path().join("gh.ps1"), b"Write-Output wrong").unwrap();
        let installed = tempfile::tempdir().unwrap();
        let gh = installed.path().join("gh.exe");
        std::fs::write(&gh, b"synthetic executable").unwrap();
        let search_path = std::env::join_paths([unlaunchable.path(), installed.path()]).unwrap();

        assert_windows_path_eq(
            find_windows_gh(&search_path, Some(OsStr::new(".PS1;.EXE"))),
            &gh,
        );
    }

    #[test]
    fn github_cli_discovery_skips_batch_shims_that_reject_multiline_bodies() {
        let batch_shim = tempfile::tempdir().unwrap();
        std::fs::write(batch_shim.path().join("gh.cmd"), b"@echo wrong\r\n").unwrap();
        let installed = tempfile::tempdir().unwrap();
        let gh = installed.path().join("gh.exe");
        std::fs::write(&gh, b"synthetic executable").unwrap();
        let search_path = std::env::join_paths([batch_shim.path(), installed.path()]).unwrap();

        assert_windows_path_eq(
            find_windows_gh(&search_path, Some(OsStr::new(".CMD;.EXE"))),
            &gh,
        );
    }

    #[test]
    fn manual_pr_instructions_use_powershell_quoting() {
        let gh = GhObservation {
            found: false,
            authenticated: None,
            viewer_login: None,
            binary: None,
            remediation: "install gh".into(),
        };
        let instructions = manual_pr_instructions(
            Path::new(r"C:\Users\Jane Doe\repo"),
            "tidebreak/jane's-fix",
            "Jane's fix",
            Some("It's ready"),
            &Diffstat {
                files: 1,
                insertions: 2,
                deletions: 0,
                truncated: false,
            },
            &gh,
        );

        assert!(instructions.contains(r"Set-Location -LiteralPath 'C:\Users\Jane Doe\repo'"));
        assert!(instructions.contains("git push -u origin 'tidebreak/jane''s-fix'"));
        assert!(instructions.contains("--title 'Jane''s fix'"));
        assert!(instructions.contains("--body 'It''s ready'"));
        assert!(!instructions.contains("'\\''"));
    }

    #[tokio::test]
    async fn quick_action_timeout_terminates_its_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        // `run_action` starts its 5s budget at spawn. Waiting for the pid after
        // that races PowerShell startup on a loaded runner. Spawn the same way,
        // then cancel `wait_with_output` after the descendant exists.
        let command = format!(
            "$child = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\\ping.exe') \
             -ArgumentList @('-t','127.0.0.1') -WindowStyle Hidden -PassThru; \
             [IO.File]::WriteAllText({}, $child.Id.ToString(), [Text.UTF8Encoding]::new($false)); \
             Wait-Process -Id $child.Id",
            powershell_single_quote(&pid_file.to_string_lossy())
        );
        let mut child =
            super::spawn_workspace_script(dir.path(), &command).expect("spawn quick action");
        let pid = wait_for_pid(&pid_file, &mut child).await;
        let descendant = open_process(pid);
        assert_eq!(
            wait_status(&descendant, Duration::ZERO),
            WAIT_TIMEOUT,
            "descendant {pid} exited before the quick-action timeout"
        );

        assert!(
            timeout(ACTION_TIMEOUT, child.wait_with_output())
                .await
                .is_err(),
            "quick action finished before the timeout fired"
        );
        assert_eq!(
            wait_status(&descendant, DESCENDANT_EXIT_TIMEOUT),
            WAIT_OBJECT_0,
            "descendant {pid} remained alive after quick-action job teardown"
        );
    }

    async fn wait_for_pid(path: &Path, child: &mut ProcessTreeChild) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = value.trim().trim_start_matches('\u{feff}').trim().parse() {
                    return pid;
                }
            }
            assert!(
                child.try_wait().ok().flatten().is_none(),
                "quick action ended before publishing its descendant pid"
            );
            assert!(
                Instant::now() < deadline,
                "quick action exceeded its timeout without publishing its descendant pid"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn open_process(pid: u32) -> OwnedHandle {
        // SAFETY: synchronization access is requested for the descendant pid
        // that the quick action just created and published.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!raw.is_null(), "could not open descendant process {pid}");
        // SAFETY: successful OpenProcess transfers one owned handle.
        unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
    }

    fn wait_status(process: &OwnedHandle, limit: Duration) -> u32 {
        // SAFETY: `process` is a live synchronization handle and remains owned
        // for the duration of the bounded wait.
        unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, limit.as_millis() as u32) }
    }
}
