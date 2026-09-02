//! Warm installs of harness binaries, off the session-create path.
//!
//! A cold engine is an `npm install` of 37-297MB. Paying for it inside
//! `POST /code/workspaces/{id}/sessions` turns create into a minutes-long
//! stall with nothing on screen, so the surface that knows which engine is
//! about to be used starts the install ahead of need and watches it the way
//! it watches a clone: a detached task, progress on the owner's live bus, and
//! a snapshot returned to whoever asked for it.
//!
//! The create path keeps its own ensure. Correctness does not depend on the
//! warm install having run — or having succeeded — and the per-pin lock in
//! `tidebreak_harness` means a create that arrives mid-install waits for that
//! install instead of starting a second one.
//!
//! npm reports no usable percentage to a pipe, so this reports phases rather
//! than a bar: `installing`, then `ready` or `failed`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tidebreak_core::{HarnessKind, HarnessUpdateChannel, OwnerId};

use super::bus::{CodeLiveUpdate, HarnessInstallProgress};
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::routes::code::CodeHarnessInstallSnapshot;

/// The pin is on disk and its probe is warm.
const PHASE_READY: &str = "ready";
/// `npm install` is running.
const PHASE_INSTALLING: &str = "installing";
/// The install failed; `error` says how.
const PHASE_FAILED: &str = "failed";

/// In-memory warm-install state for this process, one entry per engine. Not
/// journaled: a restart drops it and the next dialog asks again.
#[derive(Debug, Default)]
pub(crate) struct HarnessInstallJobs {
    jobs: Mutex<HashMap<HarnessKind, HarnessInstallJob>>,
}

#[derive(Debug, Clone)]
struct HarnessInstallJob {
    kind: HarnessKind,
    version: Option<String>,
    phase: &'static str,
    done: bool,
    error: Option<String>,
}

impl HarnessInstallJobs {
    fn get(&self, kind: HarnessKind) -> Option<HarnessInstallJob> {
        self.jobs
            .lock()
            .expect("harness install jobs")
            .get(&kind)
            .cloned()
    }

    fn insert(&self, job: HarnessInstallJob) {
        self.jobs
            .lock()
            .expect("harness install jobs")
            .insert(job.kind, job);
    }

    /// Take the install slot for `job`'s engine, or report the install
    /// already in it. One lock covers the check and the claim, so two
    /// requests that arrive together produce one install.
    fn claim(&self, job: HarnessInstallJob) -> Option<HarnessInstallJob> {
        let mut jobs = self.jobs.lock().expect("harness install jobs");
        match jobs.get(&job.kind) {
            Some(running) if !running.done && running.version == job.version => {
                Some(running.clone())
            }
            _ => {
                jobs.insert(job.kind, job);
                None
            }
        }
    }
}

impl HarnessInstallJob {
    fn to_snapshot(&self) -> CodeHarnessInstallSnapshot {
        CodeHarnessInstallSnapshot {
            kind: self.kind,
            version: self.version.clone(),
            phase: self.phase.to_owned(),
            done: self.done,
            error: self.error.clone(),
        }
    }
}

/// The version an install will produce, when it is known before the
/// registry answers. `None` means the detached task resolves it and reports
/// it when done.
///
/// On `pinned`, the pin. On `latest`, a deliberate install — Update — moves
/// to the registry's last answer; anything else keeps what is installed,
/// because the doctor row still shows Update and the reader has not opted
/// in. A cold machine takes the last answer either way: there is nothing
/// installed for it to keep.
fn install_target(
    channel: HarnessUpdateChannel,
    pin: &str,
    installed: Option<&str>,
    known_latest: Option<&str>,
    deliberate: bool,
) -> Option<String> {
    let past_pin = known_latest
        .filter(|latest| tidebreak_harness::compare_versions(latest, pin).is_ge())
        .map(str::to_owned);
    match channel {
        HarnessUpdateChannel::Pinned => Some(pin.to_owned()),
        HarnessUpdateChannel::Latest if deliberate => past_pin,
        HarnessUpdateChannel::Latest => installed.map(str::to_owned).or(past_pin),
    }
}

