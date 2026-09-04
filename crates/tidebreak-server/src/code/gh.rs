//! Git commit/push and `gh` pull-request operations for a workspace.
//!
//! Every subprocess is bounded and non-interactive. Arguments are an argv
//! array, never a shell string. `gh` credentials are observed, never stored;
//! a hosted machine instead lends each git operation a dying, gateway-minted
//! credential through the environment (decision 63).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use tidebreak_core::{Diffstat, PullRequestDigest, QuickAction};
use tidebreak_harness::{filter_child_env, probe_shell, HostEnv, OutputBudget};

use super::setup_script::{missing_image_toolchain_notice, spawn_workspace_script};
use crate::code::types::CodeGitHubRepositoryTarget;
use crate::obo_gateway::GitCredential;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(120);
const GH_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_CHARS: usize = 4_096;
const MAX_ACTION_OUTPUT_BYTES: usize = 4_096;
const MAX_ACTION_OUTPUT_LINES: usize = 256;
const GH_OBSERVATION_TTL: Duration = Duration::from_secs(30);
pub const GH_UNAVAILABLE_PREFIX: &str = "gh_unavailable: ";
pub const PR_HEAD_CHANGED_PREFIX: &str = "pr_head_changed: ";

/// Environment variables the one-shot credential helper reads a borrowed
/// credential from. The environment, not argv: another user's `ps` can read
/// a process's arguments, not its environment.
pub const GIT_CREDENTIAL_USERNAME_ENV: &str = "TIDEBREAK_GIT_CREDENTIAL_USERNAME";
pub const GIT_CREDENTIAL_SECRET_ENV: &str = "TIDEBREAK_GIT_CREDENTIAL_SECRET";

/// The one host the one-shot helper may answer for, set beside the pair.
pub const GIT_CREDENTIAL_HOST_ENV: &str = "TIDEBREAK_GIT_CREDENTIAL_HOST";

/// The forge host a borrowed credential is confined to (decision 63).
///
/// One value for v1 because the gateway mints from exactly one GitHub App;
/// a GHES forge would thread its own host through here.
pub const GIT_CREDENTIAL_FORGE_HOST: &str = "github.com";

/// Configuration that lends one borrowed credential to one git subprocess
/// (decision 63).
///
/// The empty helper first resets any inherited helper list. That is
/// load-bearing twice over: no configured helper can answer ahead of the
/// borrowed credential, and — because git offers a successful credential to
/// every helper's `store` — no helper like `git-credential-store` can write
/// the dying token to disk.
///
/// The one-shot helper then answers `get` from the environment — but only
/// for `https` and only for the exact host named in
/// [`GIT_CREDENTIAL_HOST_ENV`]. Git re-asks its helpers whenever a fetch or
/// push needs another host's credential — a rewritten `origin`, a redirect —
/// and without the check the environment pair would be offered to whatever
/// host asked. A mismatched description reads to git as "no credential",
/// never as an error.
pub const GIT_CREDENTIAL_CONFIG_ARGS: [&str; 4] = [
    "-c",
    "credential.helper=",
    "-c",
    "credential.helper=!f() { h=; p=; while IFS= read -r line; do case \"$line\" in host=*) if [ \"${line#host=}\" = \"$TIDEBREAK_GIT_CREDENTIAL_HOST\" ]; then h=1; fi ;; protocol=https) p=1 ;; esac; done; if [ \"$1\" = get ] && [ -n \"$h\" ] && [ -n \"$p\" ]; then printf 'username=%s\\npassword=%s\\n' \"$TIDEBREAK_GIT_CREDENTIAL_USERNAME\" \"$TIDEBREAK_GIT_CREDENTIAL_SECRET\"; fi; }; f",
];

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

/// Failure from a git, `gh`, or quick-action operation.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
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
pub struct CommitOutcome {
    pub sha: String,
    pub message: String,
    pub stat: Diffstat,
}

/// Result of pushing the workspace branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub branch: String,
    pub remote: String,
}

