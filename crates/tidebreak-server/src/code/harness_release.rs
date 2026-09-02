//! Which release of each engine this machine drives: the pin, or the newest
//! the registry publishes.
//!
//! Decision 41 pins every engine to the exact version this build was captured
//! against. The pin is still the floor; the `latest` update channel lets a
//! reader move past it without waiting for a release. Every version lands in
//! its own directory with a marker that names it, so the selection here is a
//! read of what is on disk, never a guess from whatever is newest.
//!
//! The registry is consulted in exactly two places: the doctor's Check for
//! updates, and a deliberate install on the `latest` channel. Listing and
//! session create read the disk and the memoized probe, so neither waits on a
//! network round trip.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tidebreak_core::{HarnessKind, HarnessUpdateChannel, Store};

use super::runtime::CodeRuntime;

/// Store key for the update channel. One channel for every engine: the
/// question it answers is "may this machine run past the pins", not
/// "which pins".
pub(crate) const HARNESS_UPDATE_CHANNEL_SETTING: &str = "code.harness_update_channel";

/// The stored update channel, or `pinned` when unset or unreadable.
pub(crate) async fn read_update_channel(
    store: &dyn Store,
) -> tidebreak_core::Result<HarnessUpdateChannel> {
    Ok(store
        .get_setting(HARNESS_UPDATE_CHANNEL_SETTING)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// One installed engine binary and the exact version its marker names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledHarness {
    pub version: String,
    pub binary: PathBuf,
}

/// Where one engine stands against its pin and the registry, for the doctor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HarnessReleaseStatus {
    /// The version this build pins, when it ships one.
    pub pinned_version: Option<String>,
    /// The managed install the channel currently drives, when one is on disk.
    pub managed_version: Option<String>,
    /// The newest version the registry answered with, from the last lookup
    /// this process made.
    pub latest_version: Option<String>,
    /// Whether the channel is `latest`, the registry named a version, and the
    /// driven install is older than it.
    pub update_available: bool,
}

/// Last registry answers, one per engine. Not journaled: a restart forgets
/// them and the next Check for updates asks again.
#[derive(Debug, Default)]
pub(crate) struct KnownReleases {
    latest: std::sync::Mutex<HashMap<HarnessKind, String>>,
}

impl KnownReleases {
    fn get(&self, kind: HarnessKind) -> Option<String> {
        self.latest
            .lock()
            .expect("known harness releases")
            .get(&kind)
            .cloned()
    }

    fn record(&self, kind: HarnessKind, version: String) {
        self.latest
            .lock()
            .expect("known harness releases")
            .insert(kind, version);
    }
}

impl CodeRuntime {
    /// The update channel this machine is on.
    pub(crate) async fn harness_update_channel(&self) -> HarnessUpdateChannel {
        read_update_channel(&*self.db).await.unwrap_or_default()
    }

    /// The registry's last answer for `kind`, if this process asked.
    pub(crate) fn known_latest_version(&self, kind: HarnessKind) -> Option<String> {
        self.harness_releases.get(kind)
    }

    /// The managed install the channel drives for `kind`, if one is on disk.
    ///
    /// On `latest`, the newest installed version at or past the pin. On
    /// `pinned`, the pin — an installed newer version stays on disk but is
    /// not driven, so flipping the channel back is a probe away, not a
    /// download away.
    pub(crate) async fn selected_harness(&self, kind: HarnessKind) -> Option<InstalledHarness> {
        let pin = tidebreak_harness::pin_for(kind)?;
        let channel = self.harness_update_channel().await;
        let version = match channel {
            HarnessUpdateChannel::Pinned => pin.version.to_owned(),
            HarnessUpdateChannel::Latest => {
                tidebreak_harness::installed_versions(&self.data_dir, kind)
                    .into_iter()
                    .find(|version| {
                        tidebreak_harness::compare_versions(version, pin.version).is_ge()
                    })
                    .unwrap_or_else(|| pin.version.to_owned())
            }
        };
        let binary = tidebreak_harness::managed_binary_version(&self.data_dir, kind, &version)?;
        Some(InstalledHarness { version, binary })
    }