impl CodeRuntime {
    /// Start — or report — the install of the release the channel drives for
    /// `kind`.
    ///
    /// Answers immediately in every case: the release is already installed,
    /// an install this process started is still running, or a fresh one is
    /// now detached. Two callers never produce two installs.
    ///
    /// `deliberate` separates the two callers. A picker warms the engine
    /// because a surface opened, so a failed managed-Node install stays
    /// failed rather than restarting every time someone opens a dialog, and
    /// on the `latest` channel whatever is installed is good enough. The
    /// doctor's Download or Update button is a person asking, so it retries
    /// Node first and, on `latest`, asks the registry for the newest release.
    pub(crate) async fn start_harness_install(
        self: &Arc<Self>,
        owner: &OwnerId,
        kind: HarnessKind,
        deliberate: bool,
    ) -> Result<CodeHarnessInstallSnapshot, ServerError> {
        let pin = tidebreak_harness::pin_for(kind).ok_or_else(|| {
            ServerError::unprocessable_kind(
                "harness_unavailable",
                format!("{kind} has no pinned version to install"),
            )
        })?;
        let channel = self.harness_update_channel().await;
        let installed = self.selected_harness(kind).await;
        let target = install_target(
            channel,
            pin.version,
            installed.as_ref().map(|found| found.version.as_str()),
            self.known_latest_version(kind).as_deref(),
            deliberate,
        );
        let already = match &target {
            Some(version) => {
                tidebreak_harness::managed_binary_version(&self.data_dir, kind, version).is_some()
            }
            None => false,
        };
        if already {
            let job = HarnessInstallJob {
                kind,
                version: target,
                phase: PHASE_READY,
                done: true,
                error: None,
            };
            self.harness_installs.insert(job.clone());
            return Ok(job.to_snapshot());
        }
        let job = HarnessInstallJob {
            kind,
            version: target,
            phase: PHASE_INSTALLING,
            done: false,
            error: None,
        };
        if let Some(running) = self.harness_installs.claim(job.clone()) {
            return Ok(running.to_snapshot());
        }
        self.publish_harness_install(owner, &job);

        let runtime = Arc::clone(self);
        let owner = owner.clone();
        tokio::spawn(async move {
            runtime.run_harness_install(&owner, kind, deliberate).await;
        });
        Ok(job.to_snapshot())
    }

    async fn run_harness_install(
        self: Arc<Self>,
        owner: &OwnerId,
        kind: HarnessKind,
        deliberate: bool,
    ) {
        match self.ensure_harness(kind, deliberate, deliberate).await {
            Ok(installed) => {
                self.record_pin_install(kind, Ok(()));
                // The doctor's memoized probe was taken before this install
                // and says the engine is missing. Drop it and take the cold
                // probe here, so create pays for neither.
                self.invalidate_moved_probe(kind, &installed.binary);
                if let Ok(adapter) = self.adapter(kind) {
                    self.probe(adapter.as_ref()).await;
                }
                // A session already attached copied the old file into its
                // worker at spawn; retrying its failed turn would hit the
                // same version floor. Move every idle one onto this install.
                self.resync_workers_to_selected_binaries(&[kind]).await;
                self.finish_harness_install(owner, kind, Ok(installed.version));
            }
            Err(error) => {
                self.record_pin_install(kind, Err(error.clone()));
                self.finish_harness_install(owner, kind, Err(error));
            }
        }
    }

    fn finish_harness_install(
        &self,
        owner: &OwnerId,
        kind: HarnessKind,
        result: Result<String, String>,
    ) {
        let previous = self.harness_installs.get(kind);
        let job = HarnessInstallJob {
            kind,
            version: match &result {
                Ok(version) => Some(version.clone()),
                Err(_) => previous.and_then(|job| job.version),
            },
            phase: if result.is_ok() {
                PHASE_READY
            } else {
                PHASE_FAILED
            },
            done: true,
            error: result.err(),
        };
        self.harness_installs.insert(job.clone());
        self.publish_harness_install(owner, &job);
    }

    fn publish_harness_install(&self, owner: &OwnerId, job: &HarnessInstallJob) {
        self.bus.publish_update(
            owner,
            CodeLiveUpdate::HarnessInstall(HarnessInstallProgress {
                kind: job.kind,
                version: job.version.clone(),
                phase: job.phase.to_owned(),
                done: job.done,
                error: job.error.clone(),
            }),
        );
    }
}

#[cfg(test)]
mod install_target_tests {
    use super::*;

    const PIN: &str = "2.1.234";

    /// The registry's last answer moves an install only when a person pressed
    /// Update. A warm-up keeps what is on disk, and a cold machine takes the
    /// answer because it has nothing to keep.
    #[test]
    fn only_a_deliberate_latest_install_takes_the_registry_answer() {
        let latest = HarnessUpdateChannel::Latest;
        assert_eq!(
            install_target(latest, PIN, Some(PIN), Some("2.1.258"), false).as_deref(),
            Some(PIN)
        );
        assert_eq!(
            install_target(latest, PIN, Some(PIN), Some("2.1.258"), true).as_deref(),
            Some("2.1.258")
        );
        assert_eq!(
            install_target(latest, PIN, None, Some("2.1.258"), false).as_deref(),
            Some("2.1.258")
        );
        // No answer yet: the detached task resolves it.
        assert_eq!(install_target(latest, PIN, Some(PIN), None, true), None);
        assert_eq!(install_target(latest, PIN, None, None, false), None);
        // An answer older than the pin is never a target.
        assert_eq!(
            install_target(latest, PIN, None, Some("2.1.200"), true),
            None
        );
        assert_eq!(
            install_target(
                HarnessUpdateChannel::Pinned,
                PIN,
                Some("2.1.258"),
                Some("2.1.258"),
                true
            )
            .as_deref(),
            Some(PIN)
        );
    }
}
