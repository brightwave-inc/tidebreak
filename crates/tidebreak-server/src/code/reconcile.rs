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
use std::time::{Duration, Instant};

use tracing::debug;

use tidebreak_core::db::code::{
    list_fact_repo_identities_all_owners, list_repos_all_owners, set_repo_origin,
};
use tidebreak_core::OwnerId;

use super::delivery::{query_pull_requests, repository_target_from_local, MAX_REPOSITORIES};
use super::runtime::CodeRuntime;
use crate::code::types::{CodeDeliveryPullRequestQuery, CodeGitHubRepositoryTarget};

/// Coprime with the 47s watch and 53s trigger sweeps, so three GitHub-reading
/// sweeps never land on the same tick. This is the cadence while a client
/// holds an `/updates` socket, meaning someone can see the result.
pub(crate) const RECONCILE_SWEEP_INTERVAL: Duration = Duration::from_secs(61);

/// The cadence while no client is attached.
///
/// Idle sweeps are mostly conditional 304s, but any repository that moved
/// pays real per-pull-request reads, and with nobody looking the answer only
/// has to be current by the time a window opens again. Subscribing to
/// `/updates` wakes the sweep, so reopening the app does not wait this
/// long. Prime, like the others, so the idle sweep does not fall into step
/// with the watch and trigger sweeps.
pub(crate) const RECONCILE_DETACHED_INTERVAL: Duration = Duration::from_secs(421);

/// How long the sweep waits before its next pass.
///
/// `since_last_sweep` is `None` before the first pass, which runs at once the
/// way the original fixed ticker did. Otherwise the wait is whatever remains
/// of the cadence for the current attachment state: a client attaching after
/// a long idle stretch gets an immediate pass, while a client that
/// reconnects seconds after the last pass waits out the fast interval, so a
/// flapping socket cannot make the sweep read GitHub faster than once per
/// [`RECONCILE_SWEEP_INTERVAL`].
pub(crate) fn reconcile_delay(attached: bool, since_last_sweep: Option<Duration>) -> Duration {
    let interval = if attached {
        RECONCILE_SWEEP_INTERVAL
    } else {
        RECONCILE_DETACHED_INTERVAL
    };
    match since_last_sweep {
        None => Duration::ZERO,
        Some(elapsed) => interval.saturating_sub(elapsed),
    }
}

/// A live tier younger than this answers a sweep without a host read
/// (decision 66): two reconcile intervals plus slack, so one missed pass
/// degrades to a fetch rather than a stale verdict.
const LIVE_TIER_FRESH_SECS: i64 = (RECONCILE_SWEEP_INTERVAL.as_secs() as i64) * 2 + 30;

/// Whether a live tier was observed recently enough to answer a sweep.
fn live_tier_is_fresh(
    live: &tidebreak_core::CodePullRequestLiveState,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    now - live.observed_at <= chrono::Duration::seconds(LIVE_TIER_FRESH_SECS)
}

/// Whether a sweep may answer from this pull request's stored row instead
/// of reading the host: the live tier is fresh, and an open pull request's
/// check runs are known.
///
/// A newer head clears the old head's check runs, and a read that saw only
/// the pull request object, such as a push confirmation, does not load the
/// new head's. Until a read does, the row's checks are unknown, not empty:
/// the watch would judge the new head on its review state alone, and a
/// trigger could fire before the head's checks exist. A settled pull request
/// needs no checks to classify.
pub(crate) fn live_tier_answers(
    fact: &tidebreak_core::CodePullRequestFact,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    fact.live.as_ref().is_some_and(|live| {
        live_tier_is_fresh(live, now)
            && (fact.state != tidebreak_core::CodePullRequestState::Open || live.checks.is_some())
    })
}

