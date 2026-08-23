//! Reconcile sweep: keep pull-request facts fresh and complete (decision 62).
//!
//! The post-turn detector catches the acts it can see. This sweep owns the
//! rest: it re-reads every tracked repository through the delivery read path
//! on its own interval, which refreshes fact snapshots, mints exact-tier
//! attribution the detector missed (auxiliary terminals, forks landing in
//! tracked repositories, pushes that became pull requests later), and keeps
//! `code_repo`'s origin identity current. Fact persistence itself lives on
//! the delivery read path, so a user-driven page read and a sweep tick do
//! the same work through the same seam.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tracing::debug;

use tidebreak_core::db::code::{
    list_fact_repo_identities_all_owners, list_repos_all_owners, set_repo_origin,
};
use tidebreak_core::{CodePullRequestFact, CodePullRequestId, CodePullRequestState, OwnerId};

use super::delivery::{query_pull_requests, repository_target_from_local, MAX_REPOSITORIES};
use super::runtime::CodeRuntime;
use crate::routes::code::types::{
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestSummary, CodeGitHubRepositoryTarget,
};

/// Coprime with the 47s watch and 53s trigger sweeps, so three GitHub-reading
/// sweeps never land on the same tick.
pub(crate) const RECONCILE_SWEEP_INTERVAL: Duration = Duration::from_secs(61);

/// Abort the reconcile sweep when the runtime is dropped.
///
/// The loop holds a [`Weak`] runtime handle: an `Arc` would keep the runtime
/// alive from its own field and the guard's `Drop` could never run.
pub(crate) struct ReconcileSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl ReconcileSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(RECONCILE_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_reconcile(&runtime).await;
            }
        });
        Self(Some(handle))
    }
}

impl Drop for ReconcileSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// One reconcile pass: per owner, read every tracked repository through the
/// delivery path so facts persist and durable links refresh.
///
/// Tracked means a live registered repository whose origin resolves to
/// GitHub, plus every repository identity that already holds a fact row —
/// which is how a cross-repo pull request the detector observed keeps
/// itself fresh without a local checkout.
pub(crate) async fn sweep_reconcile(runtime: &Arc<CodeRuntime>) {
    let mut targets_by_owner: HashMap<String, Vec<CodeGitHubRepositoryTarget>> = HashMap::new();

    match list_repos_all_owners(&runtime.db).await {
        Ok(repos) => {
            for repo in repos {
                if repo.removed_at.is_some() {
                    continue;
                }
                let target = match repository_target_from_local(&repo).await {
                    Ok(target) => target,
                    Err(reason) => {
                        debug!(repo = %repo.id, "reconcile skipped a repository: {reason}");
                        continue;
                    }
                };
                let stored = (
                    repo.origin_host.as_deref(),
                    repo.origin_owner.as_deref(),
                    repo.origin_name.as_deref(),
                );
                if stored
                    != (
                        Some(target.host.as_str()),
                        Some(target.owner.as_str()),
                        Some(target.name.as_str()),
                    )
                {
                    let _ = set_repo_origin(
                        &runtime.db,
                        &repo.owner,
                        repo.id,
                        &target.host,
                        &target.owner,
                        &target.name,
                    )
                    .await;
                }
                targets_by_owner
                    .entry(repo.owner.as_str().to_owned())
                    .or_default()
                    .push(target);
            }
        }
        Err(err) => {
            debug!("reconcile could not list repositories: {err}");
        }
    }

    match list_fact_repo_identities_all_owners(&runtime.db).await {
        Ok(identities) => {
            for (owner, host, repo_owner, repo_name) in identities {
                targets_by_owner
                    .entry(owner)
                    .or_default()
                    .push(CodeGitHubRepositoryTarget {
                        host,
                        owner: repo_owner,
                        name: repo_name,
                    });
            }
        }
        Err(err) => {
            debug!("reconcile could not list fact identities: {err}");
        }
    }

    for (owner, targets) in targets_by_owner {
        let Ok(owner) = OwnerId::new(&owner) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut deduped: Vec<CodeGitHubRepositoryTarget> = targets
            .into_iter()
            .filter(|target| {
                seen.insert(format!(
                    "{}/{}/{}",
                    target.host.to_ascii_lowercase(),
                    target.owner.to_ascii_lowercase(),
                    target.name.to_ascii_lowercase()
                ))
            })
            .collect();
        if deduped.len() > MAX_REPOSITORIES {
            debug!(
                dropped = deduped.len() - MAX_REPOSITORIES,
                "reconcile capped this owner's repository set"
            );
            deduped.truncate(MAX_REPOSITORIES);
        }
        if deduped.is_empty() {
            continue;
        }
        let query = CodeDeliveryPullRequestQuery {
            repositories: deduped,
            search: None,
            states: Vec::new(),
            review_states: Vec::new(),
            check_states: Vec::new(),
            authors: Vec::new(),
            attention_only: false,
            ready_only: false,
            tidebreak_linked: None,
            updated_after: None,
            cursor: None,
            limit: Some(1),
            refresh: false,
        };
        // A system path: unregistered fact identities must stay readable, so
        // the registered-target check is bypassed the way other sweeps do.
        // Fact persistence and link augmentation happen inside the read.
        if let Err(err) = query_pull_requests(runtime, &owner, true, query).await {
            debug!("reconcile read failed: {err:?}");
        }
    }
}

