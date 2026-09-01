//! Install-wide GitHub delivery reads and guarded user actions.
//!
//! The database remains the source of truth for registered repositories,
//! Tidebreak workspaces, and attributed pull-request facts. Delivery's
//! pull-request aggregate reads those facts (decision 77) so a workspace
//! link is a stored relation rather than a per-request heuristic. Workflow
//! run summaries persist as `code_workflow_run` rows and refresh through
//! the same `HostGate` as pull requests (decision 66, issue 2578).
//! Deployments stay live GitHub observations in a short in-memory cache;
//! volatile check and review fields on a pull request still overlay from
//! the host when the list read has them.
//!
//! This file holds the limits, the shared JSON and error helpers, and the
//! re-exports. Each concern lives in its own file:
//!
//! - [`api`]: the host reader, the credential-backed API, and the action API.
//! - [`cache`]: the per-owner aggregate, repository, and workspace-index caches.
//! - [`repositories`]: repository discovery, resolution, and target parsing.
//! - [`pull_requests`]: pull-request reads, actions, stacks, comments, and facts.
//! - [`runs`]: workflow runs and deployments.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use tidebreak_core::db::code::{
    delete_workflow_run_facts_absent_from, get_workflow_run_fetch_state, get_workspace,
    insert_pull_request_attribution, list_attributions_for_pull_requests,
    list_pull_request_facts_for_repo, list_workflow_run_facts_for_repo, save_pull_request_fact,
    save_workflow_run_fact, set_workflow_run_fetch_state, WorkflowRunFetchCondition,
};
use tidebreak_core::{
    CodePullRequestAttribution, CodePullRequestDiscovery, CodePullRequestFact, CodePullRequestId,
    CodePullRequestRelation, CodePullRequestState, CodeRepo, CodeWorkflowRunFact,
    CodeWorkflowRunId, CodeWorkspace, CodeWorkspaceStatus, OwnerId, PullRequestCheck,
    PullRequestCheckBucket, PullRequestComment, PullRequestCommentKind, PullRequestDigest, RepoId,
    WorkspaceId,
};

use super::gh::{self, GhObservation};
use super::reconcile::{
    StackParentCandidate, StackParentEdge, StackParentIndex, StackParentResolution,
    StackPullRequestIdentity, StackRepositoryIdentity,
};
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::obo_gateway::{GitCredential, GitForgeAttribution};
use crate::routes::code::types::{
    CodeDeliveryActionResult, CodeDeliveryCheck, CodeDeliveryDeploymentStatus,
    CodeDeliveryPrAttentionReason, CodeDeliveryPullRequestAction,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestFile,
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestSummary, CodeDeliveryPullRequestTarget,
    CodeDeliveryPullRequestsPage, CodeDeliveryRepositoriesSnapshot, CodeDeliveryRerunOutcome,
    CodeDeliveryRunAction, CodeDeliveryRunActionBody, CodeDeliveryRunAttentionReason,
    CodeDeliveryRunDetail, CodeDeliveryRunKind, CodeDeliveryRunQuery, CodeDeliveryRunSummary,
    CodeDeliveryRunTarget, CodeDeliveryRunsPage, CodeDeliverySourceError, CodeDeliveryStackMember,
    CodeDeliveryWorkflowJob, CodeDeliveryWorkspaceLink, CodeGitHubCapability,
    CodeGitHubRepositoryRef, CodeGitHubRepositoryTarget, CodePrMergeMethod,
    ResolveCodeDeliveryRepositoriesBody,
};

mod api;
mod cache;
mod pull_requests;
mod repositories;
mod runs;
#[cfg(test)]
mod tests;

pub(crate) use cache::DeliveryCache;
pub(crate) use pull_requests::{
    act_on_pull_request, digest_from_summary, pull_request_detail, query_pull_requests,
    query_pull_requests_by_number,
};
pub(crate) use repositories::{
    discover_repositories, parse_repository_input, repository_target_from_local,
    repository_target_from_path, resolve_repositories,
};
pub(crate) use runs::{act_on_run, query_runs, refresh_workflow_runs, run_detail};

use api::*;
use cache::*;
use pull_requests::*;
use repositories::*;
use runs::*;

const GH_READ_TIMEOUT: Duration = Duration::from_secs(45);

const GIT_READ_TIMEOUT: Duration = Duration::from_secs(10);

