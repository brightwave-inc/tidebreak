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
) -> Option<crate::routes::code::types::CodeGitHubRepositoryTarget> {
    let target = crate::code::delivery::repository_target_from_path(worktree)
        .await
        .ok()?;
    target
        .host
        .eq_ignore_ascii_case(gh::GIT_CREDENTIAL_FORGE_HOST)
        .then_some(target)
}

impl CodeRuntime {
    pub(crate) async fn commit_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        gh::commit_all(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.title,
            message.as_deref(),
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn push_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<PushOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let credential = self.borrow_git_credential(owner, &worktree).await?;
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
                crate::code::pr_facts::record_confirmed_fact(
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
                .await;
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
    pub(super) async fn name_workspace_author(&self, owner: &OwnerId, worktree: &std::path::Path) {
        let Some(lender) = self.git_credentials() else {
            return;
        };
        let Ok(identity) = lender.git_forge_identity(owner).await else {
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
    pub(crate) async fn branch_account_name(&self, owner: &OwnerId) -> Option<String> {
        #[cfg(test)]
        // Tests must not inherit the developer machine's `gh` login.
        self.git_credentials()?;
        if let Some(lender) = self.git_credentials() {
            let identity = lender.git_forge_identity(owner).await.ok()?;
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
    ) -> Result<Option<crate::obo_gateway::GitCredential>, ServerError> {
        let Some(lender) = self.git_credentials() else {
            return Ok(None);
        };
        let Some(target) = forge_lending_target(worktree).await else {
            return Ok(None);
        };
        let repository = format!("{}/{}", target.owner, target.name);
        match lender.git_credential(owner, &repository).await {
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
    pub(super) async fn forge_rest_context(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
    ) -> Result<
        Option<(
            crate::routes::code::types::CodeGitHubRepositoryTarget,
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
            .borrow_git_credential(owner, worktree)
            .await?
            .ok_or_else(|| {
                ServerError::unprocessable_kind(
                    "git_forge_refused",
                    "this checkout's origin is not a lendable forge repository",
                )
            })?;
        Ok(Some((target, credential)))
    }

    pub(crate) async fn workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        // Being asked is the attention signal (decision 66): the request
        // path reads local git plus the stored row, and the hot refresher
        // this mark feeds is what keeps the row current while anyone reads.
        self.mark_workspace_pr_hot(owner, workspace.id);
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
                if let Ok(identity) = lender.git_forge_identity(owner).await {
                    match identity.attribution {
                        crate::obo_gateway::GitForgeAttribution::Person { login, .. } => {
                            status.pushes_as = Some(login);
                            status.pushes_as_self = Some(true);
                        }
                        crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                            status.pushes_as = Some(bot_login.unwrap_or(identity.app_name));
                        }
                    }
                }
            }
        }
        if status.pr != workspace.pr {
            workspace.pr = status.pr.clone();
            self.save_workspace(&workspace).await?;
            // A digest that moved is a fresh host observation: write it onto
            // the fact row's live tier and fan the change out (decision 66).
            if let Some(digest) = &status.pr {
                self.record_pull_request_live_state(owner, Some(workspace.id), digest)
                    .await;
            }
        }
        Ok(status)
    }

    /// Force a fresh host read now — the user asked, or a mutation just
    /// moved the pull request — then answer with the refreshed row.
    pub(crate) async fn refresh_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.refresh_workspace_pr_row(owner, id).await;
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
    pub(crate) async fn refresh_workspace_pr_row(&self, owner: &OwnerId, id: WorkspaceId) {
        let Ok(workspace) = self.get_workspace(owner, id).await else {
            return;
        };
        if workspace.status != CodeWorkspaceStatus::Active {
            return;
        }
        let gh_path = self.gh_search_path_owned();
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let digest = match gh::authenticated_gh_binary(gh_path.as_deref()).await {
            Some(binary) => {
                let transport = crate::code::pr_fetch::FetchTransport::Gh {
                    cwd: &worktree,
                    binary: &binary,
                };
                self.fetched_workspace_digest(owner, &workspace, transport)
                    .await
            }
            None => {
                let Ok(Some((target, credential))) =
                    self.forge_rest_context(owner, &worktree).await
                else {
                    return;
                };
                let api_base = self.forge_api_base_for(&target.host);
                let transport = crate::code::pr_fetch::FetchTransport::Rest {
                    api_base: &api_base,
                    credential: &credential,
                };
                self.fetched_workspace_digest(owner, &workspace, transport)
                    .await
            }
        };
        let Some(digest) = digest else {
            return;
        };
        if workspace.pr.as_ref() != Some(&digest) {
            match set_active_workspace_pull_request(&self.db, owner, workspace.id, &digest).await {
                Ok(true) => {
                    crate::code::attention::emit_workspace_digests(
                        &self.db,
                        &self.bus,
                        owner,
                        workspace.id,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::debug!(error = %err, "code-mode: workspace digest write failed");
                }
            }
        }
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
    pub(crate) fn nudge_delivery_update(&self, owner: &OwnerId) {
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
    pub(super) async fn workspace_repository_target(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Option<crate::routes::code::types::CodeGitHubRepositoryTarget> {
        let repo = self.get_repo(owner, workspace.repo_id).await.ok()?;
        if let (Some(host), Some(repo_owner), Some(name)) = (
            repo.origin_host.clone(),
            repo.origin_owner.clone(),
            repo.origin_name.clone(),
        ) {
            return Some(crate::routes::code::types::CodeGitHubRepositoryTarget {
                host,
                owner: repo_owner,
                name,
            });
        }
        crate::code::delivery::repository_target_from_local(&repo)
            .await
            .ok()
    }

    /// The workspace's digest through the conditional fetcher (decision 66),
    /// over whichever transport the caller resolved — `gh` or the hosted
    /// forge REST API.
    ///
    /// Identity comes from the stored digest's URL, or a head lookup when
    /// the workspace knows no pull request yet. Each endpoint sends the
    /// row's stored ETag: a 304 answers from the row for free, and a 200
    /// carries new state. The result lands on the row — live tier, fanout,
    /// and the ETags for next time. `None` leaves the caller's persisted
    /// digest standing: no pull request, a parked host, a failed read, or a
    /// conditional read whose row moved under it.
    pub(super) async fn fetched_workspace_digest(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        transport: crate::code::pr_fetch::FetchTransport<'_>,
    ) -> Option<PullRequestDigest> {
        use crate::code::pr_fetch::{self, EndpointRead};

        let gate = &self.host_gate;
        let stored_identity = workspace
            .pr
            .as_ref()
            .and_then(|pr| pr.url.as_deref())
            .and_then(crate::code::pr_facts::pull_request_identity_from_url);
        let (host, repo_owner, repo_name, number) = match stored_identity {
            Some(identity) => identity,
            None => {
                let target = self.workspace_repository_target(owner, workspace).await?;
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
                    Ok(found) => found?,
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: pull-request lookup skipped");
                        return None;
                    }
                };
                (target.host, target.owner, target.name, found.number)
            }
        };
        let stored = tidebreak_core::db::code::get_pull_request_fetch_state(
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
        let (stored_fact, mut pull_etag, mut checks_etag, mut reviews_etag) = match stored {
            Some(state) => (
                Some(state.fact),
                state.pull_etag,
                state.checks_etag,
                state.reviews_etag,
            ),
            None => (None, None, None, None),
        };
        let sent_pull_etag = pull_etag.clone();
        let (pull, fresh_pull) = match pr_fetch::read_pull_request(
            gate,
            transport,
            &host,
            &repo_owner,
            &repo_name,
            number,
            pull_etag.as_deref(),
        )
        .await
        {
            Ok(EndpointRead::Fresh { value, etag }) => {
                pull_etag = etag;
                (value, true)
            }
            Ok(EndpointRead::NotModified) => {
                (pr_fetch::rest_pull_from_fact(stored_fact.as_ref()?), false)
            }
            Ok(EndpointRead::Missing) => return None,
            Err(failure) => {
                tracing::debug!(error = %failure, "code-mode: pull-request read skipped");
                return None;
            }
        };
        let stored_live = stored_fact.as_ref().and_then(|fact| fact.live.as_ref());
        let checks = match pull.head_sha.as_deref() {
            Some(sha) => {
                // A checks ETag names one head's answer; a moved head sends
                // an unconditional read.
                let same_head = stored_fact
                    .as_ref()
                    .and_then(|fact| fact.head_sha.as_deref())
                    == Some(sha);
                let conditional = if same_head {
                    checks_etag.as_deref()
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
                    Ok(EndpointRead::Fresh { value, etag }) => {
                        checks_etag = etag;
                        value
                    }
                    Ok(EndpointRead::NotModified) => stored_live
                        .and_then(|live| live.checks.clone())
                        .unwrap_or_default(),
                    Ok(EndpointRead::Missing) => Vec::new(),
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: check-runs read skipped");
                        checks_etag = None;
                        stored_live
                            .and_then(|live| live.checks.clone())
                            .unwrap_or_default()
                    }
                }
            }
            None => Vec::new(),
        };
        let open = pull.state == "open";
        let rules = if open {
            self.branch_rules_for(
                transport,
                &host,
                &repo_owner,
                &repo_name,
                pull.base_branch.as_deref(),
            )
            .await
        } else {
            None
        };
        let review_decision = if open {
            match pr_fetch::read_reviews(
                gate,
                transport,
                &host,
                &repo_owner,
                &repo_name,
                number,
                reviews_etag.as_deref(),
            )
            .await
            {
                Ok(EndpointRead::Fresh { value, etag }) => {
                    reviews_etag = etag;
                    pr_fetch::derive_review_decision(rules, &value)
                }
                Ok(EndpointRead::NotModified) => {
                    stored_live.and_then(|live| live.review_decision.clone())
                }
                Ok(EndpointRead::Missing) => None,
                Err(failure) => {
                    tracing::debug!(error = %failure, "code-mode: reviews read skipped");
                    reviews_etag = None;
                    stored_live.and_then(|live| live.review_decision.clone())
                }
            }
        } else {
            None
        };
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
        let fresh_fact = if fresh_pull {
            stored_fact.clone().map(|mut fact| {
                pr_fetch::apply_fresh_pull_to_fact(&mut fact, &pull, Utc::now());
                fact
            })
        } else {
            None
        };
        let condition = if fresh_pull {
            tidebreak_core::db::code::PullRequestFetchCondition::Unconditional
        } else {
            tidebreak_core::db::code::PullRequestFetchCondition::PullEtag(sent_pull_etag.as_deref())
        };

        let digest = pr_fetch::digest_from_parts(&pull, &checks, review_decision, in_merge_queue);
        // The validator gates every write this read produces, not just the
        // fetch state (issue 2799). A 304 reconstructs its digest from the
        // stored snapshot, so a row that moved under the read leaves that
        // reconstruction describing a pull request that no longer exists.
        // Write the transport hints first: if the row refuses them, the
        // digest never reaches the live tier, the workspace column, or the
        // caller's broadcast.
        let accepted = match tidebreak_core::db::code::set_pull_request_fetch_state(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
            fresh_fact.as_ref(),
            condition,
            pull_etag.as_deref(),
            checks_etag.as_deref(),
            reviews_etag.as_deref(),
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::debug!(error = %err, "code-mode: fetch-state write failed");
                false
            }
        };
        if !fresh_pull && !accepted {
            tracing::debug!(
                host = %host,
                number = number,
                "code-mode: a conditional pull-request read lost its validator; dropping it"
            );
            return None;
        }
        self.record_pull_request_live_state(owner, Some(workspace.id), &digest)
            .await;
        Some(digest)
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
            let cache = self.branch_rules.lock().expect("branch rules");
            if let Some(entry) = cache.get(&key) {
                if entry.fetched_at.elapsed() <= BRANCH_RULES_TTL {
                    return entry.rules;
                }
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
    pub(crate) fn refresh_workspaces_for_pull_request(
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

    /// Write a freshly observed digest onto its decision-62 fact row and fan
    /// real change out (decision 66): every other live workspace holding the
    /// same pull request takes the digest as a write-through copy — column
    /// and digest cache both — and broadcasts, so one host read updates
    /// every surface without a second one. Best-effort: a missing fact row
    /// or a store failure leaves the sweeps to correct.
    pub(crate) async fn record_pull_request_live_state(
        &self,
        owner: &OwnerId,
        source: Option<WorkspaceId>,
        digest: &PullRequestDigest,
    ) {
        let Some(url) = digest.url.as_deref() else {
            return;
        };
        let Some((host, repo_owner, repo_name, number)) =
            crate::code::pr_facts::pull_request_identity_from_url(url)
        else {
            return;
        };
        if number != digest.number {
            return;
        }
        let live = tidebreak_core::CodePullRequestLiveState::from_digest(digest, Utc::now());
        let changed = match tidebreak_core::db::code::set_pull_request_live_state(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
            &live,
        )
        .await
        {
            Ok(Some((_, changed))) => changed,
            // No fact row yet: the detector or the reconcile sweep mints it,
            // and the next digest change lands on it.
            Ok(None) => return,
            Err(err) => {
                tracing::debug!(error = %err, "code-mode: live-tier write failed");
                return;
            }
        };
        if !changed {
            return;
        }
        // One delivery nudge per real change (decision 66): the delivery
        // page and notification monitor re-read on receipt instead of on
        // their own timers.
        self.nudge_delivery_update(owner);
        let workspaces = match list_workspaces(&self.db, owner, None).await {
            Ok(workspaces) => workspaces,
            Err(_) => return,
        };
        for workspace in workspaces {
            if source == Some(workspace.id) || workspace.status != CodeWorkspaceStatus::Active {
                continue;
            }
            let holds = workspace
                .pr
                .as_ref()
                .and_then(|pr| pr.url.as_deref())
                .is_some_and(|value| value.eq_ignore_ascii_case(url));
            if !holds || workspace.pr.as_ref() == Some(digest) {
                continue;
            }
            match set_active_workspace_pull_request(&self.db, owner, workspace.id, digest).await {
                Ok(true) => {
                    crate::code::attention::emit_workspace_digests(
                        &self.db,
                        &self.bus,
                        owner,
                        workspace.id,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(
                        workspace = %workspace.id,
                        error = %err,
                        "code-mode: pull-request write-through failed"
                    );
                }
            }
        }
    }

    pub(crate) async fn workspace_pr_comments(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<gh::PrComments, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        let gh_path = self.gh_search_path_owned();
        gh::load_pr_comments(
            std::path::Path::new(&workspace.worktree_path),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)
    }

    /// Every pull request attributed to the workspace, from the durable fact
    /// store (decision 77): open first, then newest activity. No host read.
    pub(crate) async fn workspace_pull_requests(
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
    pub(crate) async fn merge_workspace_pr(
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
    pub(crate) async fn mark_workspace_pr_ready(
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

    pub(crate) async fn create_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let mut workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        // On a hosted machine the pull request rides the forge REST API with
        // a borrowed credential (decision 65) and lands as the caller; the
        // authored fact comes straight from the creation answer, with no
        // second host read. Everywhere else `gh` does exactly what it always
        // has (decision 34), including its own best-effort fact read below.
        let (digest, rest_fact) = match self.forge_rest_context(owner, &worktree).await? {
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
        workspace.pr = Some(digest);
        self.save_workspace(&workspace).await?;
        // Best-effort authored fact (decision 77). The digest just came from
        // the host; the REST path already holds the full row, and the `gh`
        // path re-reads it repository-qualified for full identity and
        // timestamps. Failures are silent; the reconcile sweep corrects.
        if let Some((target, fact)) = rest_fact {
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
            .await;
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
                .await;
            }
        }
        // Creation dirties the row (decision 66): the response carries the
        // fetched digest — checks pending on the fresh pull request — not
        // the light creation stub.
        self.refresh_workspace_pr(owner, id).await
    }

    pub(crate) async fn run_workspace_action(
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

    pub(super) async fn require_live_workspace(
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

    pub(crate) fn gh_search_path_owned(&self) -> Option<String> {
        #[cfg(test)]
        {
            return self.gh_search_path.lock().expect("gh search path").clone();
        }
        #[cfg(not(test))]
        None
    }
}
