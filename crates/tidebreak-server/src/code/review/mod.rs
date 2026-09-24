//! Review changes: an engine that did not write a workspace's changes reads
//! them, read-only, and its findings come back as line comments on the diff.
//!
//! A review is one hidden turn of one engine session that Tidebreak starts,
//! drives, and discards. It is not a code session: it has no row, no worker,
//! and no place in the rail, and it never takes the workspace's turn lock
//! (decision 0055), so the agents working in the workspace keep working while
//! it runs.
//!
//! Read-only holds in layers:
//!
//! - The engine loses what it has for writing files, running commands, or
//!   reaching the network (`SessionSpec::read_only`), whatever the person's
//!   own rules, hooks, or MCP servers allow: Claude Code launches in plan
//!   mode with only Read, Grep, and Glob, no MCP servers, and the person's
//!   hooks off; Codex runs in its read-only OS sandbox with web search and
//!   the person's MCP servers off; opencode's plan agent gets rules that deny
//!   every tool but reading, the person's own MCP servers' tools included.
//!   Grok CLI is not offered ([`CodeRuntime::review_blocker`]): it can't
//!   turn off network access. An engine with neither a plan mode nor
//!   approvals Tidebreak can refuse is not offered either. This layer is
//!   what stops a write aimed outside the copy below; the OS enforces it for
//!   Codex.
//!
//!   Reading is not confined: a reviewer reads what the person's account
//!   can, as the coding engines do, except opencode's, whose rules keep it
//!   inside the copy. What it reads leaves only through its own model
//!   provider.
//! - Every approval the engine asks for is refused, with feedback telling it
//!   to report the change as a finding instead. Claude Code gets no
//!   permission-prompt tool at all, so print mode refuses what plan mode
//!   would ask about. The reviewer also gets no connected apps, browser,
//!   computer use, SSH agent, or forge credentials, and the repository's own
//!   engine config stays unloaded.
//! - The reviewer never works in the person's worktree. It works in a
//!   disposable copy of the reviewed state ([`snapshot`]), its own git
//!   repository with no remote that reads the person's objects through
//!   alternates, deleted when the review ends. Links in the copy are plain
//!   files, so a write inside it stays inside it, and its git has no
//!   credential helper, hooks, or transport, so nothing is pushed from it.
//!   A workspace too large to copy is refused before anything runs.
//!
//! The reviewer is asked for one JSON object of findings ([`prompt`]), read
//! strictly ([`findings`]), and each finding is checked against the diff it
//! reviewed: one on lines a hunk shows is ready to become a line comment;
//! one elsewhere is kept to be listed instead. The client anchors findings to
//! the quoted lines, as it anchors a person's comments, so they follow their
//! code as the agent keeps working.
//!
//! Reviews live in memory. One that is running when the process stops is
//! gone, and its copy is swept at the next start. When a review ends, one
//! [`Event::ReviewFinished`] row goes into the transcript of the conversation
//! it was started from.

mod findings;
mod prompt;
mod snapshot;
#[cfg(test)]
mod tests;

pub use findings::{
    bounded_text, new_side_spans, on_the_diff, read_answer, validate_finding, ReviewAnswer,
    MAX_EXPLANATION_CHARS, MAX_FINDINGS, MAX_FINDING_LINES, MAX_TITLE_CHARS,
};
pub use prompt::{review_prompt, ReviewScope, DEFAULT_REVIEW_FOCUS, MAX_FOCUS_CHARS};
pub use snapshot::{
    is_sparse, materialize, measure, review_root, reviews_root, CopySize, Measured, ReviewCopy,
};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, watch};

use tidebreak_core::{
    CapLevel, CodeReviewId, CodeWorkspace, CodeWorkspaceStatus, Event, HarnessCaps, HarnessKind,
    OwnerId, PermissionMode, ReviewOutcome, SequencedEvent, SessionId, SessionLifecycle,
    ToolDetail, ToolOutcome, TurnId, WorkspaceId,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessAdapter, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessEventSink, HarnessProbe, HarnessSession, ProjectConfig, SessionSpec, TurnInput,
    TurnOutcome,
};

use crate::code::checkpoint::{produce_diff, resolve_diff_range, DiffBounds};
use crate::code::types::{
    CodeReviewFailure, CodeReviewFailureKind, CodeReviewFinding, CodeReviewProgress,
    CodeReviewResult, CodeReviewSnapshot, CodeReviewStatus, StartCodeReviewBody,
};
use crate::code::{harness_label, CodeRuntime};
use crate::error::ServerError;

/// How long a review may run before it is stopped.
pub const REVIEW_TIME_LIMIT: Duration = Duration::from_secs(20 * 60);
/// The most files a review copies.
pub const MAX_COPY_FILES: u64 = 200_000;
/// The most bytes a review copies.
pub const MAX_COPY_BYTES: u64 = 1_000_000_000;
/// How long a stopped reviewer gets to wind down before it is shut down.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long a finished reviewer gets to shut down.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// Ended reviews kept per workspace, newest first.
const KEEP_PER_WORKSPACE: usize = 10;
/// How long an ended review stays listed.
const KEEP_HOURS: i64 = 24;
/// Longest model id passed to an engine.
const MAX_MODEL_CHARS: usize = 200;
/// Longest failure message, in characters.
const MAX_FAILURE_CHARS: usize = 600;
/// Longest progress line, in characters.
const MAX_ACTIVITY_CHARS: usize = 120;
/// Most of a reviewer's streamed answer kept, in bytes.
const MAX_ANSWER_TAIL_BYTES: usize = 1 << 20;
/// Most of the reviewed diff a result carries, in bytes.
const MAX_RESULT_DIFF_BYTES: usize = 1 << 20;

