//! One merge for stored pull-request state.
//!
//! Several readers observe the same pull request: the conditional fetcher,
//! the reconcile and delivery reads, the trigger sweep's by-number reads, the
//! post-turn detector, and the create and push confirmations. Each one sees a
//! different subset of fields, and their writes land in any order. Every one
//! of them describes what it saw as a [`PullRequestRead`] and lands it through
//! [`merge_pull_request_read`], so no reader decides on its own which fields
//! it may overwrite.
//!
//! The merge works one field group at a time:
//!
//! - A group the read did not observe keeps its stored value.
//! - A group takes the read's value only when the read observed it later than
//!   the stored value was observed. The pull request object orders by
//!   GitHub's own `updated_at` first and by observation time second, so an
//!   answer from a lagging replica never rolls a newer version back.
//! - Check runs, mergeability, and merge-queue membership describe one head.
//!   A newer object that moves the head clears them, and an observation that
//!   describes any other head never lands.
//!
//! Each group keeps the observation with the latest key, so the same reads
//! applied in any order leave the same state. The one assumption is that a
//! head never returns once replaced: a force-push back to an old commit can
//! leave that commit's older check runs in place.

use chrono::{DateTime, Utc};

use super::{
    CodePullRequestFact, CodePullRequestId, CodePullRequestLiveState, CodePullRequestState,
    PullRequestCheck, PullRequestCheckCounts, PullRequestDigest,
};
use crate::OwnerId;

/// The fields the pull request object itself carries: what it is, where it
/// points, and the host's own timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    /// Web URL.
    pub url: String,
    /// Title.
    pub title: String,
    /// Coarse lifecycle.
    pub state: CodePullRequestState,
    /// Draft flag.
    pub draft: bool,
    /// Author login, when the host reported one.
    pub author: Option<String>,
    /// Head branch name.
    pub head_branch: String,
    /// Base branch name.
    pub base_branch: String,
    /// Head commit.
    pub head_sha: Option<String>,
    /// When the host says the pull request was opened.
    pub created_at: DateTime<Utc>,
    /// When the host says the pull request last changed. Snapshots order by
    /// this first, so a newer host version always wins.
    pub updated_at: DateTime<Utc>,
    /// Merge time, when merged.
    pub merged_at: Option<DateTime<Utc>>,
    /// Close time, when closed.
    pub closed_at: Option<DateTime<Utc>>,
}

impl PullRequestSnapshot {
    /// The snapshot a stored fact holds.
    #[must_use]
    pub fn of_fact(fact: &CodePullRequestFact) -> Self {
        Self {
            url: fact.url.clone(),
            title: fact.title.clone(),
            state: fact.state,
            draft: fact.draft,
            author: fact.author.clone(),
            head_branch: fact.head_branch.clone(),
            base_branch: fact.base_branch.clone(),
            head_sha: fact.head_sha.clone(),
            created_at: fact.created_at,
            updated_at: fact.updated_at,
            merged_at: fact.merged_at,
            closed_at: fact.closed_at,
        }
    }

    fn write_to(&self, fact: &mut CodePullRequestFact) {
        fact.url.clone_from(&self.url);
        fact.title.clone_from(&self.title);
        fact.state = self.state;
        fact.draft = self.draft;
        fact.author.clone_from(&self.author);
        fact.head_branch.clone_from(&self.head_branch);
        fact.base_branch.clone_from(&self.base_branch);
        fact.head_sha.clone_from(&self.head_sha);
        fact.created_at = self.created_at;
        fact.updated_at = self.updated_at;
        fact.merged_at = self.merged_at;
        fact.closed_at = self.closed_at;
    }
}

/// GitHub's mergeability answer. Both halves come from one pull request
/// object and describe its head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestMergeability {
    /// Lowercased host mergeability (mergeable, conflicting, unknown).
    pub mergeable: Option<String>,
    /// Lowercased host merge-state status (clean, blocked, behind, dirty, …).
    pub merge_state_status: Option<String>,
}

