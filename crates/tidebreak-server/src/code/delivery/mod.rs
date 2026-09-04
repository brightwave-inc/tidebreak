//! Server adapters for the delivery crate.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use tidebreak_code_delivery::{DeliveryError, DeliveryErrorStatus, DeliveryRuntime};
use tidebreak_core::{OwnerId, PullRequestDigest, RepoId, WorkspaceId};

use super::gh::{self, GhObservation};
use super::runtime::CodeRuntime;
use crate::code::types::{
    CodeDeliveryActionResult, CodeDeliveryPullRequestAction, CodeDeliveryPullRequestActionBody,
    CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestTarget,
    CodeDeliveryPullRequestsPage, CodeDeliveryRepositoriesSnapshot, CodeDeliveryRunActionBody,
    CodeDeliveryRunDetail, CodeDeliveryRunQuery, CodeDeliveryRunTarget, CodeDeliveryRunsPage,
    CodeGitHubCapability, CodeGitHubRepositoryRef, CodeGitHubRepositoryTarget, CodePrMergeMethod,
    ResolveCodeDeliveryRepositoriesBody,
};
use crate::error::ServerError;
use crate::obo_gateway::{GitCredential, GitForgeAttribution};

mod api;

#[derive(Clone)]
struct ServerDeliveryRuntime(Arc<CodeRuntime>);

pub(crate) use tidebreak_code_delivery::{
    digest_from_summary, git_read, parse_repository_input, repository_target_from_local,
    repository_target_from_path, DeliveryCache, MAX_REPOSITORIES,
};

const GH_READ_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_REMOTE_ITEMS_PER_REPO: usize = 100;
const PR_LIST_FIELDS_WITH_CHECKS: &str = "number,url,state,title,isDraft,author,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRepository,headRepositoryOwner,headRefName,headRefOid,baseRefName,updatedAt,createdAt,mergedAt,closedAt,labels,comments,statusCheckRollup";

pub(crate) async fn discover_repositories(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    refresh: bool,
) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::discover_repositories(&adapter, owner, refresh)
        .await
        .map_err(Into::into)
}

pub(crate) async fn resolve_repositories(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: ResolveCodeDeliveryRepositoriesBody,
) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::resolve_repositories(&adapter, owner, allow_unscoped_delivery, body)
        .await
        .map_err(Into::into)
}

pub async fn query_pull_requests(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    query: CodeDeliveryPullRequestQuery,
) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::query_pull_requests(&adapter, owner, allow_unscoped_delivery, query)
        .await
        .map_err(Into::into)
}

pub(crate) async fn query_pull_requests_by_number(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    repositories: Vec<(CodeGitHubRepositoryTarget, Vec<u64>)>,
) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::query_pull_requests_by_number(&adapter, owner, repositories)
        .await
        .map_err(Into::into)
}

pub(crate) async fn pull_request_detail(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    target: CodeDeliveryPullRequestTarget,
) -> Result<CodeDeliveryPullRequestDetail, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::pull_request_detail(&adapter, owner, allow_unscoped_delivery, target)
        .await
        .map_err(Into::into)
}

pub(crate) async fn act_on_pull_request(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: CodeDeliveryPullRequestActionBody,
) -> Result<CodeDeliveryActionResult, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::act_on_pull_request(&adapter, owner, allow_unscoped_delivery, body)
        .await
        .map_err(Into::into)
}

pub(crate) async fn query_runs(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    query: CodeDeliveryRunQuery,
) -> Result<CodeDeliveryRunsPage, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::query_runs(&adapter, owner, allow_unscoped_delivery, query)
        .await
        .map_err(Into::into)
}

pub(crate) async fn run_detail(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    target: CodeDeliveryRunTarget,
) -> Result<CodeDeliveryRunDetail, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::run_detail(&adapter, owner, allow_unscoped_delivery, target)
        .await
        .map_err(Into::into)
}

pub(crate) async fn act_on_run(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: CodeDeliveryRunActionBody,
) -> Result<CodeDeliveryActionResult, ServerError> {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::act_on_run(&adapter, owner, allow_unscoped_delivery, body)
        .await
        .map_err(Into::into)
}

pub(crate) async fn refresh_workflow_runs(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    targets: &[CodeGitHubRepositoryTarget],
) {
    let adapter = ServerDeliveryRuntime(Arc::clone(runtime));
    tidebreak_code_delivery::refresh_workflow_runs(&adapter, owner, targets).await;
}

#[async_trait::async_trait]
impl DeliveryRuntime for ServerDeliveryRuntime {
    fn store(&self) -> &tidebreak_core::DbStore {
        &self.0.db
    }