/// What a refused approval tells the reviewer.
pub const REFUSAL_FEEDBACK: &str = "This is a read-only review: do not change files, run commands that write, or push. Report what should change as a finding in your answer instead.";

/// The permission mode an engine reviews in: plan mode where it has one, or
/// Ask with every request refused where it has a structured approval
/// channel instead. `None` when it has neither and cannot review.
#[must_use]
pub fn review_permission_mode(caps: &HarnessCaps) -> Option<PermissionMode> {
    if caps.plan_mode == CapLevel::Supported {
        Some(PermissionMode::Plan)
    } else if caps.structured_approvals == CapLevel::Supported {
        Some(PermissionMode::Ask)
    } else {
        None
    }
}

/// The reviews this process has run, and the ones running now.
#[derive(Default)]
pub struct ReviewRegistry {
    entries: Mutex<HashMap<CodeReviewId, ReviewEntry>>,
    time_limit: Mutex<Option<Duration>>,
    copy_limits: Mutex<Option<CopySize>>,
    /// Why each engine install cannot review read-only here, when it cannot,
    /// as its adapter last answered. Keyed by the binary and version, and
    /// cleared with the probes.
    blockers: Mutex<HashMap<BlockerKey, Option<String>>>,
}

/// One engine install, as the read-only check keys it.
type BlockerKey = (HarnessKind, Option<PathBuf>, Option<String>);

struct ReviewEntry {
    owner: OwnerId,
    snapshot: CodeReviewSnapshot,
    cancel: watch::Sender<bool>,
}

impl ReviewRegistry {
    /// Register a review unless the workspace already has one running.
    fn admit(
        &self,
        owner: OwnerId,
        snapshot: CodeReviewSnapshot,
    ) -> Result<watch::Receiver<bool>, ServerError> {
        let mut entries = self.entries.lock().expect("reviews");
        if entries.values().any(|entry| {
            entry.snapshot.workspace_id == snapshot.workspace_id
                && entry.snapshot.status == CodeReviewStatus::Running
        }) {
            return Err(ServerError::conflict_kind(
                "review_running",
                "a review of this workspace is already running; wait for it or stop it first",
            ));
        }
        let (cancel, cancelled) = watch::channel(false);
        entries.insert(
            snapshot.id,
            ReviewEntry {
                owner,
                snapshot,
                cancel,
            },
        );
        Ok(cancelled)
    }

    fn update(&self, id: CodeReviewId, change: impl FnOnce(&mut CodeReviewSnapshot)) {
        if let Some(entry) = self.entries.lock().expect("reviews").get_mut(&id) {
            change(&mut entry.snapshot);
        }
    }

    /// The workspace's reviews, newest first. Only the newest carries its
    /// result, so the list stays small however many reviews ended; read an
    /// older one by id for its findings.
    #[must_use]
    pub fn list(&self, owner: &OwnerId, workspace: WorkspaceId) -> Vec<CodeReviewSnapshot> {
        let mut reviews: Vec<_> = self
            .entries
            .lock()
            .expect("reviews")
            .values()
            .filter(|entry| entry.owner == *owner && entry.snapshot.workspace_id == workspace)
            .map(|entry| entry.snapshot.clone())
            .collect();
        reviews.sort_by_key(|review| std::cmp::Reverse(review.started_at));
        for older in reviews.iter_mut().skip(1) {
            older.result = None;
        }
        reviews
    }

    fn get(
        &self,
        owner: &OwnerId,
        workspace: WorkspaceId,
        id: CodeReviewId,
    ) -> Option<CodeReviewSnapshot> {
        self.entries
            .lock()
            .expect("reviews")
            .get(&id)
            .filter(|entry| entry.owner == *owner && entry.snapshot.workspace_id == workspace)
            .map(|entry| entry.snapshot.clone())
    }

    /// Ask a running review to stop. Returns it as it stands.
    fn cancel(
        &self,
        owner: &OwnerId,
        workspace: WorkspaceId,
        id: CodeReviewId,
    ) -> Option<CodeReviewSnapshot> {
        let entries = self.entries.lock().expect("reviews");
        let entry = entries
            .get(&id)
            .filter(|entry| entry.owner == *owner && entry.snapshot.workspace_id == workspace)?;
        if entry.snapshot.status == CodeReviewStatus::Running {
            let _ = entry.cancel.send(true);
        }
        Some(entry.snapshot.clone())
    }

    /// Reviews still running, by the name of their copy.
    fn running_copies(&self) -> HashSet<String> {
        self.entries
            .lock()
            .expect("reviews")
            .values()
            .filter(|entry| entry.snapshot.status == CodeReviewStatus::Running)
            .map(|entry| entry.snapshot.id.to_string())
            .collect()
    }

    /// Forget ended reviews past the ones worth listing.
    fn prune(&self) {
        let cutoff = Utc::now() - chrono::Duration::hours(KEEP_HOURS);
        let mut entries = self.entries.lock().expect("reviews");
        let mut ended: HashMap<WorkspaceId, Vec<(chrono::DateTime<Utc>, CodeReviewId)>> =
            HashMap::new();
        for entry in entries.values() {
            if entry.snapshot.status.is_finished() {
                ended
                    .entry(entry.snapshot.workspace_id)
                    .or_default()
                    .push((entry.snapshot.started_at, entry.snapshot.id));
            }
        }
        for (_, mut reviews) in ended {
            reviews.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
            for (index, (started, id)) in reviews.into_iter().enumerate() {
                if index >= KEEP_PER_WORKSPACE || started < cutoff {
                    entries.remove(&id);
                }
            }
        }
    }