/// The pull request object as one read saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestObjectRead {
    /// The object's own fields.
    pub snapshot: PullRequestSnapshot,
    /// Mergeability, when the object carried it. A REST list does not.
    pub mergeability: Option<PullRequestMergeability>,
    /// Auto-merge arming, when the object carried it.
    pub auto_merge_enabled: Option<bool>,
    /// When the read asked for the object.
    pub observed_at: DateTime<Utc>,
    /// The validator the host sent with the object. Only the conditional
    /// fetcher carries one. It becomes the stored validator only when the
    /// object is complete, carrying mergeability and auto-merge, and every
    /// field it carried is what the row now holds.
    pub etag: Option<String>,
}

/// One head's check runs as one read saw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestChecksRead {
    /// The head the checks ran on. It must be the head of the object the same
    /// read loaded.
    pub head_sha: Option<String>,
    /// Every check the host reported. Empty means the host reported none.
    pub checks: Vec<PullRequestCheck>,
    /// When the read asked for the checks.
    pub observed_at: DateTime<Utc>,
    /// The validator the host sent with the checks, when the read was
    /// conditional.
    pub etag: Option<String>,
}

/// The review decision as one read saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestReviewRead {
    /// Lowercased decision, or `None` when no decision applies.
    pub decision: Option<String>,
    /// When the read asked for the reviews.
    pub observed_at: DateTime<Utc>,
    /// The validator the host sent with the reviews, when the read was
    /// conditional.
    pub etag: Option<String>,
}

/// Merge-queue membership as one read saw it for one head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestQueueRead {
    /// The head the membership describes. It must be the head of the object
    /// the same read loaded.
    pub head_sha: Option<String>,
    /// Whether the pull request sits in its merge queue.
    pub in_merge_queue: bool,
    /// When the read asked.
    pub observed_at: DateTime<Utc>,
}

/// Everything one read of one pull request observed.
///
/// A field group left `None` is one the read did not look at, and the merge
/// keeps the stored value for it. A group set to a value is one the read did
/// look at, and "nothing" is then a real answer: an empty check list clears
/// the checks, and a `None` decision clears the review decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestRead {
    /// Principal whose credential made the read.
    pub owner: OwnerId,
    /// Host, e.g. `github.com`.
    pub host: String,
    /// Repository owner login.
    pub repo_owner: String,
    /// Repository name.
    pub repo_name: String,
    /// Pull request number.
    pub number: u64,
    /// The pull request object, when the read loaded it or a 304 confirmed
    /// the stored one.
    pub object: Option<PullRequestObjectRead>,
    /// The head's check runs, when the read loaded them.
    pub checks: Option<PullRequestChecksRead>,
    /// The review decision, when the read loaded or derived it.
    pub review: Option<PullRequestReviewRead>,
    /// Merge-queue membership, when the read learned it.
    pub queue: Option<PullRequestQueueRead>,
}

impl PullRequestRead {
    /// A read that observed nothing yet, for the named pull request.
    #[must_use]
    pub fn new(
        owner: OwnerId,
        host: impl Into<String>,
        repo_owner: impl Into<String>,
        repo_name: impl Into<String>,
        number: u64,
    ) -> Self {
        Self {
            owner,
            host: host.into(),
            repo_owner: repo_owner.into(),
            repo_name: repo_name.into(),
            number,
            object: None,
            checks: None,
            review: None,
            queue: None,
        }
    }

