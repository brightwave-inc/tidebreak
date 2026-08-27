//! Reconcile sweep: keep pull-request facts fresh and complete (decision 77).
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

use super::delivery::{MAX_REPOSITORIES, query_pull_requests, repository_target_from_local};
use super::runtime::CodeRuntime;
use crate::routes::code::types::{
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestSummary, CodeGitHubRepositoryTarget,
};

/// Coprime with the 47s watch and 53s trigger sweeps, so three GitHub-reading
/// sweeps never land on the same tick.
pub(crate) const RECONCILE_SWEEP_INTERVAL: Duration = Duration::from_secs(61);

/// A live tier younger than this answers a sweep without a host read
/// (decision 66): two reconcile intervals plus slack, so one missed pass
/// degrades to a fetch rather than a stale verdict.
const LIVE_TIER_FRESH_SECS: i64 = (RECONCILE_SWEEP_INTERVAL.as_secs() as i64) * 2 + 30;

/// Whether a sweep may consume this live tier instead of fetching.
pub(crate) fn live_tier_is_fresh(
    live: &tidebreak_core::CodePullRequestLiveState,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    now - live.observed_at <= chrono::Duration::seconds(LIVE_TIER_FRESH_SECS)
}

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
            repositories: deduped.clone(),
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
        super::delivery::refresh_workflow_runs(runtime, &owner, &deduped).await;
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
        live: None,
    })
}

/// Case-insensitive repository identity used while resolving stack edges.
///
/// GitHub repository owner and name tokens are case-insensitive. Branch names
/// remain case-sensitive and live on [`StackParentEdge`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct StackRepositoryIdentity {
    pub(crate) host: String,
    pub(crate) owner: String,
    pub(crate) name: String,
}

impl StackRepositoryIdentity {
    pub(crate) fn new(host: &str, owner: &str, name: &str) -> Option<Self> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let owner = owner.trim().to_ascii_lowercase();
        let name = name.trim().to_ascii_lowercase();
        let name = name.strip_suffix(".git").unwrap_or(&name).to_owned();
        if host.is_empty() || owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self { host, owner, name })
    }
}

/// Immutable pull-request identity after a stack edge resolves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct StackPullRequestIdentity {
    pub(crate) base_repository: StackRepositoryIdentity,
    pub(crate) number: u64,
}

/// The mutable branch edge that may resolve to an immutable pull request.
///
/// The base repository scopes the pull-request number. The head repository
/// distinguishes same-named branches in forks of that base repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct StackParentEdge {
    pub(crate) base_repository: StackRepositoryIdentity,
    pub(crate) head_repository: Option<StackRepositoryIdentity>,
    pub(crate) head_branch: String,
}

impl StackParentEdge {
    pub(crate) fn new(
        base_repository: StackRepositoryIdentity,
        head_repository: Option<StackRepositoryIdentity>,
        head_branch: &str,
    ) -> Option<Self> {
        if head_branch.is_empty() {
            return None;
        }
        Some(Self {
            base_repository,
            head_repository,
            head_branch: head_branch.to_owned(),
        })
    }
}

/// One possible open stack parent from a host observation or durable fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StackParentCandidate {
    pub(crate) pull_request: StackPullRequestIdentity,
    pub(crate) open: bool,
    pub(crate) head_repository: Option<StackRepositoryIdentity>,
    pub(crate) head_branch: Option<String>,
}

/// Why a branch edge could not safely resolve to one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StackParentUnresolvedReason {
    MissingParent,
    IncompleteHostIdentity,
    Ambiguous {
        candidates: Vec<StackPullRequestIdentity>,
    },
}

/// A stack edge resolves to one immutable pull request or stays explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StackParentResolution {
    Resolved(StackPullRequestIdentity),
    Unresolved {
        edge: StackParentEdge,
        reason: StackParentUnresolvedReason,
    },
}

