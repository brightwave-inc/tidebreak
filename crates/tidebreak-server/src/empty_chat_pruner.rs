//! Periodic removal of conversations nothing ever happened in.
//!
//! A client can create a conversation before its first message, and a start
//! the reader abandons leaves an empty one behind. The list of work already
//! hides a conversation with no turns; this sweep deletes the ones that are
//! safe to lose. `Store::prune_empty_chats` holds the rule: no turns, no
//! title, no pin, not archived, and nothing attached.
//!
//! The day of grace covers a client that creates a conversation and sends into
//! it a moment later, or a script that creates one now and uses it in the next
//! command. A home draft that holds a conversation for its attachments is
//! never pruned, because the attachments count as content.

use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::Store;

/// How old an empty conversation must be before the sweep removes it.
const EMPTY_CHAT_GRACE: chrono::TimeDelta = chrono::TimeDelta::days(1);

/// How often the sweep runs after its first pass at startup.
const SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Sweep once at startup, then on a slow interval for the life of the server.
pub(crate) async fn run(store: Arc<dyn Store>) {
    loop {
        prune_once(store.as_ref()).await;
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

async fn prune_once(store: &dyn Store) {
    let cutoff = chrono::Utc::now() - EMPTY_CHAT_GRACE;
    match store.prune_empty_chats(cutoff).await {
        Ok(pruned) if !pruned.is_empty() => {
            tracing::info!("tidebreak: removed {} empty conversations", pruned.len());
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!("tidebreak: could not remove empty conversations: {error}");
        }
    }
}