    /// A read that observed every field `fact` carries at `observed_at`: the
    /// snapshot, and each live field its tier holds. Tests seed stored rows
    /// with it.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn observing_fact(fact: &CodePullRequestFact, observed_at: DateTime<Utc>) -> Self {
        let live = fact.live.as_ref();
        let head_sha = fact.head_sha.clone();
        Self {
            owner: fact.owner.clone(),
            host: fact.host.clone(),
            repo_owner: fact.repo_owner.clone(),
            repo_name: fact.repo_name.clone(),
            number: fact.number,
            object: Some(PullRequestObjectRead {
                snapshot: PullRequestSnapshot::of_fact(fact),
                mergeability: live.map(|live| PullRequestMergeability {
                    mergeable: live.mergeable.clone(),
                    merge_state_status: live.merge_state_status.clone(),
                }),
                auto_merge_enabled: live.and_then(|live| live.auto_merge_enabled),
                observed_at,
                etag: None,
            }),
            checks: live
                .and_then(|live| live.checks.clone())
                .map(|checks| PullRequestChecksRead {
                    head_sha: head_sha.clone(),
                    checks,
                    observed_at,
                    etag: None,
                }),
            review: live.map(|live| PullRequestReviewRead {
                decision: live.review_decision.clone(),
                observed_at,
                etag: None,
            }),
            queue: live
                .and_then(|live| live.in_merge_queue)
                .map(|in_merge_queue| PullRequestQueueRead {
                    head_sha,
                    in_merge_queue,
                    observed_at,
                }),
        }
    }
}

/// When each live field group on a stored pull request was observed. `None`
/// means never, or before these times were recorded; any observation is
/// newer than that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestObservedTimes {
    /// The check runs.
    pub checks: Option<DateTime<Utc>>,
    /// The review decision.
    pub review: Option<DateTime<Utc>>,
    /// Mergeability and merge state.
    pub mergeability: Option<DateTime<Utc>>,
    /// Auto-merge arming.
    pub auto_merge: Option<DateTime<Utc>>,
    /// Merge-queue membership.
    pub queue: Option<DateTime<Utc>>,
}

/// The validators the conditional fetcher sends back to the host. Each one
/// names the answer the stored fields came from, so a 304 to it confirms
/// exactly those fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestEtags {
    /// The pull request object.
    pub pull: Option<String>,
    /// The head's check runs.
    pub checks: Option<String>,
    /// The review list.
    pub reviews: Option<String>,
}

/// One stored pull request: the fact, plus what the merge orders by.
///
/// The snapshot's own key is `(fact.updated_at, fact.last_seen_at)`: GitHub's
/// version, then when a read last saw that version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPullRequest {
    /// The fact every reader projects.
    pub fact: CodePullRequestFact,
    /// When each live field group was observed.
    pub observed: PullRequestObservedTimes,
    /// The fetcher's validators.
    pub etags: PullRequestEtags,
}

impl StoredPullRequest {
    /// The row a first read creates, with `id`. `None` when the read did not
    /// load the pull request object: checks or reviews alone never mint a
    /// row.
    #[must_use]
    pub fn first_sighting(read: &PullRequestRead, id: CodePullRequestId) -> Option<Self> {
        let object = read.object.as_ref()?;
        let snapshot = &object.snapshot;
        let blank = Self {
            fact: CodePullRequestFact {
                id,
                owner: read.owner.clone(),
                host: read.host.clone(),
                repo_owner: read.repo_owner.clone(),
                repo_name: read.repo_name.clone(),
                number: read.number,
                url: snapshot.url.clone(),
                title: snapshot.title.clone(),
                state: snapshot.state,
                draft: snapshot.draft,
                author: snapshot.author.clone(),
                head_branch: snapshot.head_branch.clone(),
                base_branch: snapshot.base_branch.clone(),
                head_sha: snapshot.head_sha.clone(),
                created_at: snapshot.created_at,
                updated_at: snapshot.updated_at,
                merged_at: snapshot.merged_at,
                closed_at: snapshot.closed_at,
                first_seen_at: object.observed_at,
                last_seen_at: object.observed_at,
                live: None,
            },
            observed: PullRequestObservedTimes::default(),
            etags: PullRequestEtags::default(),
        };
        Some(merge_pull_request_read(&blank, read))
    }

