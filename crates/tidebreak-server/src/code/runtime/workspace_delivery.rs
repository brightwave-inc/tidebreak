//! Workspace delivery: commits, pushes, pull requests, branch rules, and the hot refresh set.

use super::*;

pub(super) fn validate_workspace_merge_request(
    target: &CodeDeliveryPullRequestTarget,
    expected_head_sha: &str,
) -> Result<(), ServerError> {
    if target.number == 0
        || target.repository.host.trim().is_empty()
        || target.repository.owner.trim().is_empty()
        || target.repository.name.trim().is_empty()
        || expected_head_sha.trim().is_empty()
    {
        return Err(ServerError::bad_request_kind(
            "workspace_merge_target",
            "repository, pull request number, and expected head commit are required",
        ));
    }
    Ok(())
}

pub(super) fn same_repository(
    left: &CodeGitHubRepositoryTarget,
    right: &CodeGitHubRepositoryTarget,
) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.owner.eq_ignore_ascii_case(&right.owner)
        && left.name.eq_ignore_ascii_case(&right.name)
}

pub(super) fn repository_label(target: &CodeGitHubRepositoryTarget) -> String {
    if target.host.eq_ignore_ascii_case("github.com") {
        format!("{}/{}", target.owner, target.name)
    } else {
        format!("{}/{}/{}", target.host, target.owner, target.name)
    }
}

pub(super) fn pr_head_changed(expected: &str, current: &str) -> ServerError {
    ServerError::conflict_kind(
        "pr_head_changed",
        format!(
            "pull request head changed from {} to {}; refresh before merging",
            short_sha(expected),
            short_sha(current)
        ),
    )
}

/// One conditional fetch: the digest the workspace column now shows, and
/// the read that produced it.
pub(super) struct FetchedPullRequest {
    pub(super) digest: PullRequestDigest,
    pub(super) read: tidebreak_core::PullRequestRead,
}

pub(super) fn short_sha(sha: &str) -> &str {
    sha.get(..sha.len().min(8)).unwrap_or(sha)
}

/// The origin a hosted machine may lend the forge's App identity to: a
/// parseable forge repository on the forge's own host, and nothing else
/// (decision 63).
///
/// The host gate is a security boundary, not a convenience. The origin URL
/// is workspace state an agent can rewrite, and the parser accepts any
/// `host/owner/repo` shape — without the gate, the next push would mint a
/// live installation token and offer it to whatever host `origin` names.
/// Only `owner/name` ever travels to the gateway, and the one-shot helper
/// re-checks the same host at `get`, so both halves refuse independently.
pub(super) async fn forge_lending_target(
    worktree: &std::path::Path,
) -> Option<crate::code::types::CodeGitHubRepositoryTarget> {
    let target = crate::code::delivery::repository_target_from_path(worktree)
        .await
        .ok()?;
    target
        .host
        .eq_ignore_ascii_case(gh::GIT_CREDENTIAL_FORGE_HOST)
        .then_some(target)
}