/// Open pull-request heads indexed by full fork-qualified branch identity.
#[derive(Debug, Default)]
pub(crate) struct StackParentIndex {
    exact: HashMap<StackParentEdge, Vec<StackPullRequestIdentity>>,
    incomplete_branches: HashSet<(StackRepositoryIdentity, String)>,
    incomplete_repositories: HashSet<StackRepositoryIdentity>,
}

impl StackParentIndex {
    pub(crate) fn new(candidates: impl IntoIterator<Item = StackParentCandidate>) -> Self {
        let mut index = Self::default();
        for candidate in candidates {
            if !candidate.open {
                continue;
            }
            match (candidate.head_repository, candidate.head_branch) {
                (Some(head_repository), Some(head_branch)) if !head_branch.is_empty() => {
                    let edge = StackParentEdge {
                        base_repository: candidate.pull_request.base_repository.clone(),
                        head_repository: Some(head_repository),
                        head_branch,
                    };
                    index
                        .exact
                        .entry(edge)
                        .or_default()
                        .push(candidate.pull_request);
                }
                (None, Some(head_branch)) if !head_branch.is_empty() => {
                    index
                        .incomplete_branches
                        .insert((candidate.pull_request.base_repository, head_branch));
                }
                _ => {
                    index
                        .incomplete_repositories
                        .insert(candidate.pull_request.base_repository);
                }
            }
        }
        for candidates in index.exact.values_mut() {
            candidates.sort();
            candidates.dedup();
        }
        index
    }