    /// How long a review may run.
    #[must_use]
    pub fn time_limit(&self) -> Duration {
        self.time_limit
            .lock()
            .expect("review time limit")
            .unwrap_or(REVIEW_TIME_LIMIT)
    }

    /// Shorten the time limit, so a test can watch a review run out of time.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_time_limit(&self, limit: Duration) {
        *self.time_limit.lock().expect("review time limit") = Some(limit);
    }

    /// The most a review copies: files, and their bytes.
    #[must_use]
    pub fn copy_limits(&self) -> CopySize {
        self.copy_limits
            .lock()
            .expect("review copy limits")
            .unwrap_or(CopySize {
                files: MAX_COPY_FILES,
                bytes: MAX_COPY_BYTES,
            })
    }

    /// Lower the copy limits, so a test can review a workspace too large.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_copy_limits(&self, limits: CopySize) {
        *self.copy_limits.lock().expect("review copy limits") = Some(limits);
    }

    /// Forget every read-only check, so the next one asks again.
    pub fn clear_blockers(&self) {
        self.blockers.lock().expect("review blockers").clear();
    }
}

impl CodeRuntime {
    /// Why `adapter`'s engine cannot review read-only, when it cannot, such
    /// as Grok CLI, which can't turn off network access. Asked once per
    /// install and remembered until the probes are refreshed. Asked for an
    /// engine that is not installed too: installing it would not help.
    pub async fn review_blocker(
        &self,
        adapter: &dyn HarnessAdapter,
        probe: &HarnessProbe,
    ) -> Option<String> {
        let key = (
            adapter.kind(),
            probe.binary_path.clone(),
            probe.version.clone(),
        );
        let cached = self
            .reviews
            .blockers
            .lock()
            .expect("review blockers")
            .get(&key)
            .cloned();
        if let Some(blocker) = cached {
            return blocker;
        }
        let blocker = adapter.read_only_blocker(probe).await;
        self.reviews
            .blockers
            .lock()
            .expect("review blockers")
            .insert(key, blocker.clone());
        blocker
    }
}

/// Delete the copies of reviews that are not running, such as ones a crash
/// left behind.
pub fn sweep_copies(data_dir: &Path, registry: &ReviewRegistry) {
    snapshot::sweep(data_dir, &registry.running_copies());
}

/// Everything a review needs once it starts.
struct ReviewJob {
    id: CodeReviewId,
    owner: OwnerId,
    session_id: SessionId,
    harness: HarnessKind,
    model: Option<String>,
    turn_id: Option<TurnId>,
    permission_mode: PermissionMode,
    adapter: Arc<dyn HarnessAdapter>,
    probe: HarnessProbe,
    worktree: PathBuf,
    from: String,
    to: String,
    prompt: String,
}

/// How a review ended, before it is recorded.
enum ReviewEnd {
    Answered(String),
    Failed(CodeReviewFailure),
    Cancelled,
    TimedOut,
}

/// Why a turn was stopped from outside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    TimedOut,
}