    /// The version [`Self::selected_harness`] would drive, when it is not the
    /// pin. This is what the probe's host environment carries.
    pub(super) async fn selected_harness_versions(&self) -> Vec<(HarnessKind, String)> {
        let mut selected = Vec::new();
        for kind in HarnessKind::ALL {
            let Some(pin) = tidebreak_harness::pin_for(*kind) else {
                continue;
            };
            if let Some(installed) = self.selected_harness(*kind).await {
                if installed.version != pin.version {
                    selected.push((*kind, installed.version));
                }
            }
        }
        selected
    }

    /// Whether a memoized probe still describes the install the channel
    /// drives for `kind`.
    ///
    /// A probe observes one file. When the channel moves — an update landed,
    /// or the reader went back to the pin — the file it should observe moves
    /// with it, and a probe of the old file would keep reporting a version
    /// nothing launches. A declared binary never moves, and a host with no
    /// data directory never resolves a managed install; both stay current.
    pub(super) async fn probe_is_current(
        &self,
        kind: HarnessKind,
        probe: &tidebreak_harness::HarnessProbe,
    ) -> bool {
        if kind.is_in_process()
            || self.host.data_dir.is_none()
            || self.host.declared(kind).is_some()
            || tidebreak_harness::pin_for(kind).is_none()
        {
            return true;
        }
        match self.selected_harness(kind).await {
            Some(installed) => probe.binary_path.as_deref() == Some(installed.binary.as_path()),
            // With a data directory set, the only binary a probe can find is
            // a managed one, so a found probe with nothing selected describes
            // an install the channel no longer drives.
            None => !probe.found,
        }
    }

    /// Ask the registry for `kind`'s newest release and remember the answer.
    /// A registry that cannot be reached is a warning here, not an error: the
    /// caller has an installed version or the pin to fall back on.
    async fn lookup_latest(&self, kind: HarnessKind, node_root: &Path) -> Option<String> {
        match tidebreak_harness::latest_published_version(kind, Some(node_root)).await {
            Ok(version) => {
                self.harness_releases.record(kind, version.clone());
                Some(version)
            }
            Err(err) => {
                tracing::warn!(%kind, error = %err, "registry lookup failed; using what is installed");
                None
            }
        }
    }

    /// Install — or find — the engine binary the channel drives for `kind`.
    ///
    /// On `pinned`, the pin. On `latest`, what is already installed unless
    /// this caller asked for a fresh lookup: the registry's last answer is
    /// what Update moves to, and a session create or a picker warm-up that
    /// installed it unasked would stall on a 37–297MB download and drive a
    /// binary the doctor row does not describe. Only a machine with nothing
    /// installed yet takes the newest release on its own. A registry that
    /// cannot be reached is not a fault on this path: the newest install
    /// stands in, and the pin stands in for that.
    pub(in crate::code) async fn ensure_harness(
        &self,
        kind: HarnessKind,
        retry_node: bool,
        refresh: bool,
    ) -> Result<InstalledHarness, String> {
        let node_root = self.managed_node_root(retry_node).await?;
        let pin = tidebreak_harness::pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
        let channel = self.harness_update_channel().await;
        let installed = self.selected_harness(kind).await;
        let target = match channel {
            HarnessUpdateChannel::Pinned => pin.version.to_owned(),
            HarnessUpdateChannel::Latest => {
                let past_pin = |version: &String| {
                    tidebreak_harness::compare_versions(version, pin.version).is_ge()
                };
                let installed_version = installed.as_ref().map(|found| found.version.clone());
                let resolved = if refresh {
                    self.lookup_latest(kind, &node_root)
                        .await
                        .filter(past_pin)
                        .or(installed_version)
                } else if let Some(version) = installed_version {
                    Some(version)
                } else {
                    match self.known_latest_version(kind).filter(past_pin) {
                        Some(known) => Some(known),
                        None => self.lookup_latest(kind, &node_root).await.filter(past_pin),
                    }
                };
                resolved.unwrap_or_else(|| pin.version.to_owned())
            }
        };
        let binary = tidebreak_harness::ensure_installed_version(
            &self.data_dir,
            kind,
            &target,
            Some(&node_root),
        )
        .await?;
        Ok(InstalledHarness {
            version: target,
            binary,
        })
    }