/// Names `name <email>` as this workspace's git author and committer
/// (decision 65).
///
/// `extensions.worktreeConfig` is enabled first so the identity lands in the
/// worktree's own configuration: sibling workspaces of the same clone can
/// belong to callers with other identities, and the shared repository
/// configuration must never name any one of them.
pub async fn configure_workspace_identity(
    worktree: &Path,
    name: &str,
    email: &str,
) -> Result<(), String> {
    git(
        worktree,
        &["config", "extensions.worktreeConfig", "true"],
        GIT_TIMEOUT,
    )
    .await?;
    git(
        worktree,
        &["config", "--worktree", "user.name", name],
        GIT_TIMEOUT,
    )
    .await?;
    git(
        worktree,
        &["config", "--worktree", "user.email", email],
        GIT_TIMEOUT,
    )
    .await?;
    Ok(())
}

/// Live git + `gh` observation for the workspace PR card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGitStatus {
    pub dirty: bool,
    pub unpushed: bool,
    pub ahead: u64,
    pub has_upstream: bool,
    pub suggested_commit_message: String,
    pub pr: Option<PullRequestDigest>,
    pub gh_found: bool,
    pub gh_authenticated: Option<bool>,
    pub remediation: String,
    /// The identity a push from this machine acts as, when git does not
    /// speak for whoever configured it: the deployment's GitHub App bot
    /// account (decision 63), or the caller's own login (decision 65).
    /// `None` on every machine where git speaks for whoever configured it.
    pub pushes_as: Option<String>,
    /// `Some(true)` when `pushes_as` is the caller's own account
    /// (decision 65) rather than the deployment's App.
    pub pushes_as_self: Option<bool>,
}

/// Mutable local facts that must still hold when a workspace merge reaches
/// the host. The runtime reads them while it owns the workspace turn lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMergeLocalState {
    pub current_branch: Option<String>,
    pub head_sha: String,
    pub dirty: bool,
    pub upstream: Option<String>,
    pub ahead_of_upstream: u64,
}

/// The pull request `gh` resolves from the locked workspace branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePullRequestIdentity {
    pub target: CodeGitHubRepositoryTarget,
    pub number: u64,
    pub state: String,
    pub head_branch: String,
    pub head_sha: String,
}

