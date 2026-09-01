//! Harness adapters, probe caching, and the managed Node runtime.

use super::*;

pub(super) const MANAGED_NODE_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) const MANAGED_NODE_STARTUP_GRACE: Duration = Duration::from_secs(2);

pub(super) const MANAGED_NODE_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) async fn wait_for_managed_node(
    broker: &dyn tidebreak_code_execution::HostToolBroker,
    retry: bool,
    wait_timeout: Duration,
    startup_grace: Duration,
) -> Result<PathBuf, String> {
    use tidebreak_code_execution::{HostDep, HostToolStatus};

    if retry {
        broker.retry(HostDep::Node);
    } else {
        broker.ensure(HostDep::Node);
    }
    let started = Instant::now();
    loop {
        match broker.status(HostDep::Node).await {
            HostToolStatus::Available => {
                return broker.managed_root(HostDep::Node).await.ok_or_else(|| {
                    "managed Node became available without a verified runtime root".to_owned()
                });
            }
            HostToolStatus::Installing => {}
            HostToolStatus::Unavailable(_) if retry && started.elapsed() < startup_grace => {}
            HostToolStatus::Unavailable(reason)
                if started.elapsed() < startup_grace
                    && reason.to_ascii_lowercase().contains("not installed yet") => {}
            HostToolStatus::Unavailable(reason) => return Err(reason),
        }
        if started.elapsed() >= wait_timeout {
            return Err("timed out waiting for the managed Node runtime".to_owned());
        }
        tokio::time::sleep(MANAGED_NODE_POLL_INTERVAL).await;
    }
}

/// Whether a memoized probe still describes the pinned binary on disk.
///
/// A probe observes exactly one file, and the pin version is a segment of
/// that file's path (`{data_dir}/tools/harnesses/{kind}/{version}/…`), so one
/// path comparison covers both halves of what the probe depends on: the
/// resolved binary and the pin it came from. A probe that found nothing
/// describes no install and is always stale.
pub(super) fn probe_describes(cached: Option<&HarnessProbe>, installed: &Path) -> bool {
    cached.is_some_and(|probe| probe.found && probe.binary_path.as_deref() == Some(installed))
}

impl CodeRuntime {
    /// The probe for `adapter`, resolved once and memoized.
    ///
    /// Decision 0034 makes discovery a cached read rather than a per-request
    /// one: a cold probe asks the user's shell in interactive login mode and
    /// then runs a version and an authentication observation, which is seconds
    /// of subprocess per harness. Re-probing is on demand — the doctor's
    /// refresh calls [`Self::invalidate_probes`] — so a harness installed
    /// while the app is running is picked up by the button that exists to say
    /// so, not by paying for it on every code-mode navigation.
    ///
    /// The cache fills lazily. Nothing warms it at boot: recovery already
    /// probes the kinds that have live sessions on its way to re-attaching
    /// them, and warming the rest would spend four login shells on harnesses
    /// this launch may never touch.
    pub(crate) async fn probe(&self, adapter: &dyn HarnessAdapter) -> HarnessProbe {
        let kind = adapter.kind();
        let cached = self
            .probes
            .lock()
            .expect("harness probes")
            .get(&kind)
            .cloned();
        if let Some(probe) = cached {
            return probe;
        }
        self.probe_uncached(adapter).await
    }

    /// The probe for session create: a cached signed-out observation that
    /// would refuse is stale.
    ///
    /// Signing in or adding a provider override does not invalidate the
    /// doctor's cache. Re-using that answer here would refuse a repaired
    /// machine — the false refusal this path exists to avoid. A signed-in
    /// or unverified cache, or a signed-out cache the relay or an override
    /// already carries, still hits.
    pub(super) async fn probe_for_session_create(
        &self,
        adapter: &dyn HarnessAdapter,
    ) -> HarnessProbe {
        let kind = adapter.kind();
        let cached = self
            .probes
            .lock()
            .expect("harness probes")
            .get(&kind)
            .cloned();
        if let Some(probe) = cached {
            let would_refuse =
                Self::signed_out_harness_refusal(self.harness_llm.is_some(), kind, &probe).is_err();
            if !would_refuse {
                return probe;
            }
            self.probes.lock().expect("harness probes").remove(&kind);
        }
        self.probe(adapter).await
    }

    pub(super) async fn probe_uncached(&self, adapter: &dyn HarnessAdapter) -> HarnessProbe {
        let kind = adapter.kind();
        let mut host = self.host.clone();
        host.managed_node_root = match self.host_tool_broker.as_deref() {
            Some(broker) => {
                broker
                    .managed_root(tidebreak_code_execution::HostDep::Node)
                    .await
            }
            None => tidebreak_managed_node::managed_node_root(&self.data_dir),
        };
        let probe = adapter.probe(&host).await;
        self.probes
            .lock()
            .expect("harness probes")
            .insert(kind, probe.clone());
        probe
    }