    /// Ask the registry for every engine's newest version and remember the
    /// answers. Returns the first failure when nothing answered, so a machine
    /// with no route to the registry hears that rather than "no updates".
    pub(crate) async fn check_harness_updates(&self) -> Result<(), String> {
        let node_root = self.managed_node_root(false).await?;
        let lookups = HarnessKind::ALL
            .iter()
            .copied()
            .filter(|kind| tidebreak_harness::pin_for(*kind).is_some())
            .map(|kind| {
                let node_root = node_root.clone();
                async move {
                    (
                        kind,
                        tidebreak_harness::latest_published_version(kind, Some(&node_root)).await,
                    )
                }
            });
        let mut first_error = None;
        let mut answered = false;
        for (kind, result) in futures::future::join_all(lookups).await {
            match result {
                Ok(version) => {
                    answered = true;
                    self.harness_releases.record(kind, version);
                }
                Err(err) => {
                    tracing::warn!(%kind, error = %err, "registry lookup failed");
                    first_error.get_or_insert(err);
                }
            }
        }
        match (answered, first_error) {
            (false, Some(err)) => Err(err),
            _ => Ok(()),
        }
    }

    /// Where `kind` stands against its pin and the registry.
    pub(crate) async fn harness_release_status(&self, kind: HarnessKind) -> HarnessReleaseStatus {
        let Some(pin) = tidebreak_harness::pin_for(kind) else {
            return HarnessReleaseStatus::default();
        };
        let channel = self.harness_update_channel().await;
        let managed_version = self
            .selected_harness(kind)
            .await
            .map(|installed| installed.version);
        let latest_version = self.known_latest_version(kind);
        let update_available = channel == HarnessUpdateChannel::Latest
            && matches!(
                (&latest_version, &managed_version),
                (Some(latest), Some(current))
                    if tidebreak_harness::compare_versions(latest, current).is_gt()
            );
        HarnessReleaseStatus {
            pinned_version: Some(pin.version.to_owned()),
            managed_version,
            latest_version,
            update_available,
        }
    }
}

#[cfg(test)]
pub(in crate::code) mod test_support {
    use std::path::{Path, PathBuf};

    use tidebreak_core::HarnessKind;