    /// The stored pull request object, restated as observed at
    /// `observed_at`.
    ///
    /// The fetcher sends [`PullRequestEtags::pull`] and uses this when the
    /// host answers 304: the host confirms the object that validator names,
    /// which is the stored one, so the confirmation lands as an observation
    /// like any other.
    #[must_use]
    pub fn restate_object(&self, observed_at: DateTime<Utc>) -> PullRequestObjectRead {
        let live = self.fact.live.as_ref();
        PullRequestObjectRead {
            snapshot: PullRequestSnapshot::of_fact(&self.fact),
            mergeability: Some(PullRequestMergeability {
                mergeable: live.and_then(|live| live.mergeable.clone()),
                merge_state_status: live.and_then(|live| live.merge_state_status.clone()),
            }),
            auto_merge_enabled: live.and_then(|live| live.auto_merge_enabled),
            observed_at,
            etag: self.etags.pull.clone(),
        }
    }

    /// The stored check runs, restated for a 304 to
    /// [`PullRequestEtags::checks`]. `None` when the row holds no checks, so
    /// a 304 cannot confirm anything.
    #[must_use]
    pub fn restate_checks(&self, observed_at: DateTime<Utc>) -> Option<PullRequestChecksRead> {
        let checks = self.fact.live.as_ref()?.checks.clone()?;
        Some(PullRequestChecksRead {
            head_sha: self.fact.head_sha.clone(),
            checks,
            observed_at,
            etag: self.etags.checks.clone(),
        })
    }

    /// The stored review decision, restated for a 304 to
    /// [`PullRequestEtags::reviews`].
    #[must_use]
    pub fn restate_review(&self, observed_at: DateTime<Utc>) -> PullRequestReviewRead {
        PullRequestReviewRead {
            decision: self
                .fact
                .live
                .as_ref()
                .and_then(|live| live.review_decision.clone()),
            observed_at,
            etag: self.etags.reviews.clone(),
        }
    }

    /// Whether any field a reader sees differs from `other`: the snapshot or
    /// a live value. Observation times, validators, and the seen stamps do
    /// not count, so a read that only confirmed the row reports no change.
    #[must_use]
    pub fn visibly_differs(&self, other: &Self) -> bool {
        fn live_values(
            live: Option<&CodePullRequestLiveState>,
        ) -> Option<CodePullRequestLiveState> {
            live.map(|live| CodePullRequestLiveState {
                observed_at: DateTime::<Utc>::MIN_UTC,
                ..live.clone()
            })
        }
        PullRequestSnapshot::of_fact(&self.fact) != PullRequestSnapshot::of_fact(&other.fact)
            || live_values(self.fact.live.as_ref()) != live_values(other.fact.live.as_ref())
    }
}