impl CodeRuntime {
    /// Start a review of a workspace's changes by `body.harness`.
    ///
    /// Refuses before anything runs when the engine is not installed or not
    /// signed in (`422 harness_not_found`, `422 harness_not_authenticated`),
    /// cannot review read-only, such as Grok CLI, which can't turn off network
    /// access (`422 review_engine_unsupported`), or is above
    /// the managed ceiling (`409 permission_mode_locked`); when there is
    /// nothing to review (`409 review_empty`); and while another review of the
    /// workspace runs (`409 review_running`). Returns as soon as the review is
    /// registered; it runs on a task of its own.
    pub async fn start_review(
        self: &Arc<Self>,
        owner: &OwnerId,
        workspace: CodeWorkspace,
        body: StartCodeReviewBody,
        permission_mode_ceiling: Option<PermissionMode>,
    ) -> Result<CodeReviewSnapshot, ServerError> {
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace runs in a sandbox; review changes from a workspace on this machine",
            ));
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let session = self.get_session(owner, body.session_id).await?;
        if session.workspace_id != Some(workspace.id) {
            return Err(ServerError::not_found("code session not found"));
        }
        let harness = body.harness;
        let label = harness_label(harness);
        if harness.is_in_process() {
            return Err(ServerError::unprocessable_kind(
                "review_engine_unsupported",
                format!("{label} does not review workspace changes"),
            ));
        }
        let model = review_model(body.model)?;
        let focus = review_focus(body.instructions)?;
        let adapter = self.adapter(harness)?;
        let probe = self
            .prepare_session_harness(harness, adapter.as_ref())
            .await?;
        let caps = adapter.capabilities(&probe);
        let permission_mode = review_permission_mode(&caps).ok_or_else(|| {
            ServerError::unprocessable_kind(
                "review_engine_unsupported",
                format!(
                    "{label} can't review read-only on this machine: it has no plan mode and no approvals Tidebreak can refuse"
                ),
            )
        })?;
        if let Some(ceiling) = permission_mode_ceiling {
            if permission_mode > ceiling {
                return Err(ServerError::conflict_kind(
                    "permission_mode_locked",
                    format!(
                        "{label} reviews in `{}`, above the most this managed profile allows (`{}`)",
                        permission_mode.as_str(),
                        ceiling.as_str()
                    ),
                ));
            }
        }
        // Fail closed: an engine whose read-only posture rests on something
        // this machine cannot give it does not review here.
        if let Some(reason) = self.review_blocker(adapter.as_ref(), &probe).await {
            return Err(ServerError::unprocessable_kind(
                "review_engine_unsupported",
                reason,
            ));
        }
        let (worktree, from, to, turn_id) = resolve_diff_range(&self.db, &workspace, body.turn_id)
            .await
            .map_err(super::runtime::map_checkpoint)?;
        let diff = produce_diff(&worktree, &from, &to, None, DiffBounds::default())
            .await
            .map_err(super::runtime::map_checkpoint)?;
        if diff.diff.trim().is_empty() {
            return Err(ServerError::conflict_kind(
                "review_empty",
                "there are no changes to review",
            ));
        }
        let limits = self.reviews.copy_limits();
        match measure(&worktree, &to, limits.files, limits.bytes).await {
            Ok(Measured::Within(_)) => {}
            Ok(Measured::TooLarge(size)) => {
                return Err(ServerError::unprocessable_kind(
                    "review_too_large",
                    too_large_message(size, limits, is_sparse(&worktree).await),
                ));
            }
            Err(detail) => {
                return Err(ServerError::internal(format!(
                    "Tidebreak could not measure the files to review: {detail}"
                )));
            }
        }
        let scope = match turn_id {
            None => ReviewScope::WorkingTree {
                base: &workspace.base_ref,
            },
            Some(turn) => ReviewScope::Turn {
                ordinal: tidebreak_core::db::code::get_turn(&self.db, owner, turn)
                    .await?
                    .map(|turn| turn.ordinal),
            },
        };
        let prompt = review_prompt(scope, &diff.diff, diff.truncated, focus.as_deref());
        let snapshot = CodeReviewSnapshot {
            id: CodeReviewId::new(),
            workspace_id: workspace.id,
            session_id: session.id,
            harness,
            model: model.clone(),
            turn_id,
            permission_mode,
            status: CodeReviewStatus::Running,
            progress: CodeReviewProgress {
                activity: Some("Copying the changes".to_owned()),
                ..CodeReviewProgress::default()
            },
            started_at: Utc::now(),
            finished_at: None,
            failure: None,
            result: None,
        };
        let cancelled = self.reviews.admit(owner.clone(), snapshot.clone())?;
        let job = ReviewJob {
            id: snapshot.id,
            owner: owner.clone(),
            session_id: session.id,
            harness,
            model,
            turn_id,
            permission_mode,
            adapter,
            probe,
            worktree,
            from,
            to,
            prompt,
        };
        let runtime = Arc::clone(self);
        tokio::spawn(async move { runtime.run_review(job, cancelled).await });
        Ok(snapshot)
    }

    /// The workspace's reviews, newest first.
    #[must_use]
    pub fn list_reviews(&self, owner: &OwnerId, workspace: WorkspaceId) -> Vec<CodeReviewSnapshot> {
        self.reviews.list(owner, workspace)
    }

    /// One review.
    pub fn get_review(
        &self,
        owner: &OwnerId,
        workspace: WorkspaceId,
        id: CodeReviewId,
    ) -> Result<CodeReviewSnapshot, ServerError> {
        self.reviews
            .get(owner, workspace, id)
            .ok_or_else(|| ServerError::not_found("review not found"))
    }

    /// Stop a running review. An ended one is returned as it is.
    pub fn cancel_review(
        &self,
        owner: &OwnerId,
        workspace: WorkspaceId,
        id: CodeReviewId,
    ) -> Result<CodeReviewSnapshot, ServerError> {
        self.reviews
            .cancel(owner, workspace, id)
            .ok_or_else(|| ServerError::not_found("review not found"))
    }

    async fn run_review(self: Arc<Self>, job: ReviewJob, mut cancelled: watch::Receiver<bool>) {
        let root = review_root(&self.data_dir, job.id);
        let end = self.drive_review(&job, &root, &mut cancelled).await;
        snapshot::remove(&root).await;
        self.finish_review(&job, end).await;
    }

    /// Copy the changes, run the reviewer's one turn, and shut it down.
    async fn drive_review(
        &self,
        job: &ReviewJob,
        root: &Path,
        cancelled: &mut watch::Receiver<bool>,
    ) -> ReviewEnd {
        let label = harness_label(job.harness);
        let deadline = tokio::time::Instant::now() + self.reviews.time_limit();
        let copy = tokio::select! {
            copy = materialize(&job.worktree, &job.from, &job.to, root) => copy,
            () = stop_requested(cancelled) => return ReviewEnd::Cancelled,
            () = tokio::time::sleep_until(deadline) => return ReviewEnd::TimedOut,
        };
        let copy = match copy {
            Ok(copy) => copy,
            Err(detail) => {
                return ReviewEnd::Failed(failure(
                    CodeReviewFailureKind::Failed,
                    format!("Tidebreak could not copy the changes for the review: {detail}"),
                ))
            }
        };
        self.reviews.update(job.id, |review| {
            review.progress.activity = Some(format!("Starting {label}"));
        });

        let (events_tx, mut events) = mpsc::unbounded_channel();
        let relay = self.review_relay(job);
        let mut extra_env = relay.env.clone();
        extra_env.extend(reviewer_git_env(&copy.git_config));
        let spec = SessionSpec {
            owner: job.owner.clone(),
            // No session row stands behind a review; the id is its own.
            session_id: SessionId::new(),
            worktree: copy.tree.clone(),
            allowed_read_roots: Vec::new(),
            permission_mode: job.permission_mode,
            model: job.model.clone(),
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: relay.argv.clone(),
            extra_env,
            relay_key_env: relay.key_env.clone(),
            env: reviewer_env(&job.probe.env),
            // No approval channel: an engine that would ask for one is
            // refused, by the engine itself or by `deny` below.
            approval: None,
            binary: job.probe.binary_path.clone(),
            sink: Arc::new(ReviewSink { events: events_tx }),
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
            project_config: ProjectConfig::Skip,
            // The engine's tools that write files or run commands are taken
            // away, whatever the person's own rules allow.
            read_only: true,
        };
        let session = match job.adapter.launch(spec).await {
            Ok(session) => session,
            Err(error) => {
                self.revoke_review_relay(&relay);
                return ReviewEnd::Failed(classify_harness_error(job.harness, &error));
            }
        };
        self.reviews.update(job.id, |review| {
            review.progress.activity = Some(format!("{label} is reading the changes"));
        });

        let mut trace = ReviewTrace::new(copy.tree.clone());
        let mut stop = None;
        let outcome = {
            let input = TurnInput {
                turn_id: None,
                text: job.prompt.clone(),
                model: job.model.clone(),
                reasoning_effort: None,
                fast_mode: false,
                images: Vec::new(),
            };
            let engine: &dyn HarnessSession = session.as_ref();
            let turn = engine.run_turn(input);
            tokio::pin!(turn);
            // Decisions and a stop run beside the turn: an engine can need
            // its turn read to hear either one.
            let mut calls: FuturesUnordered<EngineCall<'_>> = FuturesUnordered::new();
            let mut stop_deadline = None;
            loop {
                tokio::select! {
                    outcome = &mut turn => break Some(outcome),
                    Some(event) = events.recv() => {
                        if let Some(approval) = trace.observe(&event) {
                            calls.push(engine.decide(
                                approval,
                                ApprovalDecision::Deny {
                                    feedback: Some(REFUSAL_FEEDBACK.to_owned()),
                                },
                            ));
                        }
                        let progress = trace.progress();
                        self.reviews.update(job.id, |review| review.progress = progress);
                    }
                    Some(result) = calls.next(), if !calls.is_empty() => {
                        if let Err(error) = result {
                            tracing::debug!(review = %job.id, %error, "a reviewer call failed");
                        }
                    }
                    () = stop_requested(cancelled), if stop.is_none() => {
                        stop = Some(Stop::Cancelled);
                        stop_deadline = Some(tokio::time::Instant::now() + STOP_GRACE);
                        calls.push(engine.interrupt());
                    }
                    () = tokio::time::sleep_until(deadline), if stop.is_none() => {
                        stop = Some(Stop::TimedOut);
                        stop_deadline = Some(tokio::time::Instant::now() + STOP_GRACE);
                        calls.push(engine.interrupt());
                    }
                    () = sleep_until_some(stop_deadline) => break None,
                }
            }
        };
        while let Ok(event) = events.try_recv() {
            let _ = trace.observe(&event);
        }
        if tokio::time::timeout(SHUTDOWN_GRACE, session.shutdown())
            .await
            .is_err()
        {
            tracing::warn!(review = %job.id, "a reviewer did not shut down in time");
        }
        self.revoke_review_relay(&relay);

        match stop {
            Some(Stop::Cancelled) => return ReviewEnd::Cancelled,
            Some(Stop::TimedOut) => return ReviewEnd::TimedOut,
            None => {}
        }
        match outcome {
            None => ReviewEnd::TimedOut,
            Some(Err(error)) => ReviewEnd::Failed(classify_harness_error(job.harness, &error)),
            Some(Ok(TurnOutcome::Incomplete { detail })) => ReviewEnd::Failed(classify_failure(
                job.harness,
                trace.failure.as_deref().unwrap_or(&detail),
            )),
            Some(Ok(TurnOutcome::Parked { .. })) => ReviewEnd::Failed(failure(
                CodeReviewFailureKind::Failed,
                format!("{label} paused the review to wait for something a review cannot give it"),
            )),
            Some(Ok(TurnOutcome::Clean)) => {
                if let Some(message) = &trace.failure {
                    ReviewEnd::Failed(classify_failure(job.harness, message))
                } else if trace.interrupted {
                    ReviewEnd::Failed(failure(
                        CodeReviewFailureKind::Failed,
                        format!("{label} stopped before it finished the review"),
                    ))
                } else {
                    match trace.final_text() {
                        Some(text) => ReviewEnd::Answered(text),
                        None => ReviewEnd::Failed(failure(
                            CodeReviewFailureKind::Failed,
                            format!("{label} finished without an answer"),
                        )),
                    }
                }
            }
        }
    }

    /// Record how a review ended: its snapshot, and one row in the
    /// transcript of the conversation it was started from.
    async fn finish_review(&self, job: &ReviewJob, end: ReviewEnd) {
        let (status, outcome, failure_detail, result) = match end {
            ReviewEnd::Answered(text) => {
                let result = self.place_findings(job, read_answer(&text)).await;
                (
                    CodeReviewStatus::Completed,
                    ReviewOutcome::Completed,
                    None,
                    Some(result),
                )
            }
            ReviewEnd::Failed(failure) => (
                CodeReviewStatus::Failed,
                ReviewOutcome::Failed,
                Some(failure),
                None,
            ),
            ReviewEnd::Cancelled => (
                CodeReviewStatus::Cancelled,
                ReviewOutcome::Cancelled,
                None,
                None,
            ),
            ReviewEnd::TimedOut => (
                CodeReviewStatus::TimedOut,
                ReviewOutcome::TimedOut,
                Some(failure(
                    CodeReviewFailureKind::TimedOut,
                    format!(
                        "{} ran past {} minutes and was stopped. {}",
                        harness_label(job.harness),
                        self.reviews.time_limit().as_secs().div_ceil(60),
                        if job.turn_id.is_some() {
                            "Try again, or pick a faster model."
                        } else {
                            "Try again, or review one turn's changes instead."
                        }
                    ),
                )),
                None,
            ),
        };
        let findings = result.as_ref().map_or(0, |result| {
            u32::try_from(result.findings.len() + result.unplaced.len()).unwrap_or(u32::MAX)
        });
        // The transcript row first: a client that sees the review end finds
        // it recorded.
        self.journal_review(job, outcome, findings).await;
        self.reviews.update(job.id, |review| {
            review.status = status;
            review.finished_at = Some(Utc::now());
            review.progress.activity = None;
            review.failure = failure_detail;
            review.result = result;
        });
        self.reviews.prune();
    }

    /// Check each finding against the reviewed diff of its file.
    async fn place_findings(&self, job: &ReviewJob, answer: ReviewAnswer) -> CodeReviewResult {
        let (summary, found, rejected) = match answer {
            ReviewAnswer::Unreadable { text } => {
                return CodeReviewResult {
                    summary: None,
                    findings: Vec::new(),
                    unplaced: Vec::new(),
                    rejected: 0,
                    raw_text: Some(text),
                    diff: String::new(),
                    omitted_diffs: None,
                }
            }
            ReviewAnswer::Findings {
                summary,
                findings,
                rejected,
            } => (summary, findings, rejected),
        };
        let mut diffs: HashMap<String, String> = HashMap::new();
        let mut placed = Vec::new();
        let mut unplaced = Vec::new();
        let mut shown: Vec<String> = Vec::new();
        for mut finding in found {
            let mut on_diff = None;
            for candidate in path_candidates(&finding.path) {
                if !diffs.contains_key(&candidate) {
                    let diff = produce_diff(
                        &job.worktree,
                        &job.from,
                        &job.to,
                        Some(&candidate),
                        DiffBounds::default(),
                    )
                    .await
                    .map(|diff| diff.diff)
                    .unwrap_or_default();
                    diffs.insert(candidate.clone(), diff);
                }
                if diffs
                    .get(&candidate)
                    .is_some_and(|diff| !diff.trim().is_empty())
                {
                    on_diff = Some(candidate);
                    break;
                }
            }
            let Some(path) = on_diff else {
                unplaced.push(finding);
                continue;
            };
            let spans = new_side_spans(&diffs[&path]);
            if on_the_diff(&finding, &spans) {
                finding.path = path.clone();
                if !shown.contains(&path) {
                    shown.push(path);
                }
                placed.push(finding);
            } else {
                finding.path = path;
                unplaced.push(finding);
            }
        }
        let (diff, omitted) = bounded_result_diffs(
            &shown,
            &diffs,
            &mut placed,
            &mut unplaced,
            MAX_RESULT_DIFF_BYTES,
        );
        CodeReviewResult {
            summary,
            findings: placed,
            unplaced,
            rejected,
            raw_text: None,
            diff,
            omitted_diffs: (omitted > 0).then_some(omitted),
        }
    }

    async fn journal_review(&self, job: &ReviewJob, outcome: ReviewOutcome, findings: u32) {
        let session = match tidebreak_core::db::code::get_session(
            &self.db,
            &job.owner,
            job.session_id,
        )
        .await
        {
            Ok(Some(session)) if session.lifecycle != SessionLifecycle::Ended => session,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(review = %job.id, %error, "the review's conversation could not be read");
                return;
            }
        };
        let event = Event::ReviewFinished {
            review_id: job.id,
            harness: job.harness,
            model: job.model.clone(),
            turn_id: job.turn_id,
            outcome,
            findings,
        };
        match tidebreak_core::db::code::append_event(
            &self.db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            &event,
        )
        .await
        {
            Ok(seq) => self.bus.publish(session.id, SequencedEvent { seq, event }),
            Err(error) => tracing::warn!(
                review = %job.id,
                session = %session.id,
                %error,
                "the review was not recorded in its conversation"
            ),
        }
    }

    /// Inference wiring for a machine that relays engine inference (decision
    /// 71). Only inference: a reviewer never gets the forge credentials a
    /// session's git borrows.
    fn review_relay(&self, job: &ReviewJob) -> ReviewRelay {
        let Some(relay) = self
            .harness_llm
            .as_ref()
            .filter(|relay| relay.forwards_inference())
        else {
            return ReviewRelay::default();
        };
        let Some(base) = self.loopback_base.lock().expect("loopback base").clone() else {
            return ReviewRelay::default();
        };
        let key = relay.issue_for_review(&job.owner, job.session_id, job.harness, &job.probe);
        let (argv, env) = crate::code::harness_llm::spawn_wiring(job.harness, &base, &key);
        ReviewRelay {
            argv,
            env,
            key_env: Some(crate::code::harness_llm::RELAY_KEY_ENV.to_owned()),
            key: Some(key),
        }
    }

    fn revoke_review_relay(&self, relay: &ReviewRelay) {
        if let (Some(llm), Some(key)) = (self.harness_llm.as_ref(), relay.key.as_deref()) {
            llm.revoke_key(key);
        }
    }
}

