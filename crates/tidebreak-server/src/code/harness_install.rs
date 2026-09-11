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
use crate::code::types::CodeHarnessInstallSnapshot;
use crate::error::ServerError;

/// The pin is on disk and its probe is warm.
const PHASE_READY: &str = "ready";
/// `npm install` is running.
const PHASE_INSTALLING: &str = "installing";
/// The install failed; `error` says how.
const PHASE_FAILED: &str = "failed";

/// In-memory warm-install state for this process, one entry per engine and
/// channel. Not journaled: a restart drops it and the next dialog asks again.
#[derive(Debug, Default)]
struct HarnessInstallJobsInner {
    jobs: HashMap<(HarnessKind, HarnessUpdateChannel), HarnessInstallJob>,
    next_generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct HarnessInstallJobs {
    inner: Mutex<HarnessInstallJobsInner>,
}

#[derive(Debug, Clone)]
struct HarnessInstallJob {
    kind: HarnessKind,
    channel: HarnessUpdateChannel,
    version: Option<String>,
    phase: &'static str,
    done: bool,
    error: Option<String>,
    generation: u64,
}

#[derive(Debug)]
enum HarnessInstallClaim {
    Started(HarnessInstallJob),
    Running(HarnessInstallJob),
}

impl HarnessInstallJobs {
    fn key(job: &HarnessInstallJob) -> (HarnessKind, HarnessUpdateChannel) {
        (job.kind, job.channel)
    }

    fn get(&self, kind: HarnessKind, channel: HarnessUpdateChannel) -> Option<HarnessInstallJob> {
        self.inner
            .lock()
            .expect("harness install jobs")
            .jobs
            .get(&(kind, channel))
            .cloned()
    }

    /// An installed binary does not complete a different install in progress.
    fn observe_ready(&self, job: HarnessInstallJob) -> HarnessInstallJob {
        let key = Self::key(&job);
        let mut inner = self.inner.lock().expect("harness install jobs");
        match inner.jobs.get(&key) {
            Some(running) if !running.done => running.clone(),
            _ => {
                inner.jobs.insert(key, job.clone());
                job
            }
        }
    }

    /// Two callers of the same engine and channel share one install. A cold
    /// `latest` request (`version` unset) joins a later click that already
    /// knows the version. A request whose target is known and different from
    /// the running job is a distinct job and takes the slot.
    ///
    /// Return the claimed generation while holding the lock. A later request
    /// may replace the slot before the caller starts its worker.
    fn claim(&self, mut job: HarnessInstallJob) -> HarnessInstallClaim {
        let mut inner = self.inner.lock().expect("harness install jobs");
        let key = Self::key(&job);
        match inner.jobs.get(&key) {
            Some(running) if !running.done && Self::same_running_job(running, &job) => {
                HarnessInstallClaim::Running(running.clone())
            }
            _ => {
                job.generation = inner.next_generation;
                inner.next_generation += 1;
                inner.jobs.insert(key, job.clone());
                HarnessInstallClaim::Started(job)
            }
        }
    }

    fn same_running_job(running: &HarnessInstallJob, job: &HarnessInstallJob) -> bool {
        match (running.version.as_deref(), job.version.as_deref()) {
            (None, _) | (_, None) => true,
            (Some(running), Some(requested)) => running == requested,
        }
    }

    /// Mark the slot done only if `generation` still owns it. A later claim
    /// that replaced the slot is left running.
    fn finish(
        &self,
        kind: HarnessKind,
        channel: HarnessUpdateChannel,
        generation: u64,
        job: HarnessInstallJob,
    ) -> bool {
        let mut inner = self.inner.lock().expect("harness install jobs");
        match inner.jobs.get(&(kind, channel)) {
            Some(running) if running.generation == generation => {
                inner.jobs.insert((kind, channel), job);
                true
            }
            _ => false,
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
    /// now detached. Callers for the same engine, channel, and target share
    /// one install. A cold `latest` request with no known version also joins
    /// a running install for that engine and channel.
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
                channel,
                version: target,
                phase: PHASE_READY,
                done: true,
                error: None,
                generation: 0,
            };
            return Ok(self.harness_installs.observe_ready(job).to_snapshot());
        }
        let job = HarnessInstallJob {
            kind,
            channel,
            version: target,
            phase: PHASE_INSTALLING,
            done: false,
            error: None,
            generation: 0,
        };
        let claimed = match self.harness_installs.claim(job) {
            HarnessInstallClaim::Running(running) => return Ok(running.to_snapshot()),
            HarnessInstallClaim::Started(claimed) => claimed,
        };
        self.publish_harness_install(owner, &claimed);

        let runtime = Arc::clone(self);
        let owner = owner.clone();
        let generation = claimed.generation;
        tokio::spawn(async move {
            runtime
                .run_harness_install(&owner, kind, channel, generation, deliberate)
                .await;
        });
        Ok(claimed.to_snapshot())
    }