    pub(crate) fn resolve(
        &self,
        edge: &StackParentEdge,
        child: Option<&StackPullRequestIdentity>,
    ) -> StackParentResolution {
        if edge.head_repository.is_none() {
            return StackParentResolution::Unresolved {
                edge: edge.clone(),
                reason: StackParentUnresolvedReason::IncompleteHostIdentity,
            };
        }
        let mut candidates = self.exact.get(edge).cloned().unwrap_or_default();
        if let Some(child) = child {
            candidates.retain(|candidate| candidate != child);
        }
        let incomplete = self.incomplete_repositories.contains(&edge.base_repository)
            || self
                .incomplete_branches
                .contains(&(edge.base_repository.clone(), edge.head_branch.clone()));
        let reason = if incomplete {
            StackParentUnresolvedReason::IncompleteHostIdentity
        } else if candidates.is_empty() {
            StackParentUnresolvedReason::MissingParent
        } else if candidates.len() == 1 {
            return StackParentResolution::Resolved(
                candidates
                    .pop()
                    .expect("a one-candidate stack edge has a parent"),
            );
        } else {
            StackParentUnresolvedReason::Ambiguous { candidates }
        };
        StackParentResolution::Unresolved {
            edge: edge.clone(),
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(owner: &str, name: &str) -> StackRepositoryIdentity {
        StackRepositoryIdentity::new("github.com", owner, name).unwrap()
    }

    fn parent(
        number: u64,
        base_repository: &StackRepositoryIdentity,
        head_repository: Option<&StackRepositoryIdentity>,
        head_branch: Option<&str>,
    ) -> StackParentCandidate {
        StackParentCandidate {
            pull_request: StackPullRequestIdentity {
                base_repository: base_repository.clone(),
                number,
            },
            open: true,
            head_repository: head_repository.cloned(),
            head_branch: head_branch.map(str::to_owned),
        }
    }

    fn edge(
        base_repository: &StackRepositoryIdentity,
        head_repository: &StackRepositoryIdentity,
        head_branch: &str,
    ) -> StackParentEdge {
        StackParentEdge::new(
            base_repository.clone(),
            Some(head_repository.clone()),
            head_branch,
        )
        .unwrap()
    }

    #[test]
    fn same_named_fork_heads_resolve_to_the_requested_fork() {
        let base = repository("acme", "tools");
        let alice = repository("alice", "tools");
        let bob = repository("bob", "tools");
        let index = StackParentIndex::new([
            parent(41, &base, Some(&alice), Some("stack/base")),
            parent(42, &base, Some(&bob), Some("stack/base")),
        ]);

        assert_eq!(
            index.resolve(&edge(&base, &bob, "stack/base"), None),
            StackParentResolution::Resolved(StackPullRequestIdentity {
                base_repository: base,
                number: 42,
            })
        );
    }

    #[test]
    fn same_named_heads_in_different_base_repositories_stay_isolated() {
        let tools = repository("acme", "tools");
        let web = repository("acme", "web");
        let index = StackParentIndex::new([
            parent(51, &tools, Some(&tools), Some("stack/base")),
            parent(61, &web, Some(&web), Some("stack/base")),
        ]);

        assert_eq!(
            index.resolve(&edge(&web, &web, "stack/base"), None),
            StackParentResolution::Resolved(StackPullRequestIdentity {
                base_repository: web,
                number: 61,
            })
        );
    }

    #[test]
    fn renamed_and_deleted_parent_branches_stay_unresolved() {
        let base = repository("acme", "tools");
        let other_fork = repository("other", "tools");
        let renamed = StackParentIndex::new([
            parent(71, &base, Some(&base), Some("stack/renamed")),
            parent(72, &base, Some(&other_fork), Some("stack/base")),
        ]);
        assert_eq!(
            renamed.resolve(&edge(&base, &base, "stack/base"), None),
            StackParentResolution::Unresolved {
                edge: edge(&base, &base, "stack/base"),
                reason: StackParentUnresolvedReason::MissingParent,
            }
        );

        let deleted = StackParentIndex::new([parent(73, &base, None, Some("stack/base"))]);
        assert_eq!(
            deleted.resolve(&edge(&base, &base, "stack/base"), None),
            StackParentResolution::Unresolved {
                edge: edge(&base, &base, "stack/base"),
                reason: StackParentUnresolvedReason::IncompleteHostIdentity,
            }
        );
    }

    #[test]
    fn duplicate_parent_candidates_are_explicit_and_deterministic() {
        let base = repository("acme", "tools");
        let index = StackParentIndex::new([
            parent(82, &base, Some(&base), Some("stack/base")),
            parent(81, &base, Some(&base), Some("stack/base")),
        ]);

        assert_eq!(
            index.resolve(&edge(&base, &base, "stack/base"), None),
            StackParentResolution::Unresolved {
                edge: edge(&base, &base, "stack/base"),
                reason: StackParentUnresolvedReason::Ambiguous {
                    candidates: vec![
                        StackPullRequestIdentity {
                            base_repository: base.clone(),
                            number: 81,
                        },
                        StackPullRequestIdentity {
                            base_repository: base,
                            number: 82,
                        },
                    ],
                },
            }
        );
    }

    #[test]
    fn an_incomplete_edge_stays_explicitly_unresolved() {
        let base = repository("acme", "tools");
        let index = StackParentIndex::new([parent(83, &base, Some(&base), Some("stack/base"))]);
        let incomplete = StackParentEdge::new(base, None, "stack/base").unwrap();

        assert_eq!(
            index.resolve(&incomplete, None),
            StackParentResolution::Unresolved {
                edge: incomplete,
                reason: StackParentUnresolvedReason::IncompleteHostIdentity,
            }
        );
    }

    #[test]
    fn closed_heads_and_self_edges_do_not_parent_a_stack() {
        let base = repository("acme", "tools");
        let mut closed = parent(91, &base, Some(&base), Some("stack/base"));
        closed.open = false;
        let child = StackPullRequestIdentity {
            base_repository: base.clone(),
            number: 92,
        };
        let index =
            StackParentIndex::new([closed, parent(92, &base, Some(&base), Some("stack/base"))]);

        assert_eq!(
            index.resolve(&edge(&base, &base, "stack/base"), Some(&child)),
            StackParentResolution::Unresolved {
                edge: edge(&base, &base, "stack/base"),
                reason: StackParentUnresolvedReason::MissingParent,
            }
        );
    }
}