/// A relay key a reviewer borrows for inference, and how it is wired.
#[derive(Default)]
struct ReviewRelay {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    key_env: Option<String>,
    key: Option<String>,
}

type EngineCall<'a> = Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

/// Resolves once a stop is asked for; never, if the sender goes away first.
async fn stop_requested(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow_and_update() {
            return;
        }
        if cancelled.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn sleep_until_some(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The paths the diff may name a finding's file by.
fn path_candidates(path: &str) -> Vec<String> {
    let mut candidates = vec![path.to_owned()];
    for prefix in ["a/", "b/"] {
        if let Some(stripped) = path.strip_prefix(prefix) {
            if !stripped.is_empty() {
                candidates.push(stripped.to_owned());
            }
        }
    }
    candidates
}

/// The probe's environment, less what would let a reviewer push: the SSH
/// agent. Forge tokens never reach an engine child in the first place.
/// The reviewed diffs a result carries, within `max_bytes`: each file with a
/// finding on its lines, in the order the findings named them. A file whose
/// diff does not fit is left out, and its findings move to `unplaced`, where
/// a client lists them by file and lines instead of on the diff. Returns the
/// diff and how many files were left out.
fn bounded_result_diffs(
    shown: &[String],
    diffs: &HashMap<String, String>,
    placed: &mut Vec<CodeReviewFinding>,
    unplaced: &mut Vec<CodeReviewFinding>,
    max_bytes: usize,
) -> (String, u32) {
    let mut diff = String::new();
    let mut omitted = HashSet::new();
    for path in shown {
        let text = diffs[path].trim_end_matches('\n');
        if diff.len() + text.len() + 1 > max_bytes {
            omitted.insert(path.as_str());
            continue;
        }
        diff.push_str(text);
        diff.push('\n');
    }
    if !omitted.is_empty() {
        let (kept, moved): (Vec<_>, Vec<_>) = std::mem::take(placed)
            .into_iter()
            .partition(|finding| !omitted.contains(finding.path.as_str()));
        *placed = kept;
        unplaced.splice(0..0, moved);
    }
    (diff, u32::try_from(omitted.len()).unwrap_or(u32::MAX))
}

fn reviewer_env(
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    const STRIPPED: [&str; 5] = [
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "SSH_ASKPASS",
        "GIT_ASKPASS",
        "GIT_SSH_COMMAND",
    ];
    env.iter()
        .filter(|(key, _)| !STRIPPED.iter().any(|stripped| key == stripped))
        .cloned()
        .collect()
}

/// The reviewer's git, wherever an engine runs one: the copy's empty global
/// config in place of the person's, no system config, and never a prompt
/// for credentials. With the copy's own config (no helper, no transport),
/// nothing fetches or pushes with the person's credentials.
fn reviewer_git_env(global_config: &Path) -> [(String, String); 3] {
    [
        (
            "GIT_CONFIG_GLOBAL".to_owned(),
            global_config.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
    ]
}

/// Why a workspace is too large to review, in the person's terms.
fn too_large_message(size: CopySize, limits: CopySize, sparse: bool) -> String {
    let what = if size.files > limits.files {
        format!("it has more than {} files", group_thousands(limits.files))
    } else {
        format!("its files come to more than {}", format_bytes(limits.bytes))
    };
    let mut message = format!(
        "This workspace is too large to review: a review works in a copy of every file, and {what}, the most a review copies."
    );
    if sparse {
        message.push_str(
            " Its sparse checkout leaves files out of your worktree, but the copy would hold all of them.",
        );
    }
    message
}

fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "GB"), (1_000_000, "MB"), (1_000, "KB")];
    for (unit, label) in UNITS {
        if bytes >= unit {
            let whole = bytes / unit;
            let tenth = (bytes % unit) * 10 / unit;
            return if tenth == 0 {
                format!("{whole} {label}")
            } else {
                format!("{whole}.{tenth} {label}")
            };
        }
    }
    format!("{bytes} bytes")
}