/// Merge one read into the stored state and return the new state.
///
/// Fields the read did not observe stay as stored. A read older than the
/// stored state changes nothing. A newer object that moves the head clears
/// the check runs, mergeability, and queue membership the old head carried.
/// `first_seen_at` never moves.
#[must_use]
pub fn merge_pull_request_read(
    stored: &StoredPullRequest,
    read: &PullRequestRead,
) -> StoredPullRequest {
    let mut merged = stored.clone();
    let stored_key = (stored.fact.updated_at, stored.fact.last_seen_at);

    if let Some(object) = &read.object {
        if (object.snapshot.updated_at, object.observed_at) > stored_key {
            object.snapshot.write_to(&mut merged.fact);
            merged.fact.last_seen_at = object.observed_at;
        }
    }

    // What described the old head no longer describes the pull request.
    if merged.fact.head_sha != stored.fact.head_sha {
        forget_head(&mut merged);
    }

    // Any live observation makes the tier exist and dates it, whether or not
    // its value lands: the stamp says when a read last looked.
    if let Some(latest) = latest_live_observation(read) {
        let live = merged
            .fact
            .live
            .get_or_insert_with(|| empty_live_tier(latest));
        live.observed_at = live.observed_at.max(latest);
    }

    let head = merged.fact.head_sha.clone();
    // A head-bound observation lands only when it describes the head of the
    // object the same read loaded, and that head is the stored one.
    let describes_head = |observed_head: &Option<String>| {
        observed_head == &head
            && read
                .object
                .as_ref()
                .is_some_and(|object| &object.snapshot.head_sha == observed_head)
    };

    if let Some(object) = &read.object {
        if let Some(mergeability) = &object.mergeability {
            if describes_head(&object.snapshot.head_sha)
                && is_later(object.observed_at, merged.observed.mergeability)
            {
                let live = live_mut(&mut merged.fact, object.observed_at);
                live.mergeable.clone_from(&mergeability.mergeable);
                live.merge_state_status
                    .clone_from(&mergeability.merge_state_status);
                merged.observed.mergeability = Some(object.observed_at);
            }
        }
        if let Some(enabled) = object.auto_merge_enabled {
            if is_later(object.observed_at, merged.observed.auto_merge) {
                live_mut(&mut merged.fact, object.observed_at).auto_merge_enabled = Some(enabled);
                merged.observed.auto_merge = Some(object.observed_at);
            }
        }
    }

    if let Some(checks) = &read.checks {
        if describes_head(&checks.head_sha) && is_later(checks.observed_at, merged.observed.checks)
        {
            let live = live_mut(&mut merged.fact, checks.observed_at);
            let unchanged = live.checks.as_ref() == Some(&checks.checks);
            live.checks = Some(checks.checks.clone());
            live.checks_summary =
                Some(PullRequestCheckCounts::from_checks(&checks.checks).summary_line());
            merged.observed.checks = Some(checks.observed_at);
            merged.etags.checks =
                validator_after(checks.etag.as_ref(), unchanged, &merged.etags.checks);
        }
    }

    if let Some(review) = &read.review {
        if is_later(review.observed_at, merged.observed.review) {
            let live = live_mut(&mut merged.fact, review.observed_at);
            let unchanged = live.review_decision == review.decision;
            live.review_decision.clone_from(&review.decision);
            merged.observed.review = Some(review.observed_at);
            merged.etags.reviews =
                validator_after(review.etag.as_ref(), unchanged, &merged.etags.reviews);
        }
    }

    if let Some(queue) = &read.queue {
        if describes_head(&queue.head_sha) && is_later(queue.observed_at, merged.observed.queue) {
            live_mut(&mut merged.fact, queue.observed_at).in_merge_queue =
                Some(queue.in_merge_queue);
            merged.observed.queue = Some(queue.observed_at);
        }
    }

    // The pull validator names one answer for the whole object. It moves to
    // the read's validator when that answer is complete and is now exactly
    // what is stored; it survives a read that changed nothing it covers; any
    // other change leaves no validator, so the next fetch is unconditional.
    let object_now_stored = read.object.as_ref().is_some_and(|object| {
        (object.snapshot.updated_at, object.observed_at) >= stored_key
            && PullRequestSnapshot::of_fact(&merged.fact) == object.snapshot
            && object.mergeability.as_ref() == Some(&mergeability_of(&merged.fact))
            && object.auto_merge_enabled.is_some()
            && object.auto_merge_enabled == auto_merge_of(&merged.fact)
    });
    let read_validator = read.object.as_ref().and_then(|object| object.etag.clone());
    merged.etags.pull = match read_validator {
        Some(etag) if object_now_stored => Some(etag),
        _ if pull_fields(&merged.fact) == pull_fields(&stored.fact) => stored.etags.pull.clone(),
        _ => None,
    };

    merged
}

/// Whether an observation at `at` is newer than one stored at `stored`.
/// Equal times are the same observation, so a replayed read changes nothing.
fn is_later(at: DateTime<Utc>, stored: Option<DateTime<Utc>>) -> bool {
    stored.is_none_or(|stored| at > stored)
}

/// The validator a group keeps after it takes a new observation: the
/// observation's own, or the stored one while the value did not change.
fn validator_after(
    observed: Option<&String>,
    unchanged: bool,
    stored: &Option<String>,
) -> Option<String> {
    match observed {
        Some(etag) => Some(etag.clone()),
        None if unchanged => stored.clone(),
        None => None,
    }
}