    /// Drop every memoized probe so the next read is cold. The doctor's
    /// refresh is the on-demand re-probe decision 0034 describes.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn record_pin_install(&self, kind: HarnessKind, result: Result<(), String>) {
        let mut errors = self.pin_install_errors.lock().expect("pin install errors");
        match result {
            Ok(()) => {
                errors.remove(&kind);
            }
            Err(err) => {
                errors.insert(kind, err);
            }
        }
    }

    pub(crate) fn pin_install_error(&self, kind: HarnessKind) -> Option<String> {
        self.pin_install_errors
            .lock()
            .expect("pin install errors")
            .get(&kind)
            .cloned()
    }

    pub(crate) fn invalidate_probes(&self) {
        self.probes.lock().expect("harness probes").clear();
    }

    /// Drop the memoized probe for `kind` only when the install it was taken
    /// against is not the one now on disk.
    ///
    /// Session create used to invalidate unconditionally, which charged every
    /// create a cold probe — a login shell plus a Node CLI start — to observe
    /// a binary that had not moved since the last one.
    pub(in crate::code) fn invalidate_moved_probe(&self, kind: HarnessKind, installed: &Path) {
        let mut probes = self.probes.lock().expect("harness probes");
        if !probe_describes(probes.get(&kind), installed) {
            probes.remove(&kind);
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(super) async fn managed_node_root(&self, retry: bool) -> Result<PathBuf, String> {
        match self.host_tool_broker.as_deref() {
            Some(broker) => {
                wait_for_managed_node(
                    broker,
                    retry,
                    MANAGED_NODE_WAIT_TIMEOUT,
                    MANAGED_NODE_STARTUP_GRACE,
                )
                .await
            }
            None => tidebreak_managed_node::managed_node_root(&self.data_dir)
                .ok_or_else(|| {
                    "the verified managed Node runtime is not installed in this Tidebreak data directory"
                        .to_owned()
                }),
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(in crate::code) async fn ensure_pinned_harness(
        &self,
        kind: HarnessKind,
        retry_node: bool,
    ) -> Result<PathBuf, String> {
        let node_root = self.managed_node_root(retry_node).await?;
        tidebreak_harness::ensure_installed(&self.data_dir, kind, Some(&node_root)).await
    }

    pub(crate) fn adapter(
        &self,
        kind: HarnessKind,
    ) -> Result<Arc<dyn HarnessAdapter>, ServerError> {
        self.adapters.get(kind).ok_or_else(|| {
            ServerError::bad_request_kind(
                "harness_unavailable",
                format!("no adapter is registered for {kind}"),
            )
        })
    }
}

#[cfg(test)]
mod probe_freshness_tests {
    use super::*;

    fn probe_at(path: Option<&str>) -> HarnessProbe {
        HarnessProbe {
            found: path.is_some(),
            binary_path: path.map(PathBuf::from),
            version: Some("2.1.234".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn a_probe_of_the_installed_binary_is_still_current() {
        let installed =
            Path::new("/data/tools/harnesses/claude_code/2.1.234/node_modules/.bin/claude");
        assert!(probe_describes(
            Some(&probe_at(installed.to_str())),
            installed
        ));
    }

    #[test]
    fn a_pin_bump_moves_the_path_and_stales_the_probe() {
        let previous = probe_at(Some(
            "/data/tools/harnesses/claude_code/2.1.233/node_modules/.bin/claude",
        ));
        assert!(!probe_describes(
            Some(&previous),
            Path::new("/data/tools/harnesses/claude_code/2.1.234/node_modules/.bin/claude")
        ));
    }

    #[test]
    fn no_probe_and_a_probe_that_found_nothing_are_both_stale() {
        let installed = Path::new("/data/tools/harnesses/codex/0.147.0/node_modules/.bin/codex");
        assert!(!probe_describes(None, installed));
        assert!(!probe_describes(Some(&probe_at(None)), installed));
    }
}

#[cfg(test)]
mod managed_node_wait_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tidebreak_code_execution::{HostDep, HostToolBroker, HostToolStatus};

    struct RecordingBroker {
        ensure_calls: AtomicUsize,
        retry_calls: AtomicUsize,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl HostToolBroker for RecordingBroker {
        fn ensure(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.ensure_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn retry(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.retry_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn status(&self, tool: HostDep) -> HostToolStatus {
            assert_eq!(tool, HostDep::Node);
            HostToolStatus::Available
        }

        async fn managed_root(&self, tool: HostDep) -> Option<PathBuf> {
            assert_eq!(tool, HostDep::Node);
            Some(self.root.clone())
        }
    }

    /// Reproduces the Doctor Refresh race: `retry` has been called, but the
    /// first `status` still reports the remembered failure from the previous
    /// attempt. That Unavailable must not fail the wait while startup grace
    /// is open.
    struct StaleFailureThenReadyBroker {
        retry_calls: AtomicUsize,
        polls: AtomicUsize,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl HostToolBroker for StaleFailureThenReadyBroker {
        fn ensure(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
        }

        fn retry(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.retry_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn status(&self, tool: HostDep) -> HostToolStatus {
            assert_eq!(tool, HostDep::Node);
            match self.polls.fetch_add(1, Ordering::Relaxed) {
                0 => HostToolStatus::Unavailable(
                    "the previous Node install failed: disk full".into(),
                ),
                1 => HostToolStatus::Installing,
                _ => HostToolStatus::Available,
            }
        }

        async fn managed_root(&self, tool: HostDep) -> Option<PathBuf> {
            assert_eq!(tool, HostDep::Node);
            Some(self.root.clone())
        }
    }

    #[tokio::test]
    async fn explicit_harness_refresh_retries_node_provisioning() {
        let broker = RecordingBroker {
            ensure_calls: AtomicUsize::new(0),
            retry_calls: AtomicUsize::new(0),
            root: PathBuf::from("/verified/node"),
        };
        let root = wait_for_managed_node(&broker, true, Duration::from_secs(1), Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(root, PathBuf::from("/verified/node"));
        assert_eq!(broker.ensure_calls.load(Ordering::Relaxed), 0);
        assert_eq!(broker.retry_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn retry_does_not_fail_on_a_stale_unavailable_during_startup_grace() {
        let broker = StaleFailureThenReadyBroker {
            retry_calls: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            root: PathBuf::from("/verified/node"),
        };
        let root = wait_for_managed_node(
            &broker,
            true,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .await
        .expect("stale failure during grace is in-progress, not terminal");

        assert_eq!(root, PathBuf::from("/verified/node"));
        assert_eq!(broker.retry_calls.load(Ordering::Relaxed), 1);
        assert!(broker.polls.load(Ordering::Relaxed) >= 3);
    }

    #[cfg(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        )
    ))]
    #[tokio::test]
    async fn headless_runtime_reuses_a_verified_node_for_an_existing_pinned_harness() {
        use std::os::unix::fs::PermissionsExt as _;
        use tidebreak_managed_node::{
            current_managed_node_pin, managed_node_install_marker, managed_node_marker_path,
            managed_node_version_dir,
        };

        let data_dir = tempfile::tempdir().expect("data dir");
        let node_pin = current_managed_node_pin().expect("supported test platform");
        let node_root = managed_node_version_dir(data_dir.path());
        let node_bin = node_root.join("bin");
        std::fs::create_dir_all(&node_bin).expect("node bin");
        for name in ["node", "npm"] {
            let path = node_bin.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").expect("node entrypoint");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("node entrypoint mode");
        }
        std::fs::write(
            managed_node_marker_path(&node_root),
            managed_node_install_marker(node_pin).expect("node marker"),
        )
        .expect("write node marker");

        let harness = HarnessKind::ClaudeCode;
        let harness_pin = tidebreak_harness::pin_for(harness).expect("harness pin");
        let harness_dir = tidebreak_harness::pin::install_dir(data_dir.path(), harness_pin);
        let harness_binary = harness_dir
            .join("node_modules")
            .join(".bin")
            .join(harness_pin.bin);
        std::fs::create_dir_all(harness_binary.parent().expect("harness parent"))
            .expect("harness bin");
        std::fs::write(&harness_binary, b"#!/usr/bin/env node\n").expect("harness binary");
        std::fs::set_permissions(&harness_binary, std::fs::Permissions::from_mode(0o755))
            .expect("harness mode");
        std::fs::write(
            harness_dir.join("installed.json"),
            serde_json::to_vec(&serde_json::json!({
                "package": harness_pin.package,
                "version": harness_pin.version,
            }))
            .expect("harness marker"),
        )
        .expect("write harness marker");

        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            data_dir.path().join("headless-code.db").display()
        ))
        .await
        .expect("db");
        let runtime = CodeRuntime::new(
            Arc::new(db),
            data_dir.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        assert_eq!(
            runtime.managed_node_root(false).await.expect("node root"),
            node_root
        );
        assert_eq!(
            runtime
                .ensure_pinned_harness(harness, false)
                .await
                .expect("existing harness"),
            harness_binary
        );
    }
}