/// Abort the reconcile sweep when the runtime is dropped.
///
/// The loop holds a [`Weak`] runtime handle: an `Arc` would keep the runtime
/// alive from its own field and the guard's `Drop` could never run.
pub(crate) struct ReconcileSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl ReconcileSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let handle = tokio::spawn(async move {
            let mut last_sweep: Option<Instant> = None;
            loop {
                let Some(strong) = runtime.upgrade() else {
                    return;
                };
                let attached = strong.bus.has_updates_subscribers();
                let delay = reconcile_delay(attached, last_sweep.map(|at| at.elapsed()));
                let bus = Arc::clone(&strong.bus);
                // Drop the strong handle while waiting: this loop must not be
                // what keeps the runtime alive.
                drop(strong);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    // A client attached (or reconnected). Re-read the count
                    // and recompute the wait rather than sweeping blindly, so
                    // the fast cadence still bounds how often GitHub is read.
                    _ = bus.updates_attached() => continue,
                }
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_reconcile(&runtime).await;
                last_sweep = Some(Instant::now());
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
pub async fn sweep_reconcile(runtime: &Arc<CodeRuntime>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a stored pull request through the store's merge, the way the
    /// fetcher and a push confirmation would leave it.
    fn stored_after(
        reads: &[tidebreak_core::PullRequestRead],
    ) -> tidebreak_core::StoredPullRequest {
        let first = tidebreak_core::StoredPullRequest::first_sighting(
            &reads[0],
            tidebreak_core::CodePullRequestId::new(),
        )
        .unwrap();
        reads.iter().fold(first, |state, read| {
            tidebreak_core::merge_pull_request_read(&state, read)
        })
    }

    fn object_read(
        head: &str,
        version: chrono::DateTime<chrono::Utc>,
        observed_at: chrono::DateTime<chrono::Utc>,
        state: tidebreak_core::CodePullRequestState,
    ) -> tidebreak_core::PullRequestRead {
        let mut read = tidebreak_core::PullRequestRead::new(
            tidebreak_core::OwnerId::local(),
            "github.com",
            "acme",
            "tools",
            7,
        );
        read.object = Some(tidebreak_core::PullRequestObjectRead {
            snapshot: tidebreak_core::PullRequestSnapshot {
                url: "https://github.com/acme/tools/pull/7".into(),
                title: "Tools".into(),
                state,
                draft: false,
                author: None,
                head_branch: "feature".into(),
                base_branch: "main".into(),
                head_sha: Some(head.into()),
                created_at: version,
                updated_at: version,
                merged_at: None,
                closed_at: None,
            },
            mergeability: None,
            auto_merge_enabled: None,
            observed_at,
            etag: None,
        });
        read
    }

    /// The trigger and watch sweeps share this rule. After a push
    /// confirmation moves the head, the row's checks are unknown until a read
    /// loads them, and a row with unknown checks sends the sweep to the host
    /// rather than firing on "no checks".
    #[test]
    fn a_moved_head_answers_again_only_once_its_checks_load() {
        use tidebreak_core::{CodePullRequestState, PullRequestChecksRead, PullRequestReviewRead};

        let now = chrono::Utc::now();
        let at =
            |seconds: i64| now - chrono::Duration::seconds(60) + chrono::Duration::seconds(seconds);
        let mut fetched = object_read("aaa", at(0), at(1), CodePullRequestState::Open);
        fetched.checks = Some(PullRequestChecksRead {
            head_sha: Some("aaa".into()),
            checks: Vec::new(),
            observed_at: at(1),
            etag: None,
        });
        fetched.review = Some(PullRequestReviewRead {
            decision: Some("review_required".into()),
            observed_at: at(1),
            etag: None,
        });
        let before_push = stored_after(&[fetched.clone()]);
        assert!(live_tier_answers(&before_push.fact, now));

        // The push confirmation carries only the object, on the new head.
        let pushed = object_read("bbb", at(10), at(11), CodePullRequestState::Open);
        let after_push = stored_after(&[fetched.clone(), pushed.clone()]);
        let live = after_push.fact.live.as_ref().unwrap();
        assert!(super::live_tier_is_fresh(live, now), "the stamp is recent");
        assert_eq!(live.checks, None);
        assert!(
            !live_tier_answers(&after_push.fact, now),
            "unknown checks on an open pull request do not answer"
        );

        // A read of the new head's checks makes the row answer again.
        let mut checked = object_read("bbb", at(10), at(20), CodePullRequestState::Open);
        checked.checks = Some(PullRequestChecksRead {
            head_sha: Some("bbb".into()),
            checks: Vec::new(),
            observed_at: at(20),
            etag: None,
        });
        assert!(live_tier_answers(
            &stored_after(&[fetched.clone(), pushed.clone(), checked]).fact,
            now
        ));

        // A settled pull request needs no checks to classify.
        let merged = object_read("bbb", at(30), at(31), CodePullRequestState::Merged);
        assert!(live_tier_answers(
            &stored_after(&[fetched, pushed, merged]).fact,
            now
        ));
    }

    #[test]
    fn the_first_pass_runs_at_once_in_either_state() {
        assert_eq!(reconcile_delay(true, None), Duration::ZERO);
        assert_eq!(reconcile_delay(false, None), Duration::ZERO);
    }

    #[test]
    fn an_attached_client_keeps_the_fast_cadence() {
        assert_eq!(
            reconcile_delay(true, Some(Duration::from_secs(1))),
            RECONCILE_SWEEP_INTERVAL - Duration::from_secs(1)
        );
    }

    #[test]
    fn no_client_backs_the_sweep_off_to_minutes() {
        let delay = reconcile_delay(false, Some(Duration::from_secs(1)));
        assert_eq!(delay, RECONCILE_DETACHED_INTERVAL - Duration::from_secs(1));
        assert!(delay >= Duration::from_secs(5 * 60));
        assert!(delay <= Duration::from_secs(10 * 60));
    }

    #[test]
    fn attaching_after_an_idle_stretch_sweeps_immediately() {
        // Two fast intervals into an idle stretch a window opens: the wait
        // recomputed against the fast cadence has already elapsed.
        let idle = RECONCILE_SWEEP_INTERVAL * 2;
        assert_eq!(
            reconcile_delay(false, Some(idle)),
            RECONCILE_DETACHED_INTERVAL - idle
        );
        assert_eq!(reconcile_delay(true, Some(idle)), Duration::ZERO);
    }

    #[test]
    fn a_reconnect_right_after_a_pass_waits_out_the_fast_interval() {
        let just_swept = Duration::from_secs(5);
        assert_eq!(
            reconcile_delay(true, Some(just_swept)),
            RECONCILE_SWEEP_INTERVAL - just_swept
        );
    }
}