    async fn run_harness_install(
        self: Arc<Self>,
        owner: &OwnerId,
        kind: HarnessKind,
        channel: HarnessUpdateChannel,
        generation: u64,
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
                self.finish_harness_install(
                    owner,
                    kind,
                    channel,
                    generation,
                    Ok(installed.version),
                );
            }
            Err(error) => {
                self.record_pin_install(kind, Err(error.clone()));
                self.finish_harness_install(owner, kind, channel, generation, Err(error));
            }
        }
    }

    fn finish_harness_install(
        &self,
        owner: &OwnerId,
        kind: HarnessKind,
        channel: HarnessUpdateChannel,
        generation: u64,
        result: Result<String, String>,
    ) {
        let previous = self.harness_installs.get(kind, channel);
        if previous
            .as_ref()
            .is_some_and(|job| job.generation != generation)
        {
            return;
        }
        let job = HarnessInstallJob {
            kind,
            channel,
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
            generation,
        };
        if !self
            .harness_installs
            .finish(kind, channel, generation, job.clone())
        {
            return;
        }
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

    const PIN: &str = "2.1.259";

    /// The registry's last answer moves an install only when a person pressed
    /// Update. A warm-up keeps what is on disk, and a cold machine takes the
    /// answer because it has nothing to keep.
    #[test]
    fn only_a_deliberate_latest_install_takes_the_registry_answer() {
        let latest = HarnessUpdateChannel::Latest;
        assert_eq!(
            install_target(latest, PIN, Some(PIN), Some("2.1.300"), false).as_deref(),
            Some(PIN)
        );
        assert_eq!(
            install_target(latest, PIN, Some(PIN), Some("2.1.300"), true).as_deref(),
            Some("2.1.300")
        );
        assert_eq!(
            install_target(latest, PIN, None, Some("2.1.300"), false).as_deref(),
            Some("2.1.300")
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
                Some("2.1.300"),
                Some("2.1.300"),
                true
            )
            .as_deref(),
            Some(PIN)
        );
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    fn installing(channel: HarnessUpdateChannel, version: Option<&str>) -> HarnessInstallJob {
        HarnessInstallJob {
            kind: HarnessKind::ClaudeCode,
            channel,
            version: version.map(str::to_owned),
            phase: PHASE_INSTALLING,
            done: false,
            error: None,
            generation: 0,
        }
    }

    fn started(claim: HarnessInstallClaim) -> HarnessInstallJob {
        match claim {
            HarnessInstallClaim::Started(job) => job,
            HarnessInstallClaim::Running(_) => panic!("expected a distinct install"),
        }
    }

    #[test]
    fn cold_latest_none_then_resolved_version_joins_the_running_install() {
        let jobs = HarnessInstallJobs::default();
        let first = started(jobs.claim(installing(HarnessUpdateChannel::Latest, None)));
        let HarnessInstallClaim::Running(running) =
            jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.300")))
        else {
            panic!("second claim must join the running install");
        };
        assert!(!running.done);
        assert_eq!(running.version, None);
        assert_eq!(running.channel, HarnessUpdateChannel::Latest);
        assert_eq!(running.generation, first.generation);
        assert!(matches!(
            jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.300"))),
            HarnessInstallClaim::Running(_)
        ));
    }

    #[test]
    fn latest_warmup_for_installed_does_not_join_deliberate_newer_target() {
        let jobs = HarnessInstallJobs::default();
        let first = started(jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.259"))));
        let running =
            started(jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.300"))));
        assert!(!running.done);
        assert_eq!(running.version.as_deref(), Some("2.1.300"));
        assert_eq!(running.phase, PHASE_INSTALLING);
        assert_ne!(running.generation, first.generation);
    }

    #[test]
    fn pinned_jobs_for_different_versions_are_distinct() {
        let jobs = HarnessInstallJobs::default();
        let first = started(jobs.claim(installing(HarnessUpdateChannel::Pinned, Some("2.1.259"))));
        let running =
            started(jobs.claim(installing(HarnessUpdateChannel::Pinned, Some("2.1.300"))));
        assert_eq!(running.version.as_deref(), Some("2.1.300"));
        assert_ne!(running.generation, first.generation);
    }

    #[test]
    fn a_started_claim_keeps_its_generation_after_another_request_replaces_the_slot() {
        let jobs = HarnessInstallJobs::default();
        let first_claim = jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.259")));
        // Another request replaces the slot before the first caller starts
        // its worker. The first claim must still carry its own generation.
        let second = started(jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.300"))));
        let first = started(first_claim);
        assert_ne!(first.generation, second.generation);
        assert_eq!(first.version.as_deref(), Some("2.1.259"));
        let finished = HarnessInstallJob {
            done: true,
            phase: PHASE_READY,
            ..first.clone()
        };
        assert!(!jobs.finish(first.kind, first.channel, first.generation, finished));
        let still = jobs
            .get(HarnessKind::ClaudeCode, HarnessUpdateChannel::Latest)
            .expect("second still owns the slot");
        assert_eq!(still.generation, second.generation);
        assert!(!still.done);
        assert_eq!(still.version.as_deref(), Some("2.1.300"));
    }

    #[test]
    fn observing_an_installed_version_keeps_a_newer_install_running() {
        let jobs = HarnessInstallJobs::default();
        let old = started(jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.259"))));
        let update = started(jobs.claim(installing(HarnessUpdateChannel::Latest, Some("2.1.300"))));
        let observed = jobs.observe_ready(HarnessInstallJob {
            done: true,
            phase: PHASE_READY,
            generation: 0,
            ..old
        });
        assert_eq!(observed.generation, update.generation);
        assert_eq!(observed.version, update.version);
        assert!(!observed.done);
        let finished = HarnessInstallJob {
            done: true,
            phase: PHASE_READY,
            ..update.clone()
        };
        assert!(jobs.finish(update.kind, update.channel, update.generation, finished));
        let stored = jobs.get(update.kind, update.channel).unwrap();
        assert!(stored.done);
        assert_eq!(stored.version.as_deref(), Some("2.1.300"));
    }
}
