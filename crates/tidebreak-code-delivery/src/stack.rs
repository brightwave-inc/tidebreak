//! Durable pull-request facts and stack-parent resolution.

use std::collections::{HashMap, HashSet};

use tidebreak_core::{CodePullRequestFact, CodePullRequestId, CodePullRequestState, OwnerId};

use crate::wire::CodeDeliveryPullRequestSummary;

pub fn fact_from_summary(
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
pub struct StackRepositoryIdentity {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl StackRepositoryIdentity {
    pub fn new(host: &str, owner: &str, name: &str) -> Option<Self> {
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
pub struct StackPullRequestIdentity {
    pub base_repository: StackRepositoryIdentity,
    pub number: u64,
}

/// The mutable branch edge that may resolve to an immutable pull request.
///
/// The base repository scopes the pull-request number. The head repository
/// distinguishes same-named branches in forks of that base repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StackParentEdge {
    pub base_repository: StackRepositoryIdentity,
    pub head_repository: Option<StackRepositoryIdentity>,
    pub head_branch: String,
}

impl StackParentEdge {
    pub fn new(
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
pub struct StackParentCandidate {
    pub pull_request: StackPullRequestIdentity,
    pub open: bool,
    pub head_repository: Option<StackRepositoryIdentity>,
    pub head_branch: Option<String>,
}

/// Why a branch edge could not safely resolve to one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackParentUnresolvedReason {
    MissingParent,
    IncompleteHostIdentity,
    Ambiguous {
        candidates: Vec<StackPullRequestIdentity>,
    },
}

/// A stack edge resolves to one immutable pull request or stays explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackParentResolution {
    Resolved(StackPullRequestIdentity),
    Unresolved {
        edge: StackParentEdge,
        reason: StackParentUnresolvedReason,
    },
}

/// Open pull-request heads indexed by full fork-qualified branch identity.
#[derive(Debug, Default)]
pub struct StackParentIndex {
    exact: HashMap<StackParentEdge, Vec<StackPullRequestIdentity>>,
    incomplete_branches: HashSet<(StackRepositoryIdentity, String)>,
    incomplete_repositories: HashSet<StackRepositoryIdentity>,
}

impl StackParentIndex {
    pub fn new(candidates: impl IntoIterator<Item = StackParentCandidate>) -> Self {
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

    pub fn resolve(
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