/// A trimmed model id an engine can take on its argv, or `None` for the
/// engine's default.
fn review_model(model: Option<String>) -> Result<Option<String>, ServerError> {
    let Some(model) = model.map(|model| model.trim().to_owned()) else {
        return Ok(None);
    };
    if model.is_empty() {
        return Ok(None);
    }
    if model.chars().count() > MAX_MODEL_CHARS
        || model.starts_with('-')
        || model.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ServerError::bad_request_kind(
            "review_model",
            "that model name can't be passed to the engine",
        ));
    }
    Ok(Some(model))
}

fn review_focus(instructions: Option<String>) -> Result<Option<String>, ServerError> {
    let Some(focus) = instructions.map(|focus| focus.trim().to_owned()) else {
        return Ok(None);
    };
    if focus.is_empty() {
        return Ok(None);
    }
    if focus.chars().count() > MAX_FOCUS_CHARS {
        return Err(ServerError::bad_request_kind(
            "review_instructions",
            format!("review instructions are limited to {MAX_FOCUS_CHARS} characters"),
        ));
    }
    Ok(Some(focus))
}

fn failure(kind: CodeReviewFailureKind, message: String) -> CodeReviewFailure {
    CodeReviewFailure {
        kind,
        message: bounded_text(message.trim(), MAX_FAILURE_CHARS),
    }
}