impl CodeRuntime {
    /// Commit every change in the worktree.
    ///
    /// The person commits what they reviewed, so the commit never waits for a
    /// turn: a running or waiting turn refuses it at once, and a click that
    /// raced one would otherwise commit that turn's unreviewed changes under
    /// the person's message. `expected_tree` is the `worktree_tree` of the
    /// file list the person reviewed; a worktree that moved since refuses.
    pub async fn commit_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        message: Option<String>,
        expected_tree: Option<&str>,
    ) -> Result<CommitOutcome, ServerError> {
        let _turn_guard = self
            .worktree_turn_lock(id)
            .try_lock_owned()
            .map_err(|_| super::undo::turn_running())?;
        let workspace = self.require_live_workspace(owner, id).await?;
        for session in list_sessions_for_workspace(&self.db, owner, id).await? {
            if session.lifecycle == SessionLifecycle::Running
                || get_open_turn(&self.db, owner, session.id).await?.is_some()
            {
                return Err(super::undo::turn_running());
            }
            let queued = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
                .await?
                .is_empty();
            if queued
                && !tidebreak_core::db::code::queue_paused(&self.db, owner, session.id).await?
            {
                return Err(ServerError::conflict_kind(
                    "turn_queued",
                    "A message is waiting to run in this workspace. Commit after its turn \
                     finishes, or clear the queue first.",
                ));
            }
        }
        let worktree = std::path::Path::new(&workspace.worktree_path);
        if let Some(expected) = expected_tree {
            let now = crate::code::checkpoint::snapshot_tree(worktree)
                .await
                .map_err(map_checkpoint)?;
            if now != expected {
                return Err(ServerError::conflict_kind(
                    "worktree_changed",
                    "The files changed after you reviewed them, so nothing was committed. Review \
                     the changes again, then commit.",
                ));
            }
        }
        gh::commit_all(worktree, &workspace.title, message.as_deref())
            .await
            .map_err(map_gh)
    }

    pub async fn push_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<PushOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let acts_as = self.workspace_acts_as(owner, id).await;
        let credential = self
            .borrow_git_credential(owner, &worktree, acts_as)
            .await?;
        let outcome = gh::push_branch(&worktree, &workspace.branch_name, credential.as_ref())
            .await
            .map_err(map_gh)?;
        // The delivery lists hold the pre-push row.
        self.delivery_cache.invalidate();
        // Best-effort contributed fact (decision 77): a user push to a branch
        // that is a pull request's head is the same act the detector mints
        // for. Failures are silent; the reconcile sweep corrects. On a hosted
        // machine the read rides the forge REST API with the same credential
        // the push just used (decision 65) — one borrow, one operation.
        if let Ok(target) = crate::code::delivery::repository_target_from_path(&worktree).await {
            let values = match credential.as_ref() {
                Some(credential) => crate::code::forge_rest::list_pull_requests_for_head(
                    &self.forge_api_base_for(&target.host),
                    &target,
                    credential,
                    &workspace.branch_name,
                )
                .await
                .ok(),
                None => {
                    let gh_path = self.gh_search_path_owned();
                    gh::list_pull_requests_for_head_raw(
                        &target.host,
                        &target.owner,
                        &target.name,
                        &workspace.branch_name,
                        gh_path.as_deref(),
                    )
                    .await
                    .ok()
                }
            };
            if let Some(value) = values.as_ref().and_then(|values| values.first()) {
                if let Some(applied) = crate::code::pr_facts::record_confirmed_fact(
                    &self.db,
                    owner,
                    workspace.id,
                    None,
                    None,
                    &target,
                    value,
                    tidebreak_core::CodePullRequestRelation::Contributed,
                    tidebreak_core::CodePullRequestDiscovery::Command,
                )
                .await
                {
                    self.publish_pull_request_read(owner, &applied).await;
                }
            }
        }
        // A push dirties the row: refresh it now (decision 66), so the next
        // reader sees checks pending on the new head rather than the
        // pre-push snapshot. The fetcher's checks read is keyed to the new
        // head by construction.
        self.refresh_workspace_pr_row(owner, id).await;
        Ok(outcome)
    }

    /// Name the caller on this workspace's commits (decision 65), on a
    /// machine that lends gateway git identities and only when the gateway
    /// states the caller's own account acts.
    ///
    /// Best-effort by design: a caller who has not connected, a bot-attributed
    /// deployment, and a machine with its own credentials all leave the
    /// checkout exactly as it is, and the commit path reports its own
    /// failures. An identity the gateway states without a commit email is
    /// incomplete and configures nothing — half an identity would be worse
    /// than the checkout's own.
    pub(super) async fn name_workspace_author_with_lender(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
        lender: Option<&dyn crate::obo_gateway::GitCredentialLender>,
        acts_as: tidebreak_core::ActsAs,
    ) {
        let Some(lender) = lender else {
            return;
        };
        if acts_as == tidebreak_core::ActsAs::Bot {
            return;
        }
        let Ok(identity) = lender.git_forge_identity(owner, acts_as.into()).await else {
            return;
        };
        let crate::obo_gateway::GitForgeAttribution::Person {
            login,
            display_name,
            commit_email,
        } = identity.attribution
        else {
            return;
        };
        let Some(email) = commit_email else {
            return;
        };
        let name = display_name.unwrap_or(login);
        if let Err(error) = gh::configure_workspace_identity(worktree, &name, &email).await {
            tracing::debug!(error, "the workspace git identity was not configured");
        }
    }

    /// The forge login used for account-prefixed branches, when one is known.
    pub async fn branch_account_name(&self, owner: &OwnerId) -> Option<String> {
        #[cfg(any(test, feature = "test-support"))]
        // Tests must not inherit the developer machine's `gh` login.
        self.git_credentials()?;
        if let Some(lender) = self.git_credentials() {
            let identity = lender
                .git_forge_identity(
                    owner,
                    crate::obo_gateway::GitForgeAttributionRequest::Person,
                )
                .await
                .ok()?;
            return match identity.attribution {
                crate::obo_gateway::GitForgeAttribution::Person { login, .. } => Some(login),
                crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                    bot_login.or(Some(identity.app_name))
                }
            };
        }
        let search_path = self.gh_search_path_owned();
        gh::observe_gh(search_path.as_deref()).await.viewer_login
    }

    pub(super) async fn default_branch_prefix(&self, owner: &OwnerId) -> String {
        let account = self.branch_account_name(owner).await;
        naming_settings::read(&*self.db, owner, account.as_deref())
            .await
            .map(|settings| settings.effective_branch_prefix)
            .unwrap_or_else(|_| "tidebreak/".to_owned())
    }

    /// Borrow a repository-scoped forge credential for one git operation in
    /// `worktree`, on a machine that lends them (decision 63). `Ok(None)` is
    /// every machine that does not — and every checkout whose origin
    /// [`forge_lending_target`] rules out: those operations carry no
    /// credential today and keep working exactly as they do.
    ///
    /// A refusal from the gateway fails the operation with its reason rather
    /// than falling back to an uncredentialed attempt — the attempt would
    /// fail with a worse message, and a fallback would blur which identity
    /// acted.
    pub(super) async fn borrow_git_credential(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
        acts_as: tidebreak_core::ActsAs,
    ) -> Result<Option<crate::obo_gateway::GitCredential>, ServerError> {
        let Some(lender) = self.git_credentials() else {
            return Ok(None);
        };
        let Some(target) = forge_lending_target(worktree).await else {
            return Ok(None);
        };
        let repository = format!("{}/{}", target.owner, target.name);
        match lender
            .git_credential(owner, &repository, acts_as.into())
            .await
        {
            Ok(credential) => Ok(Some(credential)),
            Err(refusal) => Err(ServerError::unprocessable_kind(
                "git_forge_refused",
                crate::code::clone::git_forge_refusal_message(&refusal),
            )),
        }
    }

    /// The REST context for a pull-request operation in `worktree`
    /// (decision 65): the forge repository the checkout names plus one
    /// borrowed credential. `Ok(None)` is every machine with its own
    /// credentials and every checkout outside the lending gate — those keep
    /// `gh` exactly as it is. A gateway refusal fails the operation with its
    /// reason, exactly as a push does.
    pub(crate) async fn workspace_acts_as(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> tidebreak_core::ActsAs {
        match list_sessions_for_workspace(&self.db, owner, workspace_id).await {
            Ok(sessions) => sessions
                .iter()
                .find(|session| session.kind == SessionKind::Interactive)
                .or(sessions.first())
                .map(Session::acts_as)
                .unwrap_or(tidebreak_core::ActsAs::Person),
            Err(_) => tidebreak_core::ActsAs::Person,
        }
    }

    pub(super) async fn forge_rest_context(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
        acts_as: tidebreak_core::ActsAs,
    ) -> Result<
        Option<(
            crate::code::types::CodeGitHubRepositoryTarget,
            crate::obo_gateway::GitCredential,
        )>,
        ServerError,
    > {
        if self.git_credentials().is_none() {
            return Ok(None);
        }
        let Some(target) = forge_lending_target(worktree).await else {
            return Ok(None);
        };
        let credential = self
            .borrow_git_credential(owner, worktree, acts_as)
            .await?
            .ok_or_else(|| {
                ServerError::unprocessable_kind(
                    "git_forge_refused",
                    "this checkout's origin is not a lendable forge repository",
                )
            })?;
        Ok(Some((target, credential)))
    }

    /// Resolve the connection that created this workspace before borrowing authority.
    /// An external workspace never falls back to a browser caller after revocation.
    pub(crate) async fn workspace_git_lender(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Result<
        (
            Option<Arc<dyn crate::obo_gateway::GitCredentialLender>>,
            bool,
        ),
        ServerError,
    > {
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace.id).await?;
        let ids: Vec<_> = sessions.iter().map(|session| session.id).collect();
        let bindings =
            tidebreak_core::db::code::list_external_bindings_for_sessions(&self.db, owner, &ids)
                .await?;
        let Some(first) = bindings.first() else {
            return Ok((self.git_credentials().cloned(), false));
        };
        if bindings
            .iter()
            .any(|binding| binding.grant_id != first.grant_id)
        {
            return Err(ServerError::conflict_kind(
                "external_connection_conflict",
                "This workspace has conflicting external connections.",
            ));
        }
        let external = self
            .harness_llm
            .as_ref()
            .and_then(|relay| relay.external_delegations())
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "external_reconnect_required",
                    "The connection that created this workspace is unavailable.",
                )
            })?;
        let gateway = external
            .for_session(owner, first.session_id)
            .await?
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "external_reconnect_required",
                    "The connection that created this workspace is unavailable.",
                )
            })?;
        Ok((Some(gateway), true))
    }

    /// A remote checkout has no host path. Its registered origin and original
    /// session connection provide the same repository-scoped REST authority.
    async fn workspace_forge_rest_context(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        lender: Option<&Arc<dyn crate::obo_gateway::GitCredentialLender>>,
        acts_as: tidebreak_core::ActsAs,
    ) -> Result<
        Option<(
            CodeGitHubRepositoryTarget,
            crate::obo_gateway::GitCredential,
        )>,
        ServerError,
    > {
        let Some(lender) = lender else {
            return Ok(None);
        };
        let target = if workspace.is_remote() {
            self.workspace_repository_target(owner, workspace).await
        } else {
            forge_lending_target(std::path::Path::new(&workspace.worktree_path)).await
        };
        let Some(target) = target.filter(|target| {
            target
                .host
                .eq_ignore_ascii_case(gh::GIT_CREDENTIAL_FORGE_HOST)
        }) else {
            return Err(ServerError::unprocessable_kind(
                "git_forge_refused",
                "This workspace has no supported forge repository.",
            ));
        };
        if let Some(pr) = workspace.pr.as_ref() {
            if let Some((host, repo_owner, repo_name, number)) = pr
                .url
                .as_deref()
                .and_then(crate::code::pr_facts::pull_request_identity_from_url)
            {
                let identity = CodeGitHubRepositoryTarget {
                    host,
                    owner: repo_owner,
                    name: repo_name,
                };
                if !same_repository(&target, &identity) || number != pr.number {
                    return Err(ServerError::conflict_kind(
                        "workspace_pr_target_changed",
                        "The pull request does not match this workspace repository.",
                    ));
                }
            }
        }
        let credential = lender
            .git_credential(
                owner,
                &format!("{}/{}", target.owner, target.name),
                acts_as.into(),
            )
            .await
            .map_err(|error| {
                ServerError::unprocessable_kind(
                    "git_forge_refused",
                    crate::code::clone::git_forge_refusal_message(&error),
                )
            })?;
        Ok(Some((target, credential)))
    }

    async fn remote_workspace_pr(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let (lender, _) = self.workspace_git_lender(owner, workspace).await?;
        let acts_as = self.workspace_acts_as(owner, workspace.id).await;
        let identity = match lender {
            Some(lender) => lender.git_forge_identity(owner, acts_as.into()).await.ok(),
            None => None,
        };
        let (pushes_as, pushes_as_self) = match identity {
            Some(identity) => match identity.attribution {
                crate::obo_gateway::GitForgeAttribution::Person { login, .. } => {
                    (Some(login), Some(true))
                }
                crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                    (Some(bot_login.unwrap_or(identity.app_name)), Some(false))
                }
            },
            None => (None, None),
        };
        Ok(WorkspaceGitStatus {
            remote: true,
            git: None,
            dirty: false,
            unpushed: false,
            ahead: 0,
            has_upstream: false,
            suggested_commit_message: String::new(),
            pr: workspace.pr.clone(),
            gh_found: false,
            gh_authenticated: None,
            remediation:
                "Git changes run in the remote runtime. Pull request status comes from GitHub."
                    .into(),
            pushes_as,
            pushes_as_self,
        })
    }

    pub async fn workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        refuse_archived_local_workspace(&workspace)?;
        // Being asked is the attention signal (decision 66): the request
        // path reads local git plus the stored row, and the hot refresher
        // this mark feeds is what keeps the row current while anyone reads.
        self.mark_workspace_pr_hot(owner, workspace.id);
        if workspace.is_remote() {
            return self.remote_workspace_pr(owner, &workspace).await;
        }
        let gh_path = self.gh_search_path_owned();
        let mut status = gh::workspace_git_status(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.title,
            &workspace.branch_name,
            &workspace.base_ref,
            workspace.pr.clone(),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        // On a hosted machine, say whose identity a push would act as
        // (decisions 63 and 65) — only for a checkout the machine would
        // actually lend an identity to, so the sentence is never wider than
        // the lending. Probed per caller and held fresh by the lender; a
        // refusal simply leaves the field empty — the push itself reports
        // refusals with their reasons.
        if let Some(lender) = self.git_credentials() {
            let worktree = std::path::Path::new(&workspace.worktree_path);
            if forge_lending_target(worktree).await.is_some() {
                let acts_as = self.workspace_acts_as(owner, workspace.id).await;
                if let Ok(identity) = lender.git_forge_identity(owner, acts_as.into()).await {
                    match identity.attribution {
                        crate::obo_gateway::GitForgeAttribution::Person { login, .. } => {
                            status.pushes_as = Some(login);
                            status.pushes_as_self = Some(true);
                        }
                        crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                            status.pushes_as = Some(bot_login.unwrap_or(identity.app_name));
                            status.pushes_as_self = Some(false);
                        }
                    }
                }
            }
        }
        Ok(status)
    }

    /// Force a fresh host read now — the user asked, or a mutation just
    /// moved the pull request — then answer with the refreshed row.
    pub async fn refresh_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.try_refresh_workspace_pr_row(owner, id)
            .await
            .map_err(|message| ServerError::unprocessable_kind("pr_refresh_failed", message))?;
        self.workspace_pr(owner, id).await
    }

    /// One conditional refresh of the workspace's pull-request row: fetch,
    /// write the row (which fans real change out to every other holder),
    /// and take the new digest as this workspace's column. Quiet on every
    /// failure — the caller's row keeps whatever it had, and the next tick
    /// or sweep corrects.
    ///
    /// The fetch rides an authenticated `gh` where one exists; a
    /// gateway-hosted machine has none (decision 65), so there the same
    /// refresh drives the forge REST API with a borrowed credential —
    /// same gate, same stored ETags, same 304-shaped traffic.
    pub async fn refresh_workspace_pr_row(&self, owner: &OwnerId, id: WorkspaceId) {
        if let Err(error) = self.try_refresh_workspace_pr_row(owner, id).await {
            tracing::debug!(%error, "workspace pull request refresh failed");
        }
    }

    async fn try_refresh_workspace_pr_row(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<(), String> {
        let workspace = self
            .get_workspace(owner, id)
            .await
            .map_err(|error| error.message().to_owned())?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Ok(());
        }
        let gh_path = self.gh_search_path_owned();
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let (lender, external) = self
            .workspace_git_lender(owner, &workspace)
            .await
            .map_err(|error| error.message().to_owned())?;
        let binary = if workspace.is_remote() || external {
            None
        } else {
            gh::authenticated_gh_binary(gh_path.as_deref()).await
        };
        // The fetch lands its read through the store's merge, which also
        // rewrites this workspace's column and publishes the change.
        match binary {
            Some(binary) => {
                let transport = crate::code::pr_fetch::FetchTransport::Gh {
                    cwd: &worktree,
                    binary: &binary,
                };
                self.fetched_workspace_digest(owner, &workspace, transport)
                    .await?;
            }
            None => {
                let (target, credential) = self
                    .workspace_forge_rest_context(
                        owner,
                        &workspace,
                        lender.as_ref(),
                        self.workspace_acts_as(owner, workspace.id).await,
                    )
                    .await
                    .map_err(|error| error.message().to_owned())?
                    .ok_or_else(|| {
                        "Connect GitHub before refreshing pull request status.".to_owned()
                    })?;
                let api_base = self.forge_api_base_for(&target.host);
                let transport = crate::code::pr_fetch::FetchTransport::Rest {
                    api_base: &api_base,
                    credential: &credential,
                };
                let fetched = self
                    .fetched_workspace_digest(owner, &workspace, transport)
                    .await?;
                if workspace.is_remote() {
                    if let Some(fetched) = fetched.as_ref() {
                        self.record_remote_workspace_pr(
                            owner,
                            &workspace,
                            &target,
                            &credential,
                            fetched,
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Attribute a remote PR only after GitHub confirms the assigned workspace branch.
    async fn record_remote_workspace_pr(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        target: &CodeGitHubRepositoryTarget,
        credential: &crate::obo_gateway::GitCredential,
        fetched: &FetchedPullRequest,
    ) -> Result<(), String> {
        let digest = &fetched.digest;
        if digest.head_branch.as_deref() != Some(workspace.branch_name.as_str()) {
            return Ok(());
        }
        let attributed = self
            .workspace_pull_requests(owner, workspace.id)
            .await
            .map_err(|error| error.message().to_owned())?;
        if attributed.iter().any(|(fact, _)| {
            fact.number == digest.number
                && fact.host.eq_ignore_ascii_case(&target.host)
                && fact.repo_owner.eq_ignore_ascii_case(&target.owner)
                && fact.repo_name.eq_ignore_ascii_case(&target.name)
        }) {
            return Ok(());
        }
        let endpoint = format!(
            "repos/{}/{}/pulls/{}",
            target.owner, target.name, digest.number
        );
        let raw = crate::code::forge_rest::api_get(
            &self.forge_api_base_for(&target.host),
            credential,
            &endpoint,
        )
        .await?;
        let value = crate::code::forge_rest::fact_value(&raw);
        if value["number"].as_u64() != Some(digest.number)
            || value["headRefName"].as_str() != Some(workspace.branch_name.as_str())
            || value["headRefOid"].as_str() != digest.head_sha.as_deref()
        {
            return Err("The pull request changed during refresh. Retry the refresh.".into());
        }
        let Some((host, repo_owner, repo_name, number)) = value["url"]
            .as_str()
            .and_then(crate::code::pr_facts::pull_request_identity_from_url)
        else {
            return Err("The pull request response has no repository identity.".into());
        };
        if number != digest.number
            || !same_repository(
                target,
                &CodeGitHubRepositoryTarget {
                    host,
                    owner: repo_owner,
                    name: repo_name,
                },
            )
        {
            return Err(
                "The pull request response does not match the workspace repository.".into(),
            );
        }
        // The fetch's own read mints the row, so the row starts with the
        // live state the fetch observed rather than a bare snapshot.
        let applied = crate::code::pr_facts::record_confirmed_read(
            &self.db,
            &fetched.read,
            workspace.id,
            None,
            None,
            tidebreak_core::CodePullRequestRelation::Contributed,
            tidebreak_core::CodePullRequestDiscovery::Reconcile,
        )
        .await
        .ok_or_else(|| "The confirmed pull request could not be saved.".to_owned())?;
        self.publish_pull_request_read(owner, &applied).await;
        Ok(())
    }

    /// Keep this workspace on the hot refresh tier.
    pub(super) fn mark_workspace_pr_hot(&self, owner: &OwnerId, id: WorkspaceId) {
        self.hot_prs.mark(owner, id);
    }

    /// The hot tier itself, for a writer that outlives no runtime reference:
    /// the post-turn fact detector marks the workspace whose head it just
    /// watched move (issue 2799).
    pub(in crate::code) fn hot_pull_requests(&self) -> crate::code::pr_refresh::HotPullRequests {
        self.hot_prs.clone()
    }

    /// One delivery nudge on the updates channel, debounced per owner
    /// (decision 66): a sweep that moves several rows costs one re-read,
    /// not one per row.
    pub fn nudge_delivery_update(&self, owner: &OwnerId) {
        self.delivery_nudges.publish(&self.bus, owner);
    }

    /// The workspaces the hot refresher walks this tick.
    pub(in crate::code) fn hot_pull_request_workspaces(&self) -> Vec<(OwnerId, WorkspaceId)> {
        self.hot_prs.live()
    }

    /// Start the hot pull-request refresher once (decision 66).
    pub(in crate::code) fn ensure_pr_refresh_sweep(self: &Arc<Self>) {
        if self.pr_refresh_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = crate::code::pr_refresh::PrRefreshGuard::spawn(Arc::downgrade(self));
        *self.pr_refresh_sweep.lock().expect("pr refresh sweep") = Some(guard);
    }

    /// The repository identity a workspace's pull request lives on: the
    /// registered origin when the reconcile sweep has confirmed one, the
    /// worktree's own remote otherwise.
    pub(crate) async fn workspace_repository_target(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Option<crate::code::types::CodeGitHubRepositoryTarget> {
        let repo = self.get_repo(owner, workspace.repo_id).await.ok()?;
        if let (Some(host), Some(repo_owner), Some(name)) = (
            repo.origin_host.clone(),
            repo.origin_owner.clone(),
            repo.origin_name.clone(),
        ) {
            return Some(crate::code::types::CodeGitHubRepositoryTarget {
                host,
                owner: repo_owner,
                name,
            });
        }
        if workspace.is_remote() {
            return None;
        }
        crate::code::delivery::repository_target_from_local(&repo)
            .await
            .ok()
    }

    /// The workspace's pull request through the conditional fetcher
    /// (decision 66), over whichever transport the caller resolved — `gh` or
    /// the hosted forge REST API.
    ///
    /// Identity comes from the stored digest's URL, or a head lookup when
    /// the workspace knows no pull request yet. Each endpoint sends the
    /// row's stored validator. A 304 restates exactly the stored fields that
    /// validator names, and a 200 carries new ones; either way the pass
    /// becomes one read, dated per endpoint, that lands through the store's
    /// merge. The merge decides what each answer may change, so a pass that
    /// lost a race to a newer read changes nothing, and the workspace column
    /// shows the merged row. `None` when the branch has no pull request.
    pub(super) async fn fetched_workspace_digest(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        transport: crate::code::pr_fetch::FetchTransport<'_>,
    ) -> Result<Option<FetchedPullRequest>, String> {
        use crate::code::pr_fetch::{self, EndpointRead};
        use tidebreak_core::{PullRequestChecksRead, PullRequestQueueRead, PullRequestReviewRead};

        let gate = &self.host_gate;
        let mut stored_identity = workspace
            .pr
            .as_ref()
            .and_then(|pr| pr.url.as_deref())
            .and_then(crate::code::pr_facts::pull_request_identity_from_url);
        if workspace.pr.as_ref().is_some_and(|pr| {
            pr.state == "merged" || pr.state == "closed" || pr.merged == Some(true)
        }) {
            if let Some(target) = self.workspace_repository_target(owner, workspace).await {
                if let Some(found) = pr_fetch::read_pull_request_for_head(
                    gate,
                    transport,
                    &target.host,
                    &target.owner,
                    &target.name,
                    &workspace.branch_name,
                )
                .await
                .map_err(|error| error.to_string())?
                {
                    if found.state == "open" {
                        stored_identity =
                            Some((target.host, target.owner, target.name, found.number));
                    }
                }
            }
        }
        let (host, repo_owner, repo_name, number) = match stored_identity {
            Some(identity) => identity,
            None => {
                let target = self
                    .workspace_repository_target(owner, workspace)
                    .await
                    .ok_or_else(|| "Could not identify the workspace repository.".to_owned())?;
                let found = match pr_fetch::read_pull_request_for_head(
                    gate,
                    transport,
                    &target.host,
                    &target.owner,
                    &target.name,
                    &workspace.branch_name,
                )
                .await
                {
                    Ok(Some(found)) => found,
                    Ok(None) => return Ok(None),
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: pull-request lookup skipped");
                        return Err(
                            "Pull request status could not be confirmed. Retry the refresh."
                                .to_owned(),
                        );
                    }
                };
                (target.host, target.owner, target.name, found.number)
            }
        };
        let stored = tidebreak_core::db::code::get_stored_pull_request(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
        )
        .await
        .ok()
        .flatten();
        let etags = stored
            .as_ref()
            .map(|stored| stored.etags.clone())
            .unwrap_or_default();
        let mut read = tidebreak_core::PullRequestRead::new(
            owner.clone(),
            host.clone(),
            repo_owner.clone(),
            repo_name.clone(),
            number,
        );

        // Every observation is dated before its request goes out, so a slow
        // answer never claims to be newer than it is.
        let pull_at = Utc::now();
        let object = match pr_fetch::read_pull_request(
            gate,
            transport,
            &host,
            &repo_owner,
            &repo_name,
            number,
            etags.pull.as_deref(),
        )
        .await
        {
            Ok(EndpointRead::Fresh { value, etag }) => value
                .object_read(pull_at, etag)
                .ok_or_else(|| "The pull request answer carried no URL.".to_owned())?,
            // The host confirms the object the stored validator names, which
            // is the stored one.
            Ok(EndpointRead::NotModified) => stored
                .as_ref()
                .ok_or_else(|| "The cached pull request is unavailable.".to_owned())?
                .restate_object(pull_at),
            Ok(EndpointRead::Missing) => {
                return Err("The pull request is unavailable on GitHub.".to_owned());
            }
            Err(failure) => {
                tracing::debug!(error = %failure, "code-mode: pull-request read skipped");
                return Err(failure.to_string());
            }
        };
        let head_sha = object.snapshot.head_sha.clone();
        let open = object.snapshot.state == tidebreak_core::CodePullRequestState::Open;
        let base_branch = object.snapshot.base_branch.clone();
        read.object = Some(object);

        let checks_at = Utc::now();
        read.checks = match head_sha.as_deref() {
            Some(sha) => {
                // A checks validator names one head's answer; a moved head
                // sends an unconditional read.
                let same_head = stored
                    .as_ref()
                    .and_then(|stored| stored.fact.head_sha.as_deref())
                    == Some(sha);
                let conditional = if same_head {
                    etags.checks.as_deref()
                } else {
                    None
                };
                match pr_fetch::read_check_runs(
                    gate,
                    transport,
                    &host,
                    &repo_owner,
                    &repo_name,
                    sha,
                    conditional,
                )
                .await
                {
                    Ok(EndpointRead::Fresh { value, etag }) => Some(PullRequestChecksRead {
                        head_sha: head_sha.clone(),
                        checks: value,
                        observed_at: checks_at,
                        etag,
                    }),
                    Ok(EndpointRead::NotModified) => stored
                        .as_ref()
                        .and_then(|stored| stored.restate_checks(checks_at)),
                    Ok(EndpointRead::Missing) => Some(PullRequestChecksRead {
                        head_sha: head_sha.clone(),
                        checks: Vec::new(),
                        observed_at: checks_at,
                        etag: None,
                    }),
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: check-runs read skipped");
                        return Err(format!(
                            "Could not read checks for the current head: {failure}"
                        ));
                    }
                }
            }
            None => Some(PullRequestChecksRead {
                head_sha: None,
                checks: Vec::new(),
                observed_at: checks_at,
                etag: None,
            }),
        };
        let rules = if open {
            self.branch_rules_for(
                transport,
                &host,
                &repo_owner,
                &repo_name,
                (!base_branch.is_empty()).then_some(base_branch.as_str()),
            )
            .await
        } else {
            None
        };
        let reviews_at = Utc::now();
        read.review = if open {
            match pr_fetch::read_reviews(
                gate,
                transport,
                &host,
                &repo_owner,
                &repo_name,
                number,
                etags.reviews.as_deref(),
            )
            .await
            {
                Ok(EndpointRead::Fresh { value, etag }) => Some(PullRequestReviewRead {
                    decision: pr_fetch::derive_review_decision(rules, &value),
                    observed_at: reviews_at,
                    etag,
                }),
                Ok(EndpointRead::NotModified) => stored
                    .as_ref()
                    .map(|stored| stored.restate_review(reviews_at)),
                Ok(EndpointRead::Missing) => Some(PullRequestReviewRead {
                    decision: None,
                    observed_at: reviews_at,
                    etag: None,
                }),
                Err(failure) => {
                    tracing::debug!(error = %failure, "code-mode: reviews read skipped");
                    return Err(format!("Could not read review status: {failure}"));
                }
            }
        } else {
            // A settled pull request has no review decision to act on.
            Some(PullRequestReviewRead {
                decision: None,
                observed_at: reviews_at,
                etag: None,
            })
        };
        let queue_at = Utc::now();
        let in_merge_queue = if open {
            match rules {
                // Rules that name no queue spare the timeline read; a queue
                // — or a host that cannot answer the rules endpoint — pays
                // it.
                Some(rules) if !rules.has_merge_queue => Some(false),
                _ => {
                    pr_fetch::read_merge_queue_membership(
                        gate,
                        transport,
                        &host,
                        &repo_owner,
                        &repo_name,
                        number,
                    )
                    .await
                }
            }
        } else {
            Some(false)
        };
        read.queue = in_merge_queue.map(|in_merge_queue| PullRequestQueueRead {
            head_sha: head_sha.clone(),
            in_merge_queue,
            observed_at: queue_at,
        });

        // A pull request no reader tracks yet stays untracked: the read
        // shows in this workspace's column and mints no row (decision 77).
        // The column takes it only while it still shows what this pass began
        // from, so a pull request a create adopted meanwhile stays.
        let applied = self
            .apply_pull_request_read(
                &read,
                tidebreak_core::db::code::PullRequestReadOptions {
                    mint_row: false,
                    adopt: Some(tidebreak_core::db::code::PullRequestAdoption {
                        workspace: workspace.id,
                        replacing: workspace.pr.as_ref().and_then(|pr| pr.url.clone()),
                    }),
                },
            )
            .await
            .ok_or_else(|| {
                "Pull request status could not be saved. Retry the refresh.".to_owned()
            })?;
        Ok(Some(FetchedPullRequest {
            digest: applied.fact.digest(),
            read,
        }))
    }

    /// The base branch's rules, cached per branch for [`BRANCH_RULES_TTL`].
    pub(super) async fn branch_rules_for(
        &self,
        transport: crate::code::pr_fetch::FetchTransport<'_>,
        host: &str,
        repo_owner: &str,
        repo_name: &str,
        branch: Option<&str>,
    ) -> Option<crate::code::pr_fetch::BranchRules> {
        let branch = branch?;
        let key = format!(
            "{}/{}/{}/{}",
            host.to_ascii_lowercase(),
            repo_owner.to_ascii_lowercase(),
            repo_name.to_ascii_lowercase(),
            branch
        );
        {
            let mut cache = self.branch_rules.lock().expect("branch rules");
            cache.retain(|_, entry| entry.fetched_at.elapsed() <= BRANCH_RULES_TTL);
            if let Some(entry) = cache.get(&key) {
                return entry.rules;
            }
        }
        let rules = match crate::code::pr_fetch::read_branch_rules(
            &self.host_gate,
            transport,
            host,
            repo_owner,
            repo_name,
            branch,
        )
        .await
        {
            Ok(crate::code::pr_fetch::EndpointRead::Fresh { value, .. }) => Some(value),
            // A host with no rules endpoint answers for the cache period
            // too: hammering a known 404 helps nobody.
            Ok(_) => None,
            // A park or a transport failure states nothing; ask again next
            // time.
            Err(_) => return None,
        };
        self.branch_rules.lock().expect("branch rules").insert(
            key,
            CachedBranchRules {
                fetched_at: Instant::now(),
                rules,
            },
        );
        rules
    }

    /// After a pull-request state change on the delivery surface, make every
    /// live workspace holding that pull request read fresh: drop each one's
    /// digest cache entry and take the normal status path, which persists
    /// the digest and broadcasts the change (decision 66). Matching is by
    /// the digest's own URL, so a same-numbered pull request in another
    /// repository stays untouched. Detached and best-effort: the action's
    /// response never waits on it, and a failed re-read leaves the next
    /// sweep to correct.
    pub fn refresh_workspaces_for_pull_request(
        self: &Arc<Self>,
        owner: &OwnerId,
        pull_request_url: &str,
    ) {
        let runtime = Arc::clone(self);
        let owner = owner.clone();
        let url = pull_request_url.to_owned();
        tokio::spawn(async move {
            let workspaces = match list_workspaces(&runtime.db, &owner, None).await {
                Ok(workspaces) => workspaces,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "code-mode: could not list workspaces after a delivery action"
                    );
                    return;
                }
            };
            for workspace in workspaces {
                if workspace.status != CodeWorkspaceStatus::Active {
                    continue;
                }
                let holds = workspace
                    .pr
                    .as_ref()
                    .and_then(|pr| pr.url.as_deref())
                    .is_some_and(|value| value.eq_ignore_ascii_case(&url));
                if !holds {
                    continue;
                }
                if let Err(err) = runtime.refresh_workspace_pr(&owner, workspace.id).await {
                    tracing::warn!(
                        workspace = %workspace.id,
                        error = %err.message(),
                        "code-mode: workspace digest refresh after a delivery action failed"
                    );
                }
            }
        });
    }

    /// Land one pull-request read through the store's merge (decision 66)
    /// and publish what it changed. Quiet on a store failure: the caller
    /// keeps what it had, and the next read corrects.
    pub(crate) async fn apply_pull_request_read(
        &self,
        read: &tidebreak_core::PullRequestRead,
        options: tidebreak_core::db::code::PullRequestReadOptions,
    ) -> Option<tidebreak_core::db::code::AppliedPullRequestRead> {
        match tidebreak_core::db::code::save_pull_request_read(&self.db, read, options).await {
            Ok(Some(applied)) => {
                self.publish_pull_request_read(&read.owner, &applied).await;
                Some(applied)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::debug!(error = %err, "code-mode: pull-request write failed");
                None
            }
        }
    }

    /// Restate the digest of every workspace whose column an applied read
    /// rewrote, and send one delivery nudge when a stored field a reader sees
    /// moved (decision 66): the delivery page and notification monitor
    /// re-read on receipt instead of on their own timers.
    pub(crate) async fn publish_pull_request_read(
        &self,
        owner: &OwnerId,
        applied: &tidebreak_core::db::code::AppliedPullRequestRead,
    ) {
        if applied.stored && applied.changed {
            self.nudge_delivery_update(owner);
        }
        for workspace in &applied.workspaces {
            crate::code::attention::emit_workspace_digests(&self.db, &self.bus, owner, *workspace)
                .await;
        }
    }

    pub async fn workspace_pr_comments(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<gh::PrComments, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        let (lender, external) = self.workspace_git_lender(owner, &workspace).await?;
        if !workspace.is_remote() && !external && lender.is_none() {
            let gh_path = self.gh_search_path_owned();
            return gh::load_pr_comments(
                std::path::Path::new(&workspace.worktree_path),
                gh_path.as_deref(),
            )
            .await
            .map_err(map_gh);
        }
        let (target, credential) = self
            .workspace_forge_rest_context(
                owner,
                &workspace,
                lender.as_ref(),
                self.workspace_acts_as(owner, workspace.id).await,
            )
            .await?
            .ok_or_else(|| {
                ServerError::unprocessable_kind(
                    "git_forge_refused",
                    "Connect GitHub before reading pull request comments.",
                )
            })?;
        let api_base = self.forge_api_base_for(&target.host);
        let number = match workspace.pr.as_ref() {
            Some(pr) => pr.number,
            None => {
                let values = crate::code::forge_rest::list_pull_requests_for_head(
                    &api_base,
                    &target,
                    &credential,
                    &workspace.branch_name,
                )
                .await
                .map_err(|error| ServerError::unprocessable_kind("pr_comments_failed", error))?;
                let pull = values
                    .iter()
                    .find(|value| value["state"].as_str() == Some("open"))
                    .or_else(|| values.first());
                pull.and_then(|value| value["number"].as_u64())
                    .ok_or_else(|| {
                        ServerError::not_found("No pull request exists for this branch.")
                    })?
            }
        };
        let prefix = format!("repos/{}/{}", target.owner, target.name);
        let issue_endpoint = format!("{prefix}/issues/{number}/comments?per_page=100");
        let reviews_endpoint = format!("{prefix}/pulls/{number}/reviews?per_page=100");
        let inline_endpoint = format!("{prefix}/pulls/{number}/comments?per_page=100");
        let (issues, reviews, inline) = tokio::try_join!(
            crate::code::forge_rest::api_get(&api_base, &credential, &issue_endpoint),
            crate::code::forge_rest::api_get(&api_base, &credential, &reviews_endpoint),
            crate::code::forge_rest::api_get(&api_base, &credential, &inline_endpoint),
        )
        .map_err(|error| ServerError::unprocessable_kind("pr_comments_failed", error))?;
        let view = serde_json::json!({
            "number": number,
            "comments": issues,
            "reviews": reviews,
        });
        let (_, mut comments) = gh::parse_pr_view_comments(&view.to_string());
        comments.extend(gh::parse_review_comments(&inline.to_string()));
        comments.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(gh::PrComments { number, comments })
    }

    /// Every pull request attributed to the workspace, from the durable fact
    /// store (decision 77): open first, then newest activity. No host read.
    pub async fn workspace_pull_requests(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<
        Vec<(
            tidebreak_core::CodePullRequestFact,
            tidebreak_core::CodePullRequestRelation,
        )>,
        ServerError,
    > {
        // The read authorizes through the workspace row: another owner's
        // workspace is indistinguishable from a missing one.
        let _ = self.get_workspace(owner, id).await?;
        let mut facts =
            tidebreak_core::db::code::list_attributed_facts_for_workspace(&self.db, owner, id)
                .await?;
        facts.sort_by(|(left, _), (right, _)| {
            let left_open = left.state == tidebreak_core::CodePullRequestState::Open;
            let right_open = right.state == tidebreak_core::CodePullRequestState::Open;
            right_open
                .cmp(&left_open)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        Ok(facts)
    }

    /// Merge the exact pull request head the desktop confirmed.
    ///
    /// The workspace turn lock covers every mutable local and host preflight
    /// plus the repository-qualified merge invocation. The final helper also
    /// sends `--match-head-commit`, so a force push after the live read fails
    /// at GitHub instead of changing what this request lands.
    pub async fn merge_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        target: CodeDeliveryPullRequestTarget,
        expected_head_sha: String,
        method: gh::MergeMethod,
        auto: bool,
    ) -> Result<WorkspaceMergeOutcome, ServerError> {
        validate_workspace_merge_request(&target, &expected_head_sha)?;
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::Path::new(&workspace.worktree_path);
        let local = gh::inspect_workspace_merge_local_state(worktree)
            .await
            .map_err(map_gh)?;
        let current_branch = local.current_branch.ok_or_else(|| {
            ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace is detached; check out {} and refresh before merging",
                    workspace.branch_name
                ),
            )
        })?;
        if current_branch != workspace.branch_name {
            return Err(ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace branch changed from {} to {current_branch}; refresh before merging",
                    workspace.branch_name
                ),
            ));
        }
        if local.dirty {
            return Err(ServerError::conflict_kind(
                "workspace_dirty",
                "the workspace now has uncommitted changes; review them before merging",
            ));
        }
        let upstream = local.upstream.ok_or_else(|| {
            ServerError::conflict_kind(
                "workspace_upstream_missing",
                "the workspace branch no longer has an upstream; push it and refresh before merging",
            )
        })?;
        let expected_upstream = format!("origin/{}", workspace.branch_name);
        if upstream != expected_upstream {
            return Err(ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace branch now tracks {upstream} instead of {expected_upstream}; refresh before merging"
                ),
            ));
        }
        if local.ahead_of_upstream > 0 {
            return Err(ServerError::conflict_kind(
                "workspace_unpushed",
                format!(
                    "the workspace now has {} unpushed commit{}; push and refresh before merging",
                    local.ahead_of_upstream,
                    if local.ahead_of_upstream == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            ));
        }

        let local_target = crate::code::delivery::repository_target_from_path(worktree)
            .await
            .map_err(|message| {
                ServerError::conflict_kind(
                    "workspace_repository_changed",
                    format!("the workspace repository could not be verified: {message}"),
                )
            })?;
        if !same_repository(&local_target, &target.repository) {
            return Err(ServerError::conflict_kind(
                "workspace_repository_changed",
                format!(
                    "the workspace repository changed from {} to {}; refresh before merging",
                    repository_label(&target.repository),
                    repository_label(&local_target)
                ),
            ));
        }

        let gh_path = self.gh_search_path_owned();
        let live = gh::view_workspace_pull_request(worktree, gh_path.as_deref())
            .await
            .map_err(map_gh)?;
        if !same_repository(&live.target, &target.repository) || live.number != target.number {
            return Err(ServerError::conflict_kind(
                "pr_target_changed",
                format!(
                    "the workspace now resolves to {}#{} instead of {}#{}; refresh before merging",
                    repository_label(&live.target),
                    live.number,
                    repository_label(&target.repository),
                    target.number
                ),
            ));
        }
        if live.head_branch != workspace.branch_name {
            return Err(ServerError::conflict_kind(
                "pr_target_changed",
                format!(
                    "pull request #{} now uses branch {} instead of {}; refresh before merging",
                    target.number, live.head_branch, workspace.branch_name
                ),
            ));
        }
        if live.state != "open" {
            return Err(ServerError::conflict_kind(
                "pr_not_mergeable",
                format!(
                    "pull request #{} is {}; refresh before merging",
                    target.number, live.state
                ),
            ));
        }
        if local.head_sha != expected_head_sha {
            return Err(pr_head_changed(&expected_head_sha, &local.head_sha));
        }
        if live.head_sha != expected_head_sha {
            return Err(pr_head_changed(&expected_head_sha, &live.head_sha));
        }

        gh::merge_pull_request_target(
            &target.repository.host,
            &target.repository.owner,
            &target.repository.name,
            target.number,
            method,
            auto,
            false,
            &expected_head_sha,
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        drop(_turn_guard);
        // A merge dirties the row (decision 66); the delivery lists hold the
        // pre-merge row.
        self.delivery_cache.invalidate();
        let status = self.refresh_workspace_pr(owner, id).await?;
        Ok(WorkspaceMergeOutcome {
            target,
            accepted_head_sha: expected_head_sha,
            status,
        })
    }

    /// Take the workspace's pull request out of draft and return a fresh
    /// status. Decision 42 keeps pull-request state changes on a user-initiated
    /// endpoint rather than on any agent or automation path, so this is the
    /// only route to `gh pr ready` for a workspace.
    pub async fn mark_workspace_pr_ready(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let gh_path = self.gh_search_path_owned();
        gh::mark_workspace_pull_request_ready(
            std::path::Path::new(&workspace.worktree_path),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        self.delivery_cache.invalidate();
        self.refresh_workspace_pr(owner, id).await
    }

    pub async fn create_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        // On a hosted machine the pull request rides the forge REST API with
        // a borrowed credential (decision 65) and lands as the caller; the
        // authored fact comes straight from the creation answer, with no
        // second host read. Everywhere else `gh` does exactly what it always
        // has (decision 34), including its own best-effort fact read below.
        let acts_as = self.workspace_acts_as(owner, id).await;
        let (digest, rest_fact) = match self.forge_rest_context(owner, &worktree, acts_as).await? {
            Some((target, credential)) => {
                let api_base = self.forge_api_base_for(&target.host);
                let (digest, fact) = gh::create_pull_request_rest(
                    &worktree,
                    &workspace.title,
                    &workspace.branch_name,
                    &workspace.base_ref,
                    title.as_deref(),
                    body.as_deref(),
                    &api_base,
                    &target,
                    &credential,
                )
                .await
                .map_err(map_gh)?;
                (digest, Some((target, fact)))
            }
            None => {
                let gh_path = self.gh_search_path_owned();
                let digest = gh::create_pull_request(
                    &worktree,
                    &workspace.title,
                    &workspace.branch_name,
                    &workspace.base_ref,
                    title.as_deref(),
                    body.as_deref(),
                    gh_path.as_deref(),
                )
                .await
                .map_err(map_gh)?;
                (digest, None)
            }
        };
        self.delivery_cache.invalidate();
        let created_number = digest.number;
        self.save_created_workspace_pr(&workspace, digest).await?;
        // Best-effort authored fact (decision 77). The digest just came from
        // the host; the REST path already holds the full row, and the `gh`
        // path re-reads it repository-qualified for full identity and
        // timestamps. Failures are silent; the reconcile sweep corrects.
        let authored = if let Some((target, fact)) = rest_fact {
            crate::code::pr_facts::record_confirmed_fact(
                &self.db,
                owner,
                workspace.id,
                None,
                None,
                &target,
                &fact,
                tidebreak_core::CodePullRequestRelation::Authored,
                tidebreak_core::CodePullRequestDiscovery::Command,
            )
            .await
        } else if let Ok(target) =
            crate::code::delivery::repository_target_from_path(&worktree).await
        {
            let gh_path = self.gh_search_path_owned();
            if let Ok(value) = gh::view_pull_request_raw(
                &target.host,
                &target.owner,
                &target.name,
                created_number,
                gh_path.as_deref(),
            )
            .await
            {
                crate::code::pr_facts::record_confirmed_fact(
                    &self.db,
                    owner,
                    workspace.id,
                    None,
                    None,
                    &target,
                    &value,
                    tidebreak_core::CodePullRequestRelation::Authored,
                    tidebreak_core::CodePullRequestDiscovery::Command,
                )
                .await
            } else {
                None
            }
        } else {
            None
        };
        if let Some(applied) = authored {
            self.publish_pull_request_read(owner, &applied).await;
        }
        // Creation dirties the row (decision 66): the response carries the
        // fetched digest — checks pending on the fresh pull request — not
        // the light creation stub.
        self.refresh_workspace_pr(owner, id).await
    }

    /// Point the workspace at the pull request it just created, before the
    /// refresh, so the refresh can name it. The column only takes the
    /// creation answer while it shows some other pull request: a projection
    /// of the merged row is never older than a creation answer.
    async fn save_created_workspace_pr(
        &self,
        workspace: &CodeWorkspace,
        digest: PullRequestDigest,
    ) -> Result<(), ServerError> {
        if tidebreak_core::db::code::adopt_workspace_pull_request(
            &self.db,
            &workspace.owner,
            workspace.id,
            &digest,
        )
        .await?
        {
            crate::code::attention::emit_workspace_digests(
                &self.db,
                &self.bus,
                &workspace.owner,
                workspace.id,
            )
            .await;
        }
        Ok(())
    }

    pub async fn run_workspace_action(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        name: &str,
    ) -> Result<ActionOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        gh::run_named_action(
            std::path::Path::new(&workspace.worktree_path),
            &repo.quick_actions,
            name,
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn require_live_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
        if !std::path::Path::new(&workspace.worktree_path).exists() {
            return Err(ServerError::not_found("workspace worktree is gone"));
        }
        Ok(workspace)
    }

    pub fn gh_search_path_owned(&self) -> Option<String> {
        #[cfg(any(test, feature = "test-support"))]
        {
            return self.gh_search_path.lock().expect("gh search path").clone();
        }
        #[cfg(not(any(test, feature = "test-support")))]
        None
    }
}