    fn delivery_cache(&self) -> &DeliveryCache {
        &self.0.delivery_cache
    }

    async fn delivery_access(
        &self,
        owner: &OwnerId,
        force_refresh: bool,
    ) -> tidebreak_code_delivery::DeliveryAccess {
        api::delivery_access(&self.0, owner, force_refresh).await
    }

    async fn list_repos(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::CodeRepo>, DeliveryError> {
        Ok(tidebreak_core::db::code::list_repos(&self.0.db, owner).await?)
    }

    async fn list_workspaces(
        &self,
        owner: &OwnerId,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<tidebreak_core::CodeWorkspace>, DeliveryError> {
        Ok(tidebreak_core::db::code::list_workspaces(&self.0.db, owner, repo_id).await?)
    }

    async fn emit_workspace_digests(&self, owner: &OwnerId, workspace_id: WorkspaceId) {
        super::attention::emit_workspace_digests(&self.0.db, &self.0.bus, owner, workspace_id)
            .await;
    }

    async fn record_pull_request_live_state(
        &self,
        owner: &OwnerId,
        source: Option<WorkspaceId>,
        digest: &PullRequestDigest,
    ) {
        self.0
            .record_pull_request_live_state(owner, source, digest)
            .await;
    }

    fn refresh_workspaces_for_pull_request(&self, owner: &OwnerId, pull_request_url: &str) {
        self.0
            .refresh_workspaces_for_pull_request(owner, pull_request_url);
    }

    fn nudge_delivery_update(&self, owner: &OwnerId) {
        self.0.nudge_delivery_update(owner);
    }
}

impl From<DeliveryError> for ServerError {
    fn from(error: DeliveryError) -> Self {
        match error.status {
            DeliveryErrorStatus::BadRequest if error.kind == "bad_request" => {
                ServerError::bad_request(error.message)
            }
            DeliveryErrorStatus::BadRequest => {
                ServerError::bad_request_kind(error.kind, error.message)
            }
            DeliveryErrorStatus::Conflict if error.kind == "conflict" => {
                ServerError::conflict(error.message)
            }
            DeliveryErrorStatus::Conflict => ServerError::conflict_kind(error.kind, error.message),
            DeliveryErrorStatus::NotFound => ServerError::not_found(error.message),
            DeliveryErrorStatus::Internal => ServerError::internal(error.message),
        }
    }
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

fn map_gh_error(error: gh::GhError) -> DeliveryError {
    match error {
        gh::GhError::GhAbsent { instructions } => {
            DeliveryError::conflict_kind("gh_absent", instructions)
        }
        gh::GhError::GhSignedOut { instructions } => {
            DeliveryError::conflict_kind("gh_signed_out", instructions)
        }
        gh::GhError::MergeBlocked(message) => {
            DeliveryError::conflict_kind("pr_not_mergeable", message)
        }
        gh::GhError::AuthFailed(message) => {
            DeliveryError::conflict_kind("git_auth_failed", message)
        }
        gh::GhError::PushFailed(message) => {
            DeliveryError::conflict_kind("git_push_failed", message)
        }
        gh::GhError::NothingToCommit => DeliveryError::conflict("nothing to commit"),
        gh::GhError::User(message) => {
            if let Some(message) = message.strip_prefix(gh::GH_UNAVAILABLE_PREFIX) {
                DeliveryError::conflict_kind("gh_unavailable", message)
            } else if let Some(message) = message.strip_prefix(gh::PR_HEAD_CHANGED_PREFIX) {
                DeliveryError::conflict_kind("pr_head_changed", message)
            } else {
                DeliveryError::bad_request_kind("github", message)
            }
        }
        gh::GhError::Internal(message) => DeliveryError::internal(message),
    }
}

fn map_forge_action_error(error: super::forge_rest::ForgeActionError) -> DeliveryError {
    let message = error.message().to_owned();
    match error.kind() {
        super::forge_rest::ForgeActionErrorKind::AuthFailed => {
            DeliveryError::conflict_kind("git_auth_failed", message)
        }
        super::forge_rest::ForgeActionErrorKind::HeadChanged => {
            DeliveryError::conflict_kind("pr_head_changed", message)
        }
        super::forge_rest::ForgeActionErrorKind::NotMergeable => {
            DeliveryError::conflict_kind("pr_not_mergeable", message)
        }
        super::forge_rest::ForgeActionErrorKind::Other => {
            DeliveryError::bad_request_kind("github", message)
        }
    }
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
