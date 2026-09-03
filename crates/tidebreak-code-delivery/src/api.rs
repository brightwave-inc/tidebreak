//! Host-owned delivery transports and server integration points.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tidebreak_core::{
    CodeRepo, CodeWorkspace, DbStore, OwnerId, PullRequestDigest, RepoId, WorkspaceId,
};

use crate::wire::{
    CodeDeliveryPullRequestAction, CodeDeliverySourceError, CodeGitHubCapability,
    CodeGitHubRepositoryRef, CodeGitHubRepositoryTarget, CodePrMergeMethod,
};
use crate::{DeliveryCache, DeliveryError};

pub type DeliveryApiHandle = Arc<dyn DeliveryApi + 'static>;
pub type DeliveryReaderHandle = Arc<dyn DeliveryReader + 'static>;

/// One successful conditional host read.
#[derive(Debug)]
pub enum EndpointRead<T> {
    Fresh { value: T, etag: Option<String> },
    NotModified,
    Missing,
}

/// Why a conditional host read did not complete.
#[derive(Debug, thiserror::Error)]
pub enum HostReadError {
    #[error("the host is parked for {0:?}")]
    Parked(Duration),
    #[error("{0}")]
    Failed(String),
}

/// The caller's available delivery path and its user-facing capability.
#[derive(Clone)]
pub struct DeliveryAccess {
    pub capability: CodeGitHubCapability,
    pub reader: Option<DeliveryReaderHandle>,
    pub unavailable_kind: &'static str,
}

impl DeliveryAccess {
    pub fn source_error(&self) -> CodeDeliverySourceError {
        CodeDeliverySourceError {
            repository: None,
            kind: self.unavailable_kind.into(),
            message: self.capability.remediation.clone(),
            retry_at: None,
        }
    }

    /// Return the reader or the same refusal that list reads expose.
    pub fn require_reader(&self) -> Result<DeliveryReaderHandle, DeliveryError> {
        self.reader.clone().ok_or_else(|| {
            DeliveryError::conflict_kind(self.unavailable_kind, self.capability.remediation.clone())
        })
    }
}

/// One available local or hosted delivery transport.
#[async_trait]
pub trait DeliveryReader: Send + Sync {
    fn cache_scope(&self) -> &'static str;

    fn validate_pull_request_action(
        &self,
        _action: &CodeDeliveryPullRequestAction,
    ) -> Result<(), DeliveryError> {
        Ok(())
    }

    async fn api(&self, target: &CodeGitHubRepositoryTarget) -> Result<DeliveryApiHandle, String>;

    async fn action_api(
        &self,
        target: &CodeGitHubRepositoryTarget,
    ) -> Result<DeliveryApiHandle, DeliveryError> {
        self.api(target)
            .await
            .map_err(|message| DeliveryError::bad_request_kind("github", message))
    }
}

/// One repository-scoped delivery API.
#[async_trait]
pub trait DeliveryApi: Send + Sync {
    fn can_mark_pull_request_ready(&self) -> bool;

    async fn get(&self, endpoint: &str) -> Result<Value, String>;

    async fn repository(&self, target: &CodeGitHubRepositoryTarget) -> Result<Value, String>;

    async fn pull_requests(
        &self,
        target: &CodeGitHubRepositoryTarget,
        state: &str,
        fields: &str,
        checks_loaded: bool,
        author: Option<&str>,
    ) -> Result<Vec<Value>, String>;

    async fn deployments(&self, target: &CodeGitHubRepositoryTarget) -> Result<Value, String>;

    async fn workflow_runs(
        &self,
        target: &CodeGitHubRepositoryTarget,
        etag: Option<&str>,
    ) -> Result<EndpointRead<Vec<Value>>, HostReadError>;

    async fn merge_queue_membership(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
    ) -> Option<bool>;

    async fn pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        repository: &CodeGitHubRepositoryRef,
        number: u64,
    ) -> Result<Value, String>;

    async fn mark_pull_request_ready(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
    ) -> Result<(), DeliveryError>;

    async fn merge_pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        method: CodePrMergeMethod,
        auto: bool,
        admin: bool,
        expected_head_sha: &str,
    ) -> Result<(), DeliveryError>;

    async fn create_stack(
        &self,
        target: &CodeGitHubRepositoryTarget,
        numbers: &[u64],
    ) -> Result<(), DeliveryError>;

    async fn update_pull_request_state(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        state: &str,
    ) -> Result<(), DeliveryError>;

    async fn comment_on_pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        body: &str,
    ) -> Result<(), DeliveryError>;

    async fn rerun_failed_jobs(
        &self,
        target: &CodeGitHubRepositoryTarget,
        run_id: u64,
    ) -> Result<(), DeliveryError>;

    async fn rerun_workflow(
        &self,
        target: &CodeGitHubRepositoryTarget,
        run_id: u64,
    ) -> Result<(), DeliveryError>;
}

/// Server-owned state and side effects used by delivery business logic.
#[async_trait]
pub trait DeliveryRuntime: Send + Sync {
    fn store(&self) -> &DbStore;

    fn delivery_cache(&self) -> &DeliveryCache;

    async fn delivery_access(&self, owner: &OwnerId, force_refresh: bool) -> DeliveryAccess;

    async fn list_repos(&self, owner: &OwnerId) -> Result<Vec<CodeRepo>, DeliveryError>;

    async fn list_workspaces(
        &self,
        owner: &OwnerId,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, DeliveryError>;

    async fn emit_workspace_digests(&self, owner: &OwnerId, workspace_id: WorkspaceId);

    async fn record_pull_request_live_state(
        &self,
        owner: &OwnerId,
        source: Option<WorkspaceId>,
        digest: &PullRequestDigest,
    );

    fn refresh_workspaces_for_pull_request(&self, owner: &OwnerId, pull_request_url: &str);

    fn nudge_delivery_update(&self, owner: &OwnerId);
}