fn classify_harness_error(harness: HarnessKind, error: &HarnessError) -> CodeReviewFailure {
    match error {
        HarnessError::NotFound => failure(
            CodeReviewFailureKind::NotInstalled,
            format!(
                "{} is not installed on this machine. Install it from Settings > Coding engines.",
                harness_label(harness)
            ),
        ),
        other => classify_failure(harness, &other.to_string()),
    }
}

/// Words engines and their providers use when a limit refused the call.
const RATE_LIMIT_WORDS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "ratelimit",
    "rate-limit",
    "too many requests",
    "usage limit",
    "usage_limit",
    "quota",
    "overloaded",
    "resource_exhausted",
];

/// Words engines and their providers use when a sign-in is missing or
/// refused.
const SIGNED_OUT_WORDS: &[&str] = &[
    "not logged in",
    "not signed in",
    "please log in",
    "please login",
    "/login",
    "log in again",
    "sign in again",
    "signed out",
    "unauthorized",
    "unauthenticated",
    "invalid api key",
    "invalid x-api-key",
    "authentication_error",
    "authentication failed",
    "token has expired",
    "token expired",
    "expired token",
];

/// Sort an engine's failure into what the person can do about it.
#[must_use]
pub fn classify_failure(harness: HarnessKind, detail: &str) -> CodeReviewFailure {
    let label = harness_label(harness);
    let lower = detail.to_ascii_lowercase();
    let detail = detail.trim();
    let said = if detail.is_empty() {
        String::new()
    } else {
        format!(" The engine said: {detail}")
    };
    if RATE_LIMIT_WORDS.iter().any(|word| lower.contains(word))
        || has_status(&lower, "429")
        || has_status(&lower, "529")
    {
        failure(
            CodeReviewFailureKind::RateLimited,
            format!(
                "{label} hit a rate or usage limit. Try again later, or pick another engine.{said}"
            ),
        )
    } else if SIGNED_OUT_WORDS.iter().any(|word| lower.contains(word)) || has_status(&lower, "401")
    {
        failure(
            CodeReviewFailureKind::SignedOut,
            format!(
                "{label} is not signed in, or its sign-in was refused. Sign in from Settings > Coding engines.{said}"
            ),
        )
    } else if detail.is_empty() {
        failure(
            CodeReviewFailureKind::Failed,
            format!("{label} stopped without saying why"),
        )
    } else {
        failure(
            CodeReviewFailureKind::Failed,
            format!("{label} could not finish the review: {detail}"),
        )
    }
}

