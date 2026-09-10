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