/// Build a durable fact from one parsed delivery summary.
///
/// `None` when the summary's state token is not one the fact vocabulary
/// stores — the delivery parser already lowercases and normalizes a merged
/// close, so an unknown token here is a contract drift worth staying quiet
/// about rather than guessing.
pub(crate) fn fact_from_summary(
    owner: &OwnerId,
    summary: &CodeDeliveryPullRequestSummary,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<CodePullRequestFact> {
    let state = CodePullRequestState::from_str(&summary.state)?;
    Some(CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: summary.repository.host.clone(),
        repo_owner: summary.repository.owner.clone(),
        repo_name: summary.repository.name.clone(),
        number: summary.number,
        url: summary.url.clone(),
        title: summary.title.clone(),
        state,
        draft: summary.draft,
        author: summary.author.clone(),
        head_branch: summary.head_branch.clone(),
        base_branch: summary.base_branch.clone(),
        head_sha: summary.head_sha.clone(),
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        merged_at: summary.merged_at,
        closed_at: summary.closed_at,
        first_seen_at: now,
        last_seen_at: now,
    })
}

/// Open head branches → pull request numbers, for stack derivation
/// (decision 62). A pull request is stacked on another when its base branch
/// is that pull request's head branch in the same repository; only open
/// parents count, since a merged parent's branch is history, not a base.
pub(crate) fn stack_parents_by_head(facts: &[CodePullRequestFact]) -> HashMap<String, u64> {
    facts
        .iter()
        .filter(|fact| fact.state == CodePullRequestState::Open && !fact.head_branch.is_empty())
        .map(|fact| (fact.head_branch.clone(), fact.number))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::CodePullRequestId;

    fn fact(number: u64, head: &str, state: CodePullRequestState) -> CodePullRequestFact {
        CodePullRequestFact {
            id: CodePullRequestId::new(),
            owner: OwnerId::local(),
            host: "github.com".into(),
            repo_owner: "acme".into(),
            repo_name: "tools".into(),
            number,
            url: format!("https://github.com/acme/tools/pull/{number}"),
            title: format!("PR {number}"),
            state,
            draft: false,
            author: None,
            head_branch: head.into(),
            base_branch: "main".into(),
            head_sha: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            merged_at: None,
            closed_at: None,
            first_seen_at: chrono::Utc::now(),
            last_seen_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn only_open_heads_parent_a_stack() {
        let parents = stack_parents_by_head(&[
            fact(1, "feat/base", CodePullRequestState::Open),
            fact(2, "feat/merged-away", CodePullRequestState::Merged),
            fact(3, "", CodePullRequestState::Open),
        ]);
        assert_eq!(parents.get("feat/base"), Some(&1));
        assert!(!parents.contains_key("feat/merged-away"));
        assert!(!parents.contains_key(""));
    }
}