const LIST_CACHE_TTL: Duration = Duration::from_secs(30);

pub(crate) const MAX_REPOSITORIES: usize = 50;

const MAX_PAGE_SIZE: usize = 100;

const MAX_REMOTE_ITEMS_PER_REPO: usize = 100;

const DELIVERY_CONCURRENCY: usize = 4;

const MAX_COMMENT_BYTES: usize = 60_000;

/// Files rendered in the detail panel. The panel is a review aid rather than
/// a diff viewer, and GitHub itself stops rendering a diff well past this.
const MAX_DETAIL_FILES: usize = 300;

const GITHUB_DETAIL_PAGE_SIZE: usize = 100;

/// Transient GitHub failures (502/503/504, gateway timeouts) get one retry
/// after a short pause. A cross-repository list fans out far enough that one
/// unlucky repository would otherwise blank a whole column.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(700);

const PR_LIST_FIELDS: &str = "number,url,state,title,isDraft,author,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRepository,headRepositoryOwner,headRefName,headRefOid,baseRefName,updatedAt,createdAt,mergedAt,closedAt,labels,comments";

const PR_LIST_FIELDS_WITH_CHECKS: &str = "number,url,state,title,isDraft,author,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRepository,headRepositoryOwner,headRefName,headRefOid,baseRefName,updatedAt,createdAt,mergedAt,closedAt,labels,comments,statusCheckRollup";

/// True for GitHub failures that are worth one more attempt.
///
/// A cross-repository list is one `gh` invocation per repository, and the
/// list query is heavy enough that GitHub's gateway sheds it under load. One
/// shed request used to blank that repository's rows and surface a raw
/// `HTTP 504` banner over otherwise good results.
fn is_transient_github_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "502",
        "503",
        "504",
        "timeout",
        "timed out",
        "connection reset",
    ]
    .iter()
    .any(|token| message.contains(token))
}

/// Run one GitHub read, retrying a single time on a transient failure.
async fn with_transient_retry<T, F, Fut>(operation: F) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    match operation().await {
        Ok(value) => Ok(value),
        Err(message) if is_transient_github_error(&message) => {
            tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
            operation().await
        }
        Err(message) => Err(message),
    }
}

fn optional_filter(filters: &[String], value: Option<&str>) -> bool {
    filters.is_empty() || value.is_some_and(|value| contains_token(filters, value))
}

fn contains_token(values: &[String], target: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(target))
}

fn paginate<T>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: Option<u16>,
) -> Result<(Vec<T>, Option<String>), ServerError> {
    let offset = match cursor {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| ServerError::bad_request("invalid delivery cursor"))?,
        None => 0,
    };
    let limit = usize::from(limit.unwrap_or(50)).clamp(1, MAX_PAGE_SIZE);
    if offset > items.len() {
        return Err(ServerError::bad_request("delivery cursor is out of range"));
    }
    let end = (offset + limit).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    Ok((items.into_iter().skip(offset).take(limit).collect(), next))
}

fn string_array_path(value: &Value, path: &[&str], field: &str) -> Vec<String> {
    let mut current = value;
    for segment in path {
        let Some(next) = current.get(*segment) else {
            return Vec::new();
        };
        current = next;
    }
    current
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| text_field(item, field))
        .collect()
}

fn source_error(
    repository: Option<CodeGitHubRepositoryTarget>,
    message: String,
) -> CodeDeliverySourceError {
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("rate limit") {
        "rate_limited"
    } else if lower.contains("authentication") || lower.contains("auth login") {
        "gh_signed_out"
    } else if lower.contains("not found") || lower.contains("http 404") {
        "not_found"
    } else if lower.contains("forbidden") || lower.contains("http 403") {
        "forbidden"
    } else if is_transient_github_error(&message) {
        "transient"
    } else {
        "github"
    };
    // A shed gateway request already survived a retry by the time it lands
    // here. `HTTP 504: 504 Gateway Timeout (https://api.github.com/graphql)`
    // tells a reader nothing they can act on, so say what it means instead.
    let message = if kind == "transient" {
        let name = repository
            .as_ref()
            .map(|target| format!("{}/{}", target.owner, target.name))
            .unwrap_or_else(|| "A repository".into());
        format!("{name} did not answer in time. The next refresh retries it.")
    } else {
        message
    };
    CodeDeliverySourceError {
        repository,
        kind: kind.into(),
        message,
        retry_at: None,
    }
}

