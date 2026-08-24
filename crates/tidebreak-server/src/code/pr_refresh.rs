//! Hot-tier pull-request refresher (decision 66).
//!
//! The pull requests someone is looking at — a workspace whose digest the
//! UI just requested, or one with an active watch — refresh every
//! [`HOT_REFRESH_INTERVAL`] through the conditional fetcher, where an
//! unchanged answer is a free 304. Everything else rides the reconcile
//! sweep's slower list read. No view owns a fetch, and no view triggers an
//! unconditional one: the request path reads the stored row, and this
//! sweep is what keeps the row current while anyone watches.

use std::sync::{Arc, Weak};
use std::time::Duration;

use tidebreak_core::db::code::list_active_watches_all_owners;

use super::runtime::CodeRuntime;

/// Coprime with the 47s watch, 53s trigger, and 61s reconcile sweeps, so
/// the four background readers never land on the same tick.
pub(crate) const HOT_REFRESH_INTERVAL: Duration = Duration::from_secs(17);

/// Abort the hot refresher when the runtime is dropped. The loop holds a
/// [`Weak`] handle for the same reason every sweep does: an `Arc` would
/// keep the runtime alive from its own field.
pub(crate) struct PrRefreshGuard(Option<tokio::task::JoinHandle<()>>);

impl PrRefreshGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(HOT_REFRESH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_hot(&runtime).await;
            }
        });
        Self(Some(handle))
    }
}

impl Drop for PrRefreshGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// One hot pass: every workspace with recent digest attention or an active
/// watch takes one conditional refresh. Sequential on purpose — the gate
/// spaces the host anyway, and the hot set is small by construction.
async fn sweep_hot(runtime: &Arc<CodeRuntime>) {
    let mut targets = runtime.hot_pull_request_workspaces();
    if let Ok(watches) = list_active_watches_all_owners(&runtime.db).await {
        for watch in watches {
            if !targets.iter().any(|(_, id)| *id == watch.workspace_id) {
                targets.push((watch.owner.clone(), watch.workspace_id));
            }
        }
    }
    for (owner, id) in targets {
        runtime.refresh_workspace_pr_row(&owner, id).await;
    }
}