#[cfg(test)]
mod remote_pr_tests {
    use super::*;
    use crate::obo_gateway::test_support::FakeLender;
    use tidebreak_core::db::code::resolve_external_machine_session;

    async fn fixture(root: &Path) -> (CodeRuntime, Arc<FakeLender>, OwnerId, CodeWorkspace) {
        fixture_with_host(root, Some("github.com")).await
    }

    async fn fixture_with_host(
        root: &Path,
        host: Option<&str>,
    ) -> (CodeRuntime, Arc<FakeLender>, OwnerId, CodeWorkspace) {
        fixture_with(
            root,
            host,
            Some(shown_pr("https://github.com/acme/tools/pull/17")),
        )
        .await
    }

    /// The light digest a workspace column holds before any read enriched it.
    fn shown_pr(url: &str) -> PullRequestDigest {
        serde_json::from_value(serde_json::json!({
            "number": 17,
            "url": url,
            "state": "open",
            "title": "Stored title"
        }))
        .unwrap()
    }

    async fn fixture_with(
        root: &Path,
        host: Option<&str>,
        shown: Option<PullRequestDigest>,
    ) -> (CodeRuntime, Arc<FakeLender>, OwnerId, CodeWorkspace) {
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            root.join("remote-pr.db").display()
        ))
        .await
        .unwrap();
        let lender = Arc::new(FakeLender::offering("forge[bot]"));
        let runtime = CodeRuntime::new(
            Arc::new(db),
            root.to_owned(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_git_credentials(lender.clone());
        runtime.set_gh_search_path(Some(root.join("no-gh").display().to_string()));
        let owner = OwnerId::local();
        let repo = CodeRepo {
            id: RepoId::new(),
            owner: owner.clone(),
            root_path: root.join("missing-repository").display().to_string(),
            display_name: "tools".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: host.map(str::to_owned),
            origin_owner: Some("acme".into()),
            origin_name: Some("tools".into()),
        };
        insert_repo(&runtime.db, &repo).await.unwrap();
        let mut workspace = runtime
            .build_remote_workspace(&owner, &repo, Some("Remote delivery".into()))
            .await
            .unwrap();
        workspace.pr = shown;
        insert_workspace(&runtime.db, &workspace).await.unwrap();
        (runtime, lender, owner, workspace)
    }

    async fn bind(runtime: &CodeRuntime, owner: &OwnerId, workspace: WorkspaceId, identity: &str) {
        let (grant, _) = runtime
            .mint_adapter_grant(owner, "slack", identity, "T-TEST")
            .await
            .unwrap();
        let session = CodeRuntime::remote_session_value(
            owner,
            None,
            workspace,
            HarnessKind::Codex,
            NewSessionSettings::default(),
        );
        resolve_external_machine_session(&runtime.db, owner, grant.id, "slack", identity, &session)
            .await
            .unwrap();
    }

    const MINT: tidebreak_core::db::code::PullRequestReadOptions =
        tidebreak_core::db::code::PullRequestReadOptions {
            mint_row: true,
            adopt: None,
        };

    /// A read of pull request 17 at host version `version`, observed at
    /// `observed_at`: the object alone, as a hosted REST list returns it.
    fn read_17(
        owner: &OwnerId,
        version: chrono::DateTime<Utc>,
        observed_at: chrono::DateTime<Utc>,
    ) -> tidebreak_core::PullRequestRead {
        let mut read =
            tidebreak_core::PullRequestRead::new(owner.clone(), "github.com", "acme", "tools", 17);
        read.object = Some(tidebreak_core::PullRequestObjectRead {
            snapshot: tidebreak_core::PullRequestSnapshot {
                url: "https://github.com/acme/tools/pull/17".into(),
                title: "Stored title".into(),
                state: tidebreak_core::CodePullRequestState::Open,
                draft: false,
                author: None,
                head_branch: "feat".into(),
                base_branch: "main".into(),
                head_sha: Some("abc".into()),
                created_at: version,
                updated_at: version,
                merged_at: None,
                closed_at: None,
            },
            mergeability: None,
            auto_merge_enabled: Some(false),
            observed_at,
            etag: None,
        });
        read
    }

    fn review(
        decision: Option<&str>,
        observed_at: chrono::DateTime<Utc>,
    ) -> tidebreak_core::PullRequestReviewRead {
        tidebreak_core::PullRequestReviewRead {
            decision: decision.map(ToOwned::to_owned),
            observed_at,
            etag: None,
        }
    }

    async fn stored_17(
        runtime: &CodeRuntime,
        owner: &OwnerId,
    ) -> tidebreak_core::CodePullRequestFact {
        tidebreak_core::db::code::get_pull_request_fact(
            &runtime.db,
            owner,
            "github.com",
            "acme",
            "tools",
            17,
        )
        .await
        .unwrap()
        .unwrap()
    }

    /// Issues 3339 and 3364: a hosted REST list read loads no reviews and no
    /// mergeability. It must keep what the fetcher stored, in the row and in
    /// the column of the workspace that shows the pull request.
    #[tokio::test]
    async fn a_hosted_rest_read_keeps_what_the_fetcher_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _, owner, workspace) = fixture(dir.path()).await;
        let version = Utc::now();
        let at = |seconds| version + chrono::Duration::seconds(seconds);

        let mut fetched = read_17(&owner, version, at(1));
        fetched.object.as_mut().unwrap().mergeability =
            Some(tidebreak_core::PullRequestMergeability {
                mergeable: Some("conflicting".into()),
                merge_state_status: Some("dirty".into()),
            });
        fetched.review = Some(review(Some("changes_requested"), at(1)));
        let minted = runtime
            .apply_pull_request_read(&fetched, MINT)
            .await
            .unwrap();
        assert!(minted.stored && minted.changed);
        assert_eq!(
            minted.workspaces,
            vec![workspace.id],
            "the workspace showing the pull request takes the projection"
        );

        let listed = runtime
            .apply_pull_request_read(&read_17(&owner, version, at(2)), MINT)
            .await
            .unwrap();
        assert!(
            !listed.changed,
            "a list read that saw nothing new changes nothing"
        );
        assert!(listed.workspaces.is_empty());

        let live = stored_17(&runtime, &owner).await.live.unwrap();
        assert_eq!(live.review_decision.as_deref(), Some("changes_requested"));
        assert_eq!(live.mergeable.as_deref(), Some("conflicting"));
        assert_eq!(live.merge_state_status.as_deref(), Some("dirty"));
        let shown = runtime
            .get_workspace(&owner, workspace.id)
            .await
            .unwrap()
            .pr
            .unwrap();
        assert_eq!(shown.review_decision.as_deref(), Some("changes_requested"));
        assert_eq!(shown.mergeable.as_deref(), Some("conflicting"));
    }

    /// Review decisions land in the order they were observed, whatever order
    /// they arrive in: a late older decision and a read that never loaded
    /// reviews change nothing, and a newer read that found no decision
    /// clears one.
    #[tokio::test]
    async fn review_decisions_land_in_observation_order() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _, owner, _) = fixture(dir.path()).await;
        let version = Utc::now();
        let at = |seconds| version + chrono::Duration::seconds(seconds);
        let apply = |decision: Option<Option<&str>>, observed: i64| {
            let mut read = read_17(&owner, version, at(observed));
            read.review = decision.map(|decision| review(decision, at(observed)));
            read
        };

        for read in [
            apply(Some(Some("changes_requested")), 1),
            apply(Some(Some("approved")), 3),
            // Observed before the approval, landing after it.
            apply(Some(Some("changes_requested")), 2),
            // A hosted REST read: reviews were never asked for.
            apply(None, 4),
        ] {
            runtime.apply_pull_request_read(&read, MINT).await.unwrap();
        }
        assert_eq!(
            stored_17(&runtime, &owner)
                .await
                .live
                .unwrap()
                .review_decision
                .as_deref(),
            Some("approved")
        );

        runtime
            .apply_pull_request_read(&apply(Some(None), 5), MINT)
            .await
            .unwrap();
        assert_eq!(
            stored_17(&runtime, &owner)
                .await
                .live
                .unwrap()
                .review_decision,
            None,
            "a newer read that loaded reviews and found no decision clears it"
        );
    }

    #[tokio::test]
    async fn a_created_digest_stays_on_the_column_when_the_refresh_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _, owner, workspace) =
            fixture_with(dir.path(), Some("unsupported.example"), None).await;
        let mut created = shown_pr("https://github.com/acme/tools/pull/17");
        created.title = Some("Created title".into());
        runtime
            .save_created_workspace_pr(&workspace, created)
            .await
            .unwrap();
        assert!(runtime
            .refresh_workspace_pr(&owner, workspace.id)
            .await
            .is_err());

        let stored = runtime.get_workspace(&owner, workspace.id).await.unwrap();
        assert_eq!(
            stored.pr.as_ref().unwrap().title.as_deref(),
            Some("Created title")
        );
        let status = runtime.workspace_pr(&owner, workspace.id).await.unwrap();
        assert_eq!(status.pr.as_ref().unwrap().review_decision, None);
    }

    /// A creation answer seeds a column that shows another pull request, and
    /// never replaces a column that already shows this one: whatever is
    /// there came from a read at least as new.
    #[tokio::test]
    async fn a_created_digest_only_seeds_a_column_that_shows_another_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _, owner, workspace) = fixture_with(
            dir.path(),
            Some("github.com"),
            Some(shown_pr("https://github.com/acme/tools/pull/9")),
        )
        .await;
        let mut created = shown_pr("https://github.com/acme/tools/pull/17");
        created.review_decision = Some("approved".into());
        runtime
            .save_created_workspace_pr(&workspace, created)
            .await
            .unwrap();
        let shown = runtime
            .get_workspace(&owner, workspace.id)
            .await
            .unwrap()
            .pr;
        assert_eq!(
            shown.as_ref().and_then(|pr| pr.review_decision.as_deref()),
            Some("approved")
        );

        let mut again = shown_pr("https://github.com/acme/tools/pull/17");
        again.title = Some("A late creation answer".into());
        runtime
            .save_created_workspace_pr(&workspace, again)
            .await
            .unwrap();
        assert_eq!(
            runtime
                .get_workspace(&owner, workspace.id)
                .await
                .unwrap()
                .pr,
            shown
        );
    }

    #[tokio::test]
    async fn remote_status_preserves_the_pr_without_inventing_local_git_facts() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, lender, owner, workspace) = fixture(dir.path()).await;
        assert!(!Path::new(&workspace.worktree_path).exists());
        let status = runtime.workspace_pr(&owner, workspace.id).await.unwrap();
        assert!(status.remote);
        assert!(status.git.is_none());
        assert_eq!(status.pr, workspace.pr);
        assert_eq!(status.pushes_as.as_deref(), Some("forge[bot]"));
        assert_eq!(status.pushes_as_self, Some(false));
        assert!(lender.minted().is_empty());
    }

    #[tokio::test]
    async fn remote_reads_refuse_a_missing_original_connection_without_browser_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, lender, owner, workspace) = fixture(dir.path()).await;
        bind(&runtime, &owner, workspace.id, "U-ONE").await;
        let error = runtime
            .workspace_pr(&owner, workspace.id)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "external_reconnect_required");
        let error = runtime
            .workspace_pr_comments(&owner, workspace.id)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "external_reconnect_required");
        assert!(runtime
            .refresh_workspace_pr(&owner, workspace.id)
            .await
            .is_err());
        assert!(lender.minted().is_empty());
    }

    #[tokio::test]
    async fn remote_reads_refuse_conflicting_original_connections() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, lender, owner, workspace) = fixture(dir.path()).await;
        bind(&runtime, &owner, workspace.id, "U-ONE").await;
        bind(&runtime, &owner, workspace.id, "U-TWO").await;
        let error = runtime
            .workspace_pr(&owner, workspace.id)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "external_connection_conflict");
        let error = runtime
            .workspace_pr_comments(&owner, workspace.id)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "external_connection_conflict");
        assert!(lender.minted().is_empty());
    }

    #[tokio::test]
    async fn remote_refresh_refuses_unknown_or_unsupported_origins_before_lending() {
        for host in [Some("evil.example"), None] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, lender, owner, workspace) = fixture_with_host(dir.path(), host).await;
            assert!(runtime
                .refresh_workspace_pr(&owner, workspace.id)
                .await
                .is_err());
            assert!(runtime
                .workspace_pr_comments(&owner, workspace.id)
                .await
                .is_err());
            assert!(lender.minted().is_empty());
        }
    }

    #[tokio::test]
    async fn remote_reads_refuse_a_pr_outside_the_registered_repository_before_lending() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, lender, owner, workspace) = fixture_with(
            dir.path(),
            Some("github.com"),
            Some(shown_pr("https://github.com/other/repository/pull/17")),
        )
        .await;
        assert!(runtime
            .refresh_workspace_pr(&owner, workspace.id)
            .await
            .is_err());
        let error = runtime
            .workspace_pr_comments(&owner, workspace.id)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_pr_target_changed");
        assert!(lender.minted().is_empty());
    }

    #[tokio::test]
    async fn remote_refresh_and_discussion_use_the_registered_repository_over_rest() {
        use axum::extract::State;
        use axum::http::{HeaderMap, Uri};
        use tidebreak_core::PullRequestCommentKind;
        type Seen = Arc<Mutex<Vec<String>>>;
        async fn forge(
            State((seen, branch)): State<(Seen, String)>,
            uri: Uri,
            headers: HeaderMap,
        ) -> axum::Json<serde_json::Value> {
            assert_eq!(
                headers.get("authorization").unwrap(),
                "Bearer ghs_fake_borrowed"
            );
            let path = uri.path();
            seen.lock().unwrap().push(path.to_owned());
            let value = match path {
                "/repos/acme/tools/pulls" => {
                    assert!(uri
                        .query()
                        .unwrap_or_default()
                        .contains(&format!("head=acme:{branch}")));
                    serde_json::json!([{
                        "number": 17,
                        "html_url": "https://github.com/acme/tools/pull/17",
                        "title": "Fresh title",
                        "state": "open",
                        "head": {"ref": branch, "sha": "feedfeed"},
                        "base": {"ref": "main"}
                    }])
                }
                "/repos/acme/tools/pulls/17" => serde_json::json!({
                    "number": 17,
                    "html_url": "https://github.com/acme/tools/pull/17",
                    "title": "Fresh title",
                    "state": "open",
                    "draft": false,
                    "user": {"login": "author"},
                    "head": {"ref": branch, "sha": "feedfeed"},
                    "base": {"ref": "main"},
                    "merged": false,
                    "mergeable": true,
                    "mergeable_state": "clean",
                    "created_at": "2026-09-04T10:00:00Z",
                    "updated_at": "2026-09-04T10:00:00Z"
                }),
                "/repos/acme/tools/commits/feedfeed/check-runs" => {
                    serde_json::json!({"check_runs": []})
                }
                "/repos/acme/tools/issues/17/comments" => serde_json::json!([{
                    "id": 1, "body": "Issue comment", "user": {"login": "issue-author", "avatar_url": "https://avatars.example/1"},
                    "html_url": "https://github.com/acme/tools/pull/17#issuecomment-1",
                    "created_at": "2026-09-04T11:00:00Z"
                }]),
                "/repos/acme/tools/pulls/17/reviews" => serde_json::json!([{
                    "id": 2, "body": "Review body", "state": "CHANGES_REQUESTED", "user": {"login": "review-author"},
                    "html_url": "https://github.com/acme/tools/pull/17#pullrequestreview-2",
                    "submitted_at": "2026-09-04T10:00:00Z"
                }]),
                "/repos/acme/tools/pulls/17/comments" => serde_json::json!([{
                    "id": 3, "body": "Inline comment", "user": {"login": "inline-author"},
                    "html_url": "https://github.com/acme/tools/pull/17#discussion_r3",
                    "created_at": "2026-09-04T12:00:00Z", "path": "src/lib.rs", "line": 4
                }]),
                "/repos/acme/tools/issues/17/timeline"
                | "/repos/acme/tools/rules/branches/main" => serde_json::json!([]),
                other => panic!("unexpected forge request: {other}"),
            };
            axum::Json(value)
        }
        let dir = tempfile::tempdir().unwrap();
        // No pull request yet: the refresh finds it by the workspace branch.
        let (runtime, lender, owner, workspace) =
            fixture_with(dir.path(), Some("github.com"), None).await;
        let seen: Seen = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .fallback(forge)
            .with_state((seen.clone(), workspace.branch_name.clone()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        runtime.set_forge_api_base(Some(base));
        let refreshed = runtime
            .refresh_workspace_pr(&owner, workspace.id)
            .await
            .unwrap();
        assert!(refreshed.remote);
        assert!(refreshed.git.is_none());
        assert_eq!(refreshed.pr.unwrap().title.as_deref(), Some("Fresh title"));
        let attributed = runtime
            .workspace_pull_requests(&owner, workspace.id)
            .await
            .unwrap();
        assert_eq!(attributed.len(), 1);
        assert_eq!(attributed[0].0.number, 17);
        assert_eq!(attributed[0].0.head_branch, workspace.branch_name);
        assert_eq!(
            attributed[0].1,
            tidebreak_core::CodePullRequestRelation::Contributed
        );
        // The confirmed row starts with the live state the fetch observed,
        // not a bare snapshot.
        let live = attributed[0].0.live.clone().unwrap();
        assert_eq!(live.review_decision.as_deref(), Some("changes_requested"));
        assert_eq!(live.mergeable.as_deref(), Some("mergeable"));
        assert_eq!(live.merge_state_status.as_deref(), Some("clean"));
        assert_eq!(live.checks, Some(Vec::new()));
        assert_eq!(live.in_merge_queue, Some(false));
        let comments = runtime
            .workspace_pr_comments(&owner, workspace.id)
            .await
            .unwrap();
        assert_eq!(comments.number, 17);
        assert_eq!(comments.comments.len(), 3);
        assert_eq!(comments.comments[0].kind, PullRequestCommentKind::Review);
        assert_eq!(
            comments.comments[0].author.as_deref(),
            Some("review-author")
        );
        assert_eq!(
            comments.comments[0].review_state.as_deref(),
            Some("changes_requested")
        );
        assert_eq!(comments.comments[1].kind, PullRequestCommentKind::Issue);
        assert_eq!(comments.comments[1].author.as_deref(), Some("issue-author"));
        assert_eq!(
            comments.comments[1].avatar_url.as_deref(),
            Some("https://avatars.example/1")
        );
        assert_eq!(comments.comments[2].kind, PullRequestCommentKind::Inline);
        assert_eq!(comments.comments[2].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(comments.comments[2].line, Some(4));
        assert!(comments
            .comments
            .iter()
            .all(|comment| comment.url.is_some() && comment.created_at.is_some()));
        assert_eq!(lender.minted(), ["acme/tools", "acme/tools"]);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|path| path.starts_with("/repos/acme/tools/")));
        server.abort();
    }
}
