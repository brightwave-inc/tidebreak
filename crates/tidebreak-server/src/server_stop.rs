//! Stop an embedded server for good: its accept loop and every background
//! worker, before an embedder deletes the data they work on.
//!
//! A quit does not need this, because exiting the process ends everything at
//! once. Delete all data does: it removes the database and the folders around
//! it while the process lives on, so nothing may still be writing into them.

use std::sync::Arc;

use tokio::sync::watch;

/// A handle that stops a running [`crate::Server`] and waits until its
/// workers have stopped. Cloneable; every clone stops the same server.
#[derive(Clone)]
pub struct ServerStop {
    requested: Arc<watch::Sender<bool>>,
    stopped: Arc<watch::Sender<bool>>,
}

impl Default for ServerStop {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerStop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            requested: Arc::new(watch::channel(false).0),
            stopped: Arc::new(watch::channel(false).0),
        }
    }

    /// Ask the server to stop, and wait until its accept loop has closed and
    /// every worker has stopped. A server that has not started serving stops
    /// as soon as it does.
    pub async fn stop(&self) {
        self.requested.send_replace(true);
        let mut stopped = self.stopped.subscribe();
        // The sender lives in `self`, so the channel cannot close under us.
        let _ = stopped.wait_for(|stopped| *stopped).await;
    }

    /// Resolves once a stop has been asked for.
    pub(crate) async fn requested(&self) {
        let mut requested = self.requested.subscribe();
        let _ = requested.wait_for(|requested| *requested).await;
    }

    /// Tell every [`Self::stop`] caller that the server has stopped.
    pub(crate) fn mark_stopped(&self) {
        self.stopped.send_replace(true);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn stop_returns_only_once_the_server_says_it_stopped() {
        let stop = ServerStop::new();
        let server = stop.clone();
        let serving = tokio::spawn(async move {
            server.requested().await;
            // Workers wind down here.
            tokio::time::sleep(Duration::from_millis(20)).await;
            server.mark_stopped();
        });
        tokio::time::timeout(Duration::from_secs(5), stop.stop())
            .await
            .expect("the stop completes once the server marks itself stopped");
        serving.await.unwrap();
    }

    #[tokio::test]
    async fn a_stop_asked_before_serving_is_seen_when_serving_starts() {
        let stop = ServerStop::new();
        stop.requested.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), stop.requested())
            .await
            .expect("an earlier request is not lost");
    }
}
