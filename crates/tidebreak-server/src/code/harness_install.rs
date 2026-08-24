//! Warm installs of pinned harness binaries, off the session-create path.
//!
//! A cold pin is an `npm install` of 37-297MB. Paying for it inside
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

use tidebreak_core::{HarnessKind, OwnerId};

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

impl CodeRuntime {
    /// Start — or report — the install of `kind`'s pin.
    ///
    /// Answers immediately in every case: the pin is already installed, an
    /// install this process started is still running, or a fresh one is now
    /// detached. Two callers never produce two installs.
    ///
    /// `deliberate` separates the two callers. A picker warms the pin because
    /// a surface opened, so a failed managed-Node install stays failed rather
    /// than restarting every time someone opens a dialog. The doctor's
    /// Install button is a person asking, so it retries Node first.
    pub(crate) fn start_harness_install(
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
        let version = Some(pin.version.to_owned());
        if tidebreak_harness::managed_binary(&self.data_dir, kind).is_some() {
            let job = HarnessInstallJob {
                kind,
                version,
                phase: PHASE_READY,
                done: true,
                error: None,
            };
            self.harness_installs.insert(job.clone());
            return Ok(job.to_snapshot());
        }
        let job = HarnessInstallJob {
            kind,
            version,
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
        retry_node: bool,
    ) {
        match self.ensure_pinned_harness(kind, retry_node).await {
            Ok(binary) => {
                self.record_pin_install(kind, Ok(()));
                // The doctor's memoized probe was taken before this install
                // and says the engine is missing. Drop it and take the cold
                // probe here, so create pays for neither.
                self.invalidate_moved_probe(kind, &binary);
                if let Ok(adapter) = self.adapter(kind) {
                    self.probe(adapter.as_ref()).await;
                }
                self.finish_harness_install(owner, kind, Ok(()));
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
        result: Result<(), String>,
    ) {
        let previous = self.harness_installs.get(kind);
        let job = HarnessInstallJob {
            kind,
            version: previous.and_then(|job| job.version),
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