/// Outcome of one named quick action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
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
pub async fn resolve_github_clone_url(
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
pub struct GhObservation {
    pub found: bool,
    pub authenticated: Option<bool>,
    pub viewer_login: Option<String>,
    pub binary: Option<PathBuf>,
    pub remediation: String,
}

/// Stage every change in `worktree` and create one commit.
pub async fn commit_all(
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
///
/// `credential` is a borrowed, repository-scoped forge credential for a
/// hosted machine (decision 63), lent to this one subprocess and dropped.
/// `None` is every other machine: git authenticates however the operator's
/// own configuration says.
pub async fn push_branch(
    worktree: &Path,
    branch: &str,
    credential: Option<&GitCredential>,
) -> Result<PushOutcome, GhError> {
    let mut args: Vec<&str> = Vec::new();
    if credential.is_some() {
        args.extend(GIT_CREDENTIAL_CONFIG_ARGS);
    }
    args.extend(["push", "-u", "origin", branch]);
    git_with_credential(worktree, &args, GIT_PUSH_TIMEOUT, credential)
        .await
        .map_err(|err| classify_git(err, "push"))?;
    Ok(PushOutcome {
        branch: branch.to_owned(),
        remote: "origin".into(),
    })
}

/// Inspect the worktree and, when `gh` can, refresh the PR digest.
#[allow(clippy::too_many_arguments)]
pub async fn workspace_git_status(
    worktree: &Path,
    title: &str,
    branch: &str,
    base_ref: &str,
    persisted: Option<PullRequestDigest>,
    gh_search_path: Option<&str>,
) -> Result<WorkspaceGitStatus, GhError> {
    let inspect = inspect_git(worktree, base_ref, title).await?;
    let gh = observe_gh(gh_search_path).await;
    // The digest itself comes from the conditional fetcher (decision 66),
    // which the runtime drives with the fact row's ETags; this observation
    // only carries the persisted copy and the local git state.
    Ok(WorkspaceGitStatus {
        dirty: inspect.dirty,
        unpushed: inspect.unpushed,
        ahead: inspect.ahead,
        has_upstream: inspect.has_upstream,
        suggested_commit_message: inspect.suggested_commit_message,
        pr: persisted,
        gh_found: gh.found,
        gh_authenticated: gh.authenticated,
        remediation: if gh.found && gh.authenticated == Some(true) {
            String::new()
        } else {
            manual_pr_instructions(worktree, branch, title, None, &inspect.diffstat, &gh)
        },
        // Decorated by the runtime, which knows whether this machine lends
        // gateway credentials; this module only observes local state.
        pushes_as: None,
        pushes_as_self: None,
    })
}

/// The `gh` binary to drive reads with, when one is present and signed in.
pub async fn authenticated_gh_binary(search_path: Option<&str>) -> Option<PathBuf> {
    let gh = observe_gh(search_path).await;
    (gh.found && gh.authenticated == Some(true))
        .then_some(gh.binary)
        .flatten()
}

/// Create a pull request from the workspace branch. Never merges.
#[allow(clippy::too_many_arguments)]
pub async fn create_pull_request(
    worktree: &Path,
    title: &str,
    branch: &str,
    base_ref: &str,
    requested_title: Option<&str>,
    requested_body: Option<&str>,
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
    // A light digest from the creation answer. The caller's next status
    // read enriches it through the conditional fetcher (decision 66), which
    // needs exactly this URL to name the pull request.
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
        check_counts: None,
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
    Ok(digest)
}

/// [`create_pull_request`] over the forge REST API with a borrowed
/// credential (decision 65), for machines without `gh`.
///
/// The title and body come from exactly the same generation as the `gh`
/// path, the created pull request comes back beside the digest in the fact
/// shape so the caller can persist the authored fact without a second host
/// read, and the digest read after creation is best-effort exactly as it is
/// with `gh` — a light digest from the creation answer serves until the next
/// status read.
#[allow(clippy::too_many_arguments)]
pub async fn create_pull_request_rest(
    worktree: &Path,
    title: &str,
    branch: &str,
    base_ref: &str,
    requested_title: Option<&str>,
    requested_body: Option<&str>,
    api_base: &str,
    target: &crate::code::types::CodeGitHubRepositoryTarget,
    credential: &GitCredential,
) -> Result<(PullRequestDigest, serde_json::Value), GhError> {
    let inspect = inspect_git(worktree, base_ref, title).await?;
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
    let base = gh_base_branch(base_ref);
    let fact = super::forge_rest::create_pull_request(
        api_base, target, credential, &pr_title, &pr_body, base, branch,
    )
    .await
    .map_err(|reason| GhError::user(format!("the pull request was not created: {reason}")))?;
    if let Ok(Some(digest)) =
        super::forge_rest::pull_request_digest(api_base, target, credential, branch).await
    {
        return Ok((digest, fact));
    }
    let number = fact.get("number").and_then(serde_json::Value::as_u64);
    let digest = PullRequestDigest {
        number: number.unwrap_or(0),
        url: fact
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        state: "open".into(),
        title: fact
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        checks_summary: None,
        check_counts: None,
        checks: None,
        draft: fact.get("isDraft").and_then(serde_json::Value::as_bool),
        merged: Some(false),
        review_decision: None,
        mergeable: None,
        merge_state_status: None,
        head_branch: fact
            .get("headRefName")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        base_branch: fact
            .get("baseRefName")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        head_sha: fact
            .get("headRefOid")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        auto_merge_enabled: None,
        in_merge_queue: None,
    };
    Ok((digest, fact))
}

/// Merge strategy for the user-initiated merge operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
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
pub async fn mark_workspace_pull_request_ready(
    worktree: &Path,
    gh_search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(gh_search_path).await;
    let binary = require_gh_binary(&observation)?;
    run_gh(worktree, &binary, &["pr", "ready"], GH_TIMEOUT)
        .await
        .map_err(|error| classify_observed_gh(error, &observation))?;
    Ok(())
}

/// Turn a `gh pr merge` failure into something the PR card can show.
pub fn classify_merge_error(err: String) -> GhError {
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
pub struct PrComments {
    pub number: u64,
    pub comments: Vec<tidebreak_core::PullRequestComment>,
}

/// Load issue comments, review bodies, and inline review comments for the
/// workspace PR. Inline comments come from the REST endpoint because
/// `gh pr view --json` does not carry file/line positions.
pub async fn load_pr_comments(
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

pub fn require_gh_binary(gh: &GhObservation) -> Result<PathBuf, GhError> {
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
pub fn parse_pr_view_comments(
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
pub fn parse_review_comments(json: &str) -> Vec<tidebreak_core::PullRequestComment> {
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
pub async fn run_named_action(
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
pub async fn run_auto_create_actions(worktree: &Path, actions: &[QuickAction]) {
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
pub fn generate_commit_message(title: &str, stat: &Diffstat) -> String {
    let subject = title.trim();
    let subject = if subject.is_empty() {
        "Update workspace"
    } else {
        subject
    };
    format!("{subject}\n\n{}", format_shortstat(stat))
}

pub fn generate_pr_title(title: &str, branch: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        branch.rsplit('/').next().unwrap_or(branch).to_owned()
    } else {
        title.to_owned()
    }
}

pub fn generate_pr_body(commits: &[String], stat: &Diffstat) -> String {
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
pub fn gh_base_branch(base_ref: &str) -> &str {
    let trimmed = base_ref.trim();
    trimmed
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| trimmed.strip_prefix("origin/"))
        .unwrap_or(trimmed)
}

pub fn format_shortstat(stat: &Diffstat) -> String {
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

/// Read the local preconditions for merging a workspace pull request.
///
/// The caller owns the workspace turn lock for this read and the host action
/// that follows. A missing upstream and a detached head stay distinct so the
/// route can return a specific typed conflict.
pub async fn inspect_workspace_merge_local_state(
    worktree: &Path,
) -> Result<WorkspaceMergeLocalState, GhError> {
    let current_branch = git(
        worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        GIT_TIMEOUT,
    )
    .await
    .ok()
    .filter(|branch| !branch.is_empty());
    let head_sha = git(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT).await?;
    let dirty = has_uncommitted_work(worktree).await?;
    let upstream = git(
        worktree,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        GIT_TIMEOUT,
    )
    .await
    .ok()
    .filter(|upstream| !upstream.is_empty());
    let ahead_of_upstream = match upstream {
        Some(_) => parse_count(
            &git(
                worktree,
                &["rev-list", "--count", "@{u}..HEAD"],
                GIT_TIMEOUT,
            )
            .await?,
        ),
        None => 0,
    };
    Ok(WorkspaceMergeLocalState {
        current_branch,
        head_sha,
        dirty,
        upstream,
        ahead_of_upstream,
    })
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

pub fn parse_shortstat(text: &str) -> Diffstat {
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

pub async fn observe_gh(search_path: Option<&str>) -> GhObservation {
    observe_gh_with_cache(search_path, false).await
}

pub async fn refresh_gh_observation(search_path: Option<&str>) -> GhObservation {
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
        None => match probe_shell(&HostEnv::from_process(), "gh").await {
            Ok(capture) => GhLaunch {
                binary: capture.binary,
                login_env: Some(Arc::new(capture.env)),
            },
            // Homebrew installs after PATH and login-shell probing. GitHub's
            // macOS runners ship `/opt/homebrew/bin/gh`; looking there first
            // skips the probe the packaged-app smoke test exists to prove.
            Err(_) => GhLaunch {
                binary: well_known_gh()?,
                login_env: None,
            },
        },
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

fn well_known_gh() -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        [
            PathBuf::from("/opt/homebrew/bin/gh"),
            PathBuf::from("/usr/local/bin/gh"),
        ]
        .into_iter()
        .find(|candidate| is_executable(candidate))
    }
    #[cfg(windows)]
    {
        None
    }
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
    match timeout(
        ACTION_TIMEOUT,
        child.wait_with_bounded_output(
            OutputBudget::head(MAX_ACTION_OUTPUT_BYTES, MAX_ACTION_OUTPUT_LINES),
            OutputBudget::head(MAX_ACTION_OUTPUT_BYTES, MAX_ACTION_OUTPUT_LINES),
            true,
        ),
    )
    .await
    {
        Ok(Ok(output)) => {
            let stdout = output.stdout.into_marked_text();
            let mut stderr = output.stderr.into_marked_text();
            if let Some(notice) = missing_image_toolchain_notice(&stderr)
                .or_else(|| missing_image_toolchain_notice(&stdout))
            {
                if !stderr.is_empty() && !stderr.ends_with('\n') && !stderr.ends_with(' ') {
                    stderr.push(' ');
                }
                stderr.push_str(&notice);
            }
            ActionOutcome {
                name: action.name.clone(),
                success: output.status.success() && !output.terminated_for_output,
                exit_code: output.status.code(),
                stdout,
                stderr,
                timed_out: false,
            }
        }
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
    git_with_credential(cwd, args, limit, None).await
}

/// [`git`], optionally lending a borrowed credential through the environment
/// the one-shot helper in [`GIT_CREDENTIAL_CONFIG_ARGS`] reads.
async fn git_with_credential(
    cwd: &Path,
    args: &[&str],
    limit: Duration,
    credential: Option<&GitCredential>,
) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(credential) = credential {
        command
            .env(GIT_CREDENTIAL_USERNAME_ENV, &credential.username)
            .env(GIT_CREDENTIAL_SECRET_ENV, &credential.secret)
            .env(GIT_CREDENTIAL_HOST_ENV, GIT_CREDENTIAL_FORGE_HOST);
    }
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
pub async fn run_gh(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    if refuse_gh_args(args) {
        return Err("refusing to run a merge or GraphQL gh command".into());
    }
    spawn_gh(cwd, binary, args, limit).await
}

/// Mark one repository-qualified draft pull request ready for review.
///
/// This is a user-initiated state change, but not a merge. It stays on the
/// general runner so the merge-only runner remains incapable of doing
/// anything else.
pub async fn mark_pull_request_ready(
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
pub async fn close_pull_request_target(
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
pub async fn reopen_pull_request_target(
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
pub async fn comment_on_pull_request_target(
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
/// workspace. The runner still admits only `gh pr merge` argv. `admin` is the
/// user's explicit branch-protection bypass (`--admin`); callers reject the
/// `admin && auto` pair before it gets here.
#[allow(clippy::too_many_arguments)]
pub async fn merge_pull_request_target(
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    method: MergeMethod,
    auto: bool,
    admin: bool,
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
    if admin {
        args.push("--admin".to_owned());
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_gh_user_merge(Path::new("."), &binary, &borrowed, GH_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(classify_merge_error)
}

pub async fn rerun_failed_jobs_with_observation(
    observation: &GhObservation,
    host: &str,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GhError> {
    rerun_workflow_endpoint_with_observation(
        observation,
        host,
        owner,
        repo,
        run_id,
        "rerun-failed-jobs",
    )
    .await
}

pub async fn rerun_workflow_with_observation(
    observation: &GhObservation,
    host: &str,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GhError> {
    rerun_workflow_endpoint_with_observation(observation, host, owner, repo, run_id, "rerun").await
}

async fn rerun_workflow_endpoint_with_observation(
    observation: &GhObservation,
    host: &str,
    owner: &str,
    repo: &str,
    run_id: u64,
    action: &str,
) -> Result<(), GhError> {
    let binary = require_gh_binary(observation)?;
    let endpoint = format!("repos/{owner}/{repo}/actions/runs/{run_id}/{action}");
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

/// Register a stack of pull requests on the host (GitHub stacked pull
/// requests), from the chain's numbers, bottom to top.
///
/// The array rides `--field pull_requests[]=N` per member: that is the one
/// `gh api` spelling that serializes as a JSON array, which the stacks
/// endpoint requires.
pub async fn create_stack(
    host: &str,
    owner: &str,
    repo: &str,
    numbers: &[u64],
    search_path: Option<&str>,
) -> Result<(), GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let endpoint = format!("repos/{owner}/{repo}/stacks");
    let mut args = vec![
        "api".to_owned(),
        "--method".to_owned(),
        "POST".to_owned(),
        endpoint,
    ];
    for number in numbers {
        args.push("--field".to_owned());
        args.push(format!("pull_requests[]={number}"));
    }
    if host != "github.com" {
        args.extend(["--hostname".to_owned(), host.to_owned()]);
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_gh(Path::new("."), &binary, &borrowed, GH_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|error| classify_observed_gh(error, &observation))
}

pub fn cli_repository(host: &str, owner: &str, repo: &str) -> String {
    if host == "github.com" {
        format!("{owner}/{repo}")
    } else {
        format!("{host}/{owner}/{repo}")
    }
}

/// Fields a pull-request fact snapshot needs (decision 77). Narrower than the
/// delivery list fields: no checks, review, or mergeability — those stay
/// live-only.
pub const PR_FACT_FIELDS: &str = "number,url,title,state,isDraft,author,headRefName,headRefOid,baseRefName,createdAt,updatedAt,mergedAt,closedAt";

/// Resolve the pull request attached to the workspace's current branch.
///
/// This read deliberately carries no repository or pull request selector. The
/// runtime compares the returned URL, number, branch, and head with the exact
/// target the desktop confirmed before it calls the repository-qualified
/// merge helper.
pub async fn view_workspace_pull_request(
    worktree: &Path,
    search_path: Option<&str>,
) -> Result<WorkspacePullRequestIdentity, GhError> {
    let observation = observe_gh(search_path).await;
    let binary = require_gh_binary(&observation)?;
    let raw = run_gh(
        worktree,
        &binary,
        &["pr", "view", "--json", PR_FACT_FIELDS],
        GH_TIMEOUT,
    )
    .await
    .map_err(|error| classify_observed_gh(error, &observation))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| GhError::Internal(format!("could not parse pull request: {error}")))?;
    let number = value
        .get("number")
        .and_then(serde_json::Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| GhError::user("the pull request response did not name a pull request"))?;
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GhError::user("the pull request response did not name its repository"))?;
    let (host, owner, name, url_number) = super::pr_facts::pull_request_identity_from_url(url)
        .ok_or_else(|| GhError::user("the pull request response had an invalid URL"))?;
    if number != url_number {
        return Err(GhError::user(format!(
            "the pull request response named both #{number} and #{url_number}"
        )));
    }
    let state = value
        .get("state")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| GhError::user("the pull request response did not name its state"))?;
    let head_branch = value
        .get("headRefName")
        .and_then(serde_json::Value::as_str)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GhError::user("the pull request response did not name its head branch"))?;
    let head_sha = value
        .get("headRefOid")
        .and_then(serde_json::Value::as_str)
        .filter(|sha| !sha.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GhError::user("the pull request response did not name its head commit"))?;
    Ok(WorkspacePullRequestIdentity {
        target: CodeGitHubRepositoryTarget { host, owner, name },
        number,
        state,
        head_branch,
        head_sha,
    })
}

/// Read one repository-qualified pull request's fact fields, as raw JSON.
pub async fn view_pull_request_raw(
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
pub async fn list_pull_requests_for_head_raw(
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
    let output = spawn_gh_output(cwd, binary, login_env, args, limit).await?;
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

async fn spawn_gh_output(
    cwd: &Path,
    binary: &Path,
    login_env: Option<&[(OsString, OsString)]>,
    args: &[&str],
    limit: Duration,
) -> Result<std::process::Output, String> {
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
    timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("gh {} timed out", args.join(" ")))?
        .map_err(|err| format!("gh {} failed: {err}", args.join(" ")))
}

/// One `gh api --include` answer: the status line, the caching and pacing
/// headers the fetcher acts on, and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub retry_after_secs: Option<u64>,
    pub ratelimit_remaining: Option<u64>,
    pub ratelimit_reset_epoch: Option<u64>,
    pub body: String,
}

/// Run `gh api --include` and keep the HTTP answer whatever the exit code.
///
/// `gh api` exits non-zero for a 304 or a 4xx that is still a real answer
/// this process must read — a 304 is the conditional fetcher's cheapest
/// success, and a 403's headers say how long to park. Only a run that
/// produced no HTTP status line at all is an error here.
pub async fn run_gh_http(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<RawHttpResponse, String> {
    if refuse_gh_args(args) {
        return Err("refusing to run a merge or GraphQL gh command".into());
    }
    let login_env = GH_LAUNCH
        .get()
        .filter(|launch| launch.binary == binary)
        .and_then(|launch| launch.login_env.as_deref().map(Vec::as_slice));
    let output = spawn_gh_output(cwd, binary, login_env, args, limit).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    match parse_raw_http(&stdout) {
        Some(response) => Ok(response),
        None => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(if stderr.is_empty() {
                format!("gh {} answered no HTTP status", args.join(" "))
            } else {
                stderr
            })
        }
    }
}

fn refuse_gh_args(args: &[&str]) -> bool {
    args.iter()
        .any(|arg| *arg == "merge" || *arg == "--merge" || *arg == "--auto" || *arg == "graphql")
        || args.windows(2).any(|pair| pair == ["api", "graphql"])
}

/// Parse a `gh api --include` answer: status line, headers, blank line, body.
fn parse_raw_http(stdout: &str) -> Option<RawHttpResponse> {
    fn take_line<'a>(rest: &mut &'a str) -> &'a str {
        let end = rest.find('\n').map_or(rest.len(), |index| index + 1);
        let line = rest[..end].trim_end_matches(['\r', '\n']);
        *rest = &rest[end..];
        line
    }
    let mut rest = stdout;
    let status_line = take_line(&mut rest);
    let mut parts = status_line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;
    let mut etag = None;
    let mut retry_after_secs = None;
    let mut ratelimit_remaining = None;
    let mut ratelimit_reset_epoch = None;
    while !rest.is_empty() {
        let line = take_line(&mut rest);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("etag") {
            etag = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("retry-after") {
            retry_after_secs = value.parse().ok();
        } else if name.eq_ignore_ascii_case("x-ratelimit-remaining") {
            ratelimit_remaining = value.parse().ok();
        } else if name.eq_ignore_ascii_case("x-ratelimit-reset") {
            ratelimit_reset_epoch = value.parse().ok();
        }
    }
    Some(RawHttpResponse {
        status,
        etag,
        retry_after_secs,
        ratelimit_remaining,
        ratelimit_reset_epoch,
        body: rest.trim().to_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// The one-shot helper is real shell handed to real git: prove it
    /// answers `get` with the environment pair for the forge host over
    /// `https` — and answers nothing for any other host or protocol, so a
    /// rewritten origin or a redirect cannot walk the borrowed credential to
    /// a host an attacker controls (decision 63).
    #[test]
    fn the_one_shot_helper_answers_only_the_forge_host_over_https() {
        use std::io::Write as _;

        let fill = |description: &[u8]| {
            let dir = TempDir::new().unwrap();
            let mut command = StdCommand::new("git");
            command.args(GIT_CREDENTIAL_CONFIG_ARGS);
            command
                .args(["credential", "fill"])
                .current_dir(dir.path())
                .env(GIT_CREDENTIAL_USERNAME_ENV, "x-access-token")
                .env(GIT_CREDENTIAL_SECRET_ENV, "ghs_dying_token")
                .env(GIT_CREDENTIAL_HOST_ENV, GIT_CREDENTIAL_FORGE_HOST)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child.stdin.take().unwrap().write_all(description).unwrap();
            let output = child.wait_with_output().unwrap();
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        };

        let (ok, answered) = fill(b"protocol=https\nhost=github.com\npath=acme/demo.git\n\n");
        assert!(ok, "git credential fill failed for the forge host");
        assert!(answered.contains("username=x-access-token"), "{answered}");
        assert!(answered.contains("password=ghs_dying_token"), "{answered}");

        // Any other host — a rewritten origin, a redirect — gets nothing.
        // `fill` itself fails because no source answered and prompts are off,
        // which is exactly the refusal a push would surface.
        let (ok, answered) = fill(b"protocol=https\nhost=evil.example\npath=acme/demo.git\n\n");
        assert!(!ok, "a foreign host must not fill");
        assert!(!answered.contains("ghs_dying_token"), "{answered}");

        let (ok, answered) = fill(b"protocol=http\nhost=github.com\npath=acme/demo.git\n\n");
        assert!(!ok, "cleartext must not fill");
        assert!(!answered.contains("ghs_dying_token"), "{answered}");
    }

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
    fn raw_http_answers_parse_status_headers_and_body() {
        let ok = parse_raw_http(
            "HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nX-Ratelimit-Remaining: 4200\r\n\r\n{\"number\":12}\n",
        )
        .unwrap();
        assert_eq!(ok.status, 200);
        assert_eq!(ok.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(ok.ratelimit_remaining, Some(4200));
        assert_eq!(ok.body, "{\"number\":12}");

        let not_modified =
            parse_raw_http("HTTP/2.0 304 Not Modified\r\nEtag: W/\"abc\"\r\n\r\n").unwrap();
        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body.is_empty());

        let parked = parse_raw_http(
            "HTTP/1.1 403 Forbidden\nRetry-After: 90\n\n{\"message\":\"You have exceeded a secondary rate limit\"}",
        )
        .unwrap();
        assert_eq!(parked.status, 403);
        assert_eq!(parked.retry_after_secs, Some(90));

        assert!(parse_raw_http("gh: command not found").is_none());
        assert!(parse_raw_http("").is_none());
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
        let err = push_branch(&work, "tidebreak/push-fail", None)
            .await
            .unwrap_err();
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
        let pushed = push_branch(&work, "tidebreak/first-change", None)
            .await
            .unwrap();
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

        let (first, second, all) = tokio::join!(
            rerun_failed_jobs_with_observation(&observation, "github.com", "acme", "app", 10,),
            rerun_failed_jobs_with_observation(&observation, "github.com", "acme", "app", 11,),
            rerun_workflow_with_observation(&observation, "github.com", "acme", "app", 12,),
        );
        first.unwrap();
        second.unwrap();
        all.unwrap();

        let logged = std::fs::read_to_string(log).unwrap();
        assert_eq!(logged.matches("api --method POST").count(), 3, "{logged}");
        assert!(
            logged.contains("repos/acme/app/actions/runs/12/rerun"),
            "{logged}"
        );
        assert!(!logged.contains("auth status"), "{logged}");
    }

    #[tokio::test]
    async fn create_stack_posts_the_chain_as_array_fields() {
        let shim_dir = TempDir::new().unwrap();
        let log = shim_dir.path().join("log");
        let binary = shim_dir.path().join("gh");
        write_executable(
            &binary,
            &format!(
                "#!/bin/sh\necho \"$@\" >> {}\n[ \"$1\" = auth ] && echo '{{\"hosts\":{{\"github.com\":[{{\"active\":true,\"state\":\"success\",\"login\":\"tester\"}}]}}}}' && exit 0\n[ \"$1\" = api ] && exit 0\nexit 3\n",
                log.display()
            ),
        );
        create_stack(
            "github.com",
            "acme",
            "app",
            &[101, 102, 103],
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();

        let logged = std::fs::read_to_string(log).unwrap();
        assert!(
            logged.contains(
                "api --method POST repos/acme/app/stacks --field pull_requests[]=101 --field pull_requests[]=102 --field pull_requests[]=103"
            ),
            "the chain posts bottom to top as one array: {logged}"
        );
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
    async fn create_pr_returns_the_creation_stub_and_never_merges() {
        let (_dir, work, _bare) = init_paired_repos();
        run(&work, &["git", "checkout", "-b", "tidebreak/first-change"]);
        std::fs::write(work.join("extra.txt"), "line\n").unwrap();
        commit_all(&work, "first change", None).await.unwrap();
        push_branch(&work, "tidebreak/first-change", None)
            .await
            .unwrap();

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
        let digest = create_pull_request(
            &work,
            "first change",
            "tidebreak/first-change",
            "origin/main",
            None,
            None,
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
        // The creation answer is a stub: the conditional fetcher (decision
        // 66) enriches it on the caller's next status read, so creation
        // itself pays no view, checks, or timeline read.
        assert_eq!(digest.checks_summary, None);
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("pr create"), "{logged}");
        assert!(
            logged.contains("--base main"),
            "gh pr create must target the workspace base, not the host default: {logged}"
        );
        assert!(!logged.contains("--base origin/main"), "{logged}");
        assert!(!logged.contains("pr view"), "{logged}");
        assert!(!logged.contains("pr checks"), "{logged}");
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
        mark_workspace_pull_request_ready(work.path(), Some(shim_dir.path().to_str().unwrap()))
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
        merge_pull_request_target(
            "github.com",
            "acme",
            "app",
            42,
            MergeMethod::Squash,
            false,
            false,
            "abcdef123456",
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
        merge_pull_request_target(
            "github.com",
            "acme",
            "app",
            42,
            MergeMethod::Merge,
            true,
            false,
            "abcdef123456",
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(
            logged
                .contains("pr merge 42 --repo acme/app --squash --match-head-commit abcdef123456"),
            "{logged}"
        );
        assert!(
            logged.contains(
                "pr merge 42 --repo acme/app --merge --match-head-commit abcdef123456 --auto"
            ),
            "{logged}"
        );
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
            false,
            "abcdef123456",
            Some(shim_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();

        merge_pull_request_target(
            "github.com",
            "acme",
            "app",
            42,
            MergeMethod::Squash,
            false,
            true,
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
        // The admin bypass is a per-request flag, never a default.
        assert!(
            logged.contains("--match-head-commit abcdef123456 --admin"),
            "{logged}"
        );
        assert_eq!(logged.matches("pr merge").count(), 2, "{logged}");
        assert_eq!(logged.matches("--admin").count(), 1, "{logged}");
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