    /// Lay down a managed install of `version` for `kind` — an executable
    /// stub and the marker that names it — and return the binary path.
    pub(in crate::code) fn write_install(
        data_dir: &Path,
        kind: HarnessKind,
        version: &str,
    ) -> PathBuf {
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        let dir = tidebreak_harness::pin::install_dir_for(data_dir, pin, version);
        // npm lays down a `.cmd` shim on Windows, and that is the file the
        // lookup checks for there.
        let name = if cfg!(windows) {
            format!("{}.cmd", pin.bin)
        } else {
            pin.bin.to_owned()
        };
        let binary = dir.join("node_modules").join(".bin").join(name);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            dir.join("installed.json"),
            serde_json::to_vec(&serde_json::json!({
                "package": pin.package,
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        binary
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use tidebreak_core::db::DbStore;

    use super::test_support::write_install;
    use super::*;

    async fn runtime(data_dir: &Path) -> CodeRuntime {
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            data_dir.join("code.db").display()
        ))
        .await
        .expect("db");
        CodeRuntime::new(
            Arc::new(db),
            data_dir.to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    async fn set_channel(runtime: &CodeRuntime, channel: HarnessUpdateChannel) {
        runtime
            .db
            .set_setting(HARNESS_UPDATE_CHANNEL_SETTING, &serde_json::json!(channel))
            .await
            .unwrap();
    }

    fn probe_of(binary: Option<&Path>) -> tidebreak_harness::HarnessProbe {
        tidebreak_harness::HarnessProbe {
            found: binary.is_some(),
            binary_path: binary.map(Path::to_path_buf),
            version: None,
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// The channel decides which of two installs is driven, and flipping it
    /// stales the probe of the other one without touching disk.
    #[tokio::test]
    async fn the_channel_selects_between_the_pin_and_a_newer_install() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::ClaudeCode;
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        let pinned = write_install(tmp.path(), kind, pin.version);
        let newer = write_install(tmp.path(), kind, "2.1.251");
        let runtime = runtime(tmp.path()).await;

        // Default channel: the pin, and the newer install is invisible.
        let selected = runtime.selected_harness(kind).await.unwrap();
        assert_eq!(selected.binary, pinned);
        assert!(
            runtime
                .probe_is_current(kind, &probe_of(Some(&pinned)))
                .await
        );
        assert!(
            !runtime
                .probe_is_current(kind, &probe_of(Some(&newer)))
                .await
        );
        assert!(runtime.selected_harness_versions().await.is_empty());
        let status = runtime.harness_release_status(kind).await;
        assert_eq!(status.managed_version.as_deref(), Some(pin.version));
        assert!(!status.update_available);

        set_channel(&runtime, HarnessUpdateChannel::Latest).await;
        let selected = runtime.selected_harness(kind).await.unwrap();
        assert_eq!(selected.version, "2.1.251");
        assert_eq!(selected.binary, newer);
        assert!(
            !runtime
                .probe_is_current(kind, &probe_of(Some(&pinned)))
                .await
        );
        assert!(
            runtime
                .probe_is_current(kind, &probe_of(Some(&newer)))
                .await
        );
        assert_eq!(
            runtime.selected_harness_versions().await,
            vec![(kind, "2.1.251".to_owned())]
        );
    }

    /// A registry answer newer than the driven install is an update; one the
    /// machine already runs is not, and on the pinned channel none is.
    #[tokio::test]
    async fn an_update_is_a_known_release_past_the_driven_install() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::Codex;
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        write_install(tmp.path(), kind, pin.version);
        let runtime = runtime(tmp.path()).await;
        runtime.harness_releases.record(kind, "0.150.0".to_owned());

        let status = runtime.harness_release_status(kind).await;
        assert_eq!(status.latest_version.as_deref(), Some("0.150.0"));
        assert!(!status.update_available, "pinned channel never offers one");

        set_channel(&runtime, HarnessUpdateChannel::Latest).await;
        let status = runtime.harness_release_status(kind).await;
        assert!(status.update_available);
        assert_eq!(status.managed_version.as_deref(), Some(pin.version));

        write_install(tmp.path(), kind, "0.150.0");
        let status = runtime.harness_release_status(kind).await;
        assert_eq!(status.managed_version.as_deref(), Some("0.150.0"));
        assert!(!status.update_available);
    }

    /// On the pinned channel, a probe that found nothing while the pin is
    /// missing is current; with the pin on disk it is stale. Neither reads
    /// the registry.
    #[tokio::test]
    async fn a_missing_pin_keeps_a_not_found_probe_current() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::Grok;
        let runtime = runtime(tmp.path()).await;
        assert!(runtime.probe_is_current(kind, &probe_of(None)).await);
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        write_install(tmp.path(), kind, pin.version);
        assert!(!runtime.probe_is_current(kind, &probe_of(None)).await);
        assert_eq!(
            runtime
                .harness_release_status(kind)
                .await
                .pinned_version
                .as_deref(),
            Some(pin.version)
        );
    }
}
