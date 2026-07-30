//! Production scheduling for sandbox-resident container runs.
//!
//! The runner owns one exact claimed run. This worker supplies the surrounding
//! service seam: bounded polling for fresh queued work, concurrent drive lanes,
//! and an ordered recovery-then-sweep maintenance cadence.

use std::sync::Arc;
use std::time::Duration;

use openwave_core::{Result, Store};
use openwave_sandbox_protocol::SandboxBackend;
use tokio::sync::Notify;

use crate::resolver::ProviderResolver;
use crate::sandbox_container_run::{SandboxContainerRunConfig, SandboxContainerRunner};

/// Service-level polling and maintenance tunables.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxContainerRunWorkerConfig {
    pub(crate) idle_min: Duration,
    pub(crate) idle_cap: Duration,
    pub(crate) failure_delay: Duration,
    pub(crate) maintenance_interval: Duration,
    pub(crate) candidate_limit: u64,
    pub(crate) max_concurrency: usize,
}

impl Default for SandboxContainerRunWorkerConfig {
    fn default() -> Self {
        Self {
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            maintenance_interval: Duration::from_secs(30),
            candidate_limit: 16,
            max_concurrency: 4,
        }
    }
}

/// Polls for and drives container-located background runs.
#[derive(Clone)]
pub(crate) struct SandboxContainerRunWorker {
    store: Arc<dyn Store>,
    runner: Arc<SandboxContainerRunner>,
    wake: Arc<Notify>,
    config: SandboxContainerRunWorkerConfig,
}

impl SandboxContainerRunWorker {
    /// Build the service only when container execution is enabled.
    ///
    /// Production assembly passes `false` until the profile flag lands. The
    /// caller separately checks backend availability before spawning the
    /// returned worker, so absence of a container runtime stays an inert
    /// capability miss rather than a boot failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<dyn Store>,
        backend: Arc<dyn SandboxBackend>,
        resolver: Arc<dyn ProviderResolver>,
        wake: Arc<Notify>,
        enabled: bool,
        runner_config: SandboxContainerRunConfig,
        config: SandboxContainerRunWorkerConfig,
    ) -> Option<Self> {
        if !enabled {
            return None;
        }
        assert!(!config.idle_min.is_zero());
        assert!(config.idle_min <= config.idle_cap);
        assert!(!config.failure_delay.is_zero());
        assert!(!config.maintenance_interval.is_zero());
        assert!(config.candidate_limit > 0);
        assert!(config.max_concurrency > 0);
        let runner = Arc::new(SandboxContainerRunner::new(
            store.clone(),
            backend,
            resolver,
            runner_config,
        ));
        Some(Self {
            store,
            runner,
            wake,
            config,
        })
    }

    /// Run the owned drive lanes and maintenance lane until the server drops
    /// the worker task. Dropping this future drops the [`tokio::task::JoinSet`],
    /// which aborts every child lane rather than detaching background work.
    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_drive_lane());
        }
        lanes.spawn(self.run_maintenance_lane());
        while let Some(result) = lanes.join_next().await {
            match result {
                Ok(()) => eprintln!("openwave: container worker lane stopped unexpectedly"),
                Err(error) => eprintln!("openwave: container worker lane stopped: {error}"),
            }
        }
    }

    async fn run_drive_lane(self) {
        let mut idle_delay = self.config.idle_min;
        loop {
            match self.drive_one().await {
                Ok(true) => idle_delay = self.config.idle_min,
                Ok(false) => {
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Err(error) => {
                    eprintln!("openwave: container worker iteration failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    async fn drive_one(&self) -> Result<bool> {
        let candidates = self
            .store
            .list_container_agent_run_candidates(self.config.candidate_limit)
            .await?;
        for run_id in candidates {
            if self.runner.drive(run_id).await?.is_some() {
                // A completed drive frees a claim slot. Wake one sleeping lane
                // so queued work can consume it without waiting for backoff.
                self.wake.notify_one();
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn run_maintenance_lane(self) {
        loop {
            match self.runner.recover().await {
                Ok(_) => {
                    if let Err(error) = self.runner.sweep().await {
                        eprintln!("openwave: container worker sweep failed: {error}");
                    }
                }
                Err(error) => {
                    // Do not sweep after an incomplete recovery pass: a
                    // reclaimable run must be re-driven before its container
                    // can be considered an orphan.
                    eprintln!("openwave: container worker recovery failed: {error}");
                }
            }
            tokio::time::sleep(self.config.maintenance_interval).await;
        }
    }
}