/// Clear what described the old head: check runs, mergeability, and queue
/// membership, with their times and the checks validator.
fn forget_head(merged: &mut StoredPullRequest) {
    if let Some(live) = merged.fact.live.as_mut() {
        live.checks = None;
        live.checks_summary = None;
        live.mergeable = None;
        live.merge_state_status = None;
        live.in_merge_queue = None;
    }
    merged.observed.checks = None;
    merged.observed.mergeability = None;
    merged.observed.queue = None;
    merged.etags.checks = None;
}

/// The latest time this read looked at any live field.
fn latest_live_observation(read: &PullRequestRead) -> Option<DateTime<Utc>> {
    let object = read
        .object
        .as_ref()
        .filter(|object| object.mergeability.is_some() || object.auto_merge_enabled.is_some());
    [
        object.map(|object| object.observed_at),
        read.checks.as_ref().map(|checks| checks.observed_at),
        read.review.as_ref().map(|review| review.observed_at),
        read.queue.as_ref().map(|queue| queue.observed_at),
    ]
    .into_iter()
    .flatten()
    .max()
}

fn empty_live_tier(observed_at: DateTime<Utc>) -> CodePullRequestLiveState {
    CodePullRequestLiveState {
        checks_summary: None,
        checks: None,
        review_decision: None,
        mergeable: None,
        merge_state_status: None,
        auto_merge_enabled: None,
        in_merge_queue: None,
        observed_at,
    }
}

/// The live tier, created for an observation at `at` when the row has none.
fn live_mut(fact: &mut CodePullRequestFact, at: DateTime<Utc>) -> &mut CodePullRequestLiveState {
    fact.live.get_or_insert_with(|| empty_live_tier(at))
}

fn mergeability_of(fact: &CodePullRequestFact) -> PullRequestMergeability {
    let live = fact.live.as_ref();
    PullRequestMergeability {
        mergeable: live.and_then(|live| live.mergeable.clone()),
        merge_state_status: live.and_then(|live| live.merge_state_status.clone()),
    }
}

fn auto_merge_of(fact: &CodePullRequestFact) -> Option<bool> {
    fact.live.as_ref().and_then(|live| live.auto_merge_enabled)
}

/// Every field the pull validator's answer determines.
fn pull_fields(
    fact: &CodePullRequestFact,
) -> (PullRequestSnapshot, PullRequestMergeability, Option<bool>) {
    (
        PullRequestSnapshot::of_fact(fact),
        mergeability_of(fact),
        auto_merge_of(fact),
    )
}

impl CodePullRequestFact {
    /// Project the fact into the digest vocabulary every consumer reads. The
    /// snapshot fills the identity; the live tier (decision 66) fills checks,
    /// review, and mergeability when a read has written it.
    #[must_use]
    pub fn digest(&self) -> PullRequestDigest {
        let live = self.live.as_ref();
        PullRequestDigest {
            number: self.number,
            url: Some(self.url.clone()),
            state: self.state.as_str().to_owned(),
            title: Some(self.title.clone()),
            checks_summary: live.and_then(|live| live.checks_summary.clone()),
            // The live tier stores the summary and the check list, not the
            // counts; rows written with a check list re-derive them, and rows
            // old enough to carry only the summary stay uncounted.
            check_counts: live
                .and_then(|live| live.checks.as_deref())
                .map(PullRequestCheckCounts::from_checks),
            checks: live.and_then(|live| live.checks.clone()),
            draft: Some(self.draft),
            merged: Some(self.state == CodePullRequestState::Merged),
            review_decision: live.and_then(|live| live.review_decision.clone()),
            mergeable: live.and_then(|live| live.mergeable.clone()),
            merge_state_status: live.and_then(|live| live.merge_state_status.clone()),
            head_branch: Some(self.head_branch.clone()),
            base_branch: Some(self.base_branch.clone()),
            head_sha: self.head_sha.clone(),
            auto_merge_enabled: live.and_then(|live| live.auto_merge_enabled),
            in_merge_queue: live.and_then(|live| live.in_merge_queue),
        }
    }
}

#[cfg(test)]
mod tests;