fn detail_source_error(
    repository: &CodeGitHubRepositoryTarget,
    section: &str,
    message: String,
) -> CodeDeliverySourceError {
    let mut error = source_error(Some(repository.clone()), message);
    error.message = format!("Could not load {section}: {}", error.message);
    error
}

fn record_full_detail_page(
    errors: &mut Vec<CodeDeliverySourceError>,
    repository: &CodeGitHubRepositoryTarget,
    section: &str,
    item_count: Option<usize>,
) {
    if item_count != Some(GITHUB_DETAIL_PAGE_SIZE) {
        return;
    }
    errors.push(CodeDeliverySourceError {
        repository: Some(repository.clone()),
        kind: "truncated".into(),
        message: format!(
            "{section} may be incomplete because GitHub returned the one-page limit of {GITHUB_DETAIL_PAGE_SIZE} items"
        ),
        retry_at: None,
    });
}

fn observation_error_kind(observation: &GhObservation) -> &'static str {
    if !observation.found {
        "gh_absent"
    } else if observation.authenticated == Some(false) {
        "gh_signed_out"
    } else {
        "gh_unavailable"
    }
}

fn map_gh_error(error: gh::GhError) -> ServerError {
    match error {
        gh::GhError::GhAbsent { instructions } => {
            ServerError::conflict_kind("gh_absent", instructions)
        }
        gh::GhError::GhSignedOut { instructions } => {
            ServerError::conflict_kind("gh_signed_out", instructions)
        }
        gh::GhError::MergeBlocked(message) => {
            ServerError::conflict_kind("pr_not_mergeable", message)
        }
        gh::GhError::AuthFailed(message) => ServerError::conflict_kind("git_auth_failed", message),
        gh::GhError::PushFailed(message) => ServerError::conflict_kind("git_push_failed", message),
        gh::GhError::NothingToCommit => ServerError::conflict("nothing to commit"),
        gh::GhError::User(message) => {
            if let Some(message) = message.strip_prefix(gh::GH_UNAVAILABLE_PREFIX) {
                ServerError::conflict_kind("gh_unavailable", message)
            } else if let Some(message) = message.strip_prefix(gh::PR_HEAD_CHANGED_PREFIX) {
                ServerError::conflict_kind("pr_head_changed", message)
            } else {
                ServerError::bad_request_kind("github", message)
            }
        }
        gh::GhError::Internal(message) => ServerError::internal(message),
    }
}

fn map_forge_action_error(error: super::forge_rest::ForgeActionError) -> ServerError {
    let message = error.message().to_owned();
    match error.kind() {
        super::forge_rest::ForgeActionErrorKind::AuthFailed => {
            ServerError::conflict_kind("git_auth_failed", message)
        }
        super::forge_rest::ForgeActionErrorKind::HeadChanged => {
            ServerError::conflict_kind("pr_head_changed", message)
        }
        super::forge_rest::ForgeActionErrorKind::NotMergeable => {
            ServerError::conflict_kind("pr_not_mergeable", message)
        }
        super::forge_rest::ForgeActionErrorKind::Other => {
            ServerError::bad_request_kind("github", message)
        }
    }
}

fn api_endpoint(target: &CodeGitHubRepositoryTarget, tail: &str) -> String {
    format!("repos/{}/{}/{}", target.owner, target.name, tail)
}

async fn run_api_json(binary: &Path, host: &str, endpoint: &str) -> Result<Value, String> {
    let mut args = vec!["api".to_owned(), endpoint.to_owned()];
    if host != "github.com" {
        args.extend(["--hostname".to_owned(), host.to_owned()]);
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let raw = gh::run_gh(Path::new("."), binary, &borrowed, GH_READ_TIMEOUT).await?;
    serde_json::from_str(&raw).map_err(|error| format!("invalid GitHub response: {error}"))
}

pub(crate) async fn git_read(cwd: &Path, args: &[&str]) -> Result<String, String> {
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
        .map_err(|error| format!("failed to spawn git: {error}"))?;
    let output = timeout(GIT_READ_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|error| format!("git {} failed: {error}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn normalized_optional(value: &Value, key: &str) -> Option<String> {
    text_field(value, key)
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "null")
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn datetime_field(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    text_field(value, key).and_then(|value| {
        DateTime::parse_from_rfc3339(&value)
            .ok()
            .map(|value| value.with_timezone(&Utc))
    })
}