/// Whether `text` holds `code` as a number of its own, not inside a longer
/// one.
fn has_status(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + code.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// Hands the reviewer's events to the review's own loop.
struct ReviewSink {
    events: mpsc::UnboundedSender<HarnessEvent>,
}

#[async_trait]
impl HarnessEventSink for ReviewSink {
    async fn emit(&self, event: HarnessEvent) {
        let _ = self.events.send(event);
    }
}

/// What the reviewer has done so far, and what it answered.
struct ReviewTrace {
    tree: PathBuf,
    last_message: Option<String>,
    tail: String,
    files: HashSet<String>,
    tool_calls: u32,
    refused: u32,
    activity: Option<String>,
    failure: Option<String>,
    interrupted: bool,
}

impl ReviewTrace {
    fn new(tree: PathBuf) -> Self {
        Self {
            tree,
            last_message: None,
            tail: String::new(),
            files: HashSet::new(),
            tool_calls: 0,
            refused: 0,
            activity: None,
            failure: None,
            interrupted: false,
        }
    }

    /// Take in one event. Returns the approval to refuse, when it asks for
    /// one.
    fn observe(&mut self, event: &HarnessEvent) -> Option<HarnessApprovalRef> {
        match event {
            HarnessEvent::AssistantDelta { text } => {
                if self.tail.len() + text.len() <= MAX_ANSWER_TAIL_BYTES {
                    self.tail.push_str(text);
                }
            }
            HarnessEvent::AssistantMessage {
                text,
                parent_call_id: None,
            } => {
                self.last_message = Some(text.clone());
                self.tail.clear();
            }
            HarnessEvent::ToolStarted {
                detail,
                parent_call_id,
                ..
            } => {
                self.tool_calls = self.tool_calls.saturating_add(1);
                if let ToolDetail::FileRead { path } = detail {
                    if self.files.len() < 100_000 {
                        self.files.insert(self.relative(path));
                    }
                }
                self.activity = self.describe(detail);
                if parent_call_id.is_none() {
                    self.tail.clear();
                }
            }
            HarnessEvent::ToolCompleted {
                outcome: ToolOutcome::Denied,
                ..
            } => self.refused = self.refused.saturating_add(1),
            HarnessEvent::ApprovalRequested { harness_ref, .. } => {
                self.refused = self.refused.saturating_add(1);
                self.activity = Some("Refused a request to change something".to_owned());
                return Some(harness_ref.clone());
            }
            HarnessEvent::TurnFailed { error } => self.failure = Some(error.message.clone()),
            HarnessEvent::TurnInterrupted => self.interrupted = true,
            _ => {}
        }
        None
    }

    fn progress(&self) -> CodeReviewProgress {
        CodeReviewProgress {
            tool_calls: self.tool_calls,
            files_read: u32::try_from(self.files.len()).unwrap_or(u32::MAX),
            refused: self.refused,
            activity: self.activity.clone(),
        }
    }

    /// The reviewer's closing message: what it streamed after its last
    /// complete message, or that message.
    fn final_text(&self) -> Option<String> {
        let tail = self.tail.trim();
        if !tail.is_empty() {
            return Some(tail.to_owned());
        }
        self.last_message
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    /// A path inside the copy, as the repository names it.
    fn relative(&self, path: &str) -> String {
        Path::new(path)
            .strip_prefix(&self.tree)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_owned())
    }

    fn describe(&self, detail: &ToolDetail) -> Option<String> {
        let line = match detail {
            ToolDetail::FileRead { path } => format!("Reading {}", self.relative(path)),
            ToolDetail::Search { query } => format!("Searching for {query}"),
            ToolDetail::Command { cmd, .. } => format!("Running {cmd}"),
            ToolDetail::FileEdit { path } => {
                format!("Refused an edit to {}", self.relative(path))
            }
            ToolDetail::Other { summary } if !summary.trim().is_empty() => summary.clone(),
            ToolDetail::Other { .. } => return None,
        };
        let line = line.lines().next().unwrap_or_default().trim().to_owned();
        (!line.is_empty()).then(|| bounded_text(&line, MAX_ACTIVITY_CHARS))
    }
}
