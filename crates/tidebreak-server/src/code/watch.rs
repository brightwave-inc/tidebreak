//! Durable watch tasks: a server-side loop that keeps one workspace's pull
//! request moving until it merges or genuinely needs the user.
//!
//! The watch owns a dedicated [`CodeSessionKind::Watch`] session in the same
//! worktree as the conversation it forked from. Each sweep reads the PR
//! digest through the normal `workspace_pr` path (so the updates channel sees
//! every read), classifies it, and either waits, submits one bounded fix
//! turn, or parks the watch with a reason. The sweep is try-based and reads
//! its work list from the `code_watch` table every tick, so a restart resumes
//! every active watch with no extra recovery state (decision 9's promoter is
//! the precedent).
//!
//! The watch never merges, never arms auto-merge, and never marks a draft
//! ready: decision 42 reserves PR state changes for the user. It reports
//! those moments as `NeedsYou` instead.

use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use tracing::warn;

use tidebreak_core::db::code::{
    get_session, insert_watch, latest_watch_for_workspace, list_active_watches_all_owners,
    list_sessions_for_workspace, save_watch,
};
use tidebreak_core::{
    Attention, AttentionSource, CodePermissionMode, CodeSessionKind, CodeSessionLifecycle,
    CodeWatch, CodeWatchId, CodeWatchState, CodeWorkspaceStatus, HarnessKind, OwnerId,
    PullRequestDigest, WorkspaceId,
};

use super::attention::{apply_attention, emit_workspace_digests};
use super::runtime::{CodeRuntime, NewSessionSettings};
use crate::error::ServerError;

/// How often the watch sweep walks active watches.
pub(crate) const WATCH_SWEEP_INTERVAL: Duration = Duration::from_secs(47);

/// What one PR digest asks of the watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WatchAssessment {
    /// The pull request merged; the watch is complete.
    Merged,
    /// The pull request closed without merging.
    Closed,
    /// Something a fix turn can address.
    Actionable(WatchReason),
    /// The host is still working; keep polling.
    Waiting,
    /// Every requirement the watch can affect is green.
    Ready,
    /// Only the user can advance it; keep polling in case it clears.
    NeedsUser(&'static str),
}

/// The concrete condition a fix turn is asked to address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchReason {
    /// One or more checks failed.
    FailingChecks,
    /// The branch conflicts with its base.
    Conflicts,
    /// The branch is behind its base and must be updated.
    Behind,
    /// A reviewer requested changes.
    ChangesRequested,
}

impl WatchReason {
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::FailingChecks => "failing checks",
            Self::Conflicts => "merge conflicts",
            Self::Behind => "the branch is behind its base",
            Self::ChangesRequested => "requested changes",
        }
    }
}

/// Classify one digest the way the workflow control does, minus the actions
/// decision 42 reserves for the user.
pub(crate) fn assess(pr: &PullRequestDigest) -> WatchAssessment {
    let state = pr.state.trim().to_ascii_lowercase();
    if pr.merged == Some(true) || state == "merged" {
        return WatchAssessment::Merged;
    }
    if state == "closed" {
        return WatchAssessment::Closed;
    }
    if pr.draft == Some(true) {
        return WatchAssessment::NeedsUser("the pull request is a draft");
    }
    let mergeable = pr.mergeable.as_deref().map(str::trim).unwrap_or("");
    let merge_state = pr
        .merge_state_status
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if mergeable == "conflicting" || merge_state == "dirty" {
        return WatchAssessment::Actionable(WatchReason::Conflicts);
    }
    if pr.review_decision.as_deref().map(str::trim) == Some("changes_requested") {
        return WatchAssessment::Actionable(WatchReason::ChangesRequested);
    }
    let checks = pr.checks.as_deref().unwrap_or(&[]);
    let failing = checks
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Fail)
        .count();
    if failing > 0 {
        return WatchAssessment::Actionable(WatchReason::FailingChecks);
    }
    if pr.in_merge_queue == Some(true) {
        return WatchAssessment::Waiting;
    }
    if merge_state == "behind" {
        return WatchAssessment::Actionable(WatchReason::Behind);
    }
    if merge_state == "blocked"
        || merge_state == "unstable"
        || pr.review_decision.as_deref().map(str::trim) == Some("review_required")
    {
        return WatchAssessment::NeedsUser("a review or repository requirement is outstanding");
    }
    if pr.auto_merge_enabled == Some(true) {
        return WatchAssessment::Waiting;
    }
    let pending = checks
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Pending)
        .count();
    if pending > 0 {
        return WatchAssessment::Waiting;
    }
    if mergeable == "mergeable" && merge_state == "clean" {
        return WatchAssessment::Ready;
    }
    WatchAssessment::Waiting
}

/// The instruction one fix turn runs. Scoped to a single cycle: the loop
/// lives in the sweep, not in the engine's context window.
pub(crate) fn fix_turn_instruction(reason: WatchReason, pr: &PullRequestDigest) -> String {
    let number = pr.number;
    let base = pr
        .base_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("the base branch");
    let instruction = match reason {
        WatchReason::FailingChecks => format!(
            "Pull request #{number} has failing checks. Inspect the latest failing CI logs \
             for the current head SHA, reproduce the cause when practical, make the smallest \
             safe fix in this workspace, run focused validation, commit, and push. Do not \
             merge, enable auto-merge, or change the pull request's draft or review state."
        ),
        WatchReason::Conflicts => format!(
            "Pull request #{number} has merge conflicts with {base}. Fetch and rebase onto \
             {base}, resolve every conflict in this workspace, run focused validation, commit \
             if needed, and push the updated head. Do not merge or enable auto-merge."
        ),
        WatchReason::Behind => format!(
            "Update pull request #{number} from {base}. Fetch the latest base branch, rebase \
             this workspace branch onto it, resolve any conflicts, run focused validation, \
             and push the updated head. Do not merge or enable auto-merge."
        ),
        WatchReason::ChangesRequested => format!(
            "Pull request #{number} has requested changes. Inspect the latest unresolved \
             review feedback, implement each actionable request in this workspace, run \
             focused validation, commit, push, and reply where context is useful. Do not \
             merge, enable auto-merge, or resolve review threads you did not address."
        ),
    };
    let mut lines = vec![instruction, String::new()];
    lines.push(format!(
        "Pull request: #{number}{}",
        pr.title
            .as_deref()
            .map(|title| format!(" - {title}"))
            .unwrap_or_default()
    ));
    if let Some(url) = pr.url.as_deref() {
        lines.push(format!("URL: {url}"));
    }
    if let Some(summary) = pr.checks_summary.as_deref() {
        lines.push(format!("Checks: {summary}"));
    }
    let active = pr
        .checks
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Fail)
        .collect::<Vec<_>>();
    if !active.is_empty() {
        lines.push("Failing checks:".to_owned());
        for check in active {
            let mut line = format!("- {}", check.name);
            if let Some(detail) = check.detail.as_deref() {
                line.push_str(&format!(": {detail}"));
            }
            if let Some(url) = check.url.as_deref() {
                line.push_str(&format!("\n  {url}"));
            }
            lines.push(line);
        }
    }
    lines.join("\n")
}

impl CodeRuntime {
    /// Start a durable watch on the workspace's open pull request.
    ///
    /// Forks a dedicated watch session in the same worktree. The session
    /// reuses the interactive session's engine and model when one exists and
    /// always runs `auto`: a watch that must stop for every command approval
    /// is a prompt in disguise.
    pub(crate) async fn start_watch(
        self: &Arc<Self>,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<CodeWatch, ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if let Some(existing) = latest_watch_for_workspace(&self.db, owner, workspace_id).await? {
            if !existing.state.is_terminal() {
                return Err(ServerError::conflict_kind(
                    "watch_exists",
                    "this workspace already has an active watch",
                ));
            }
        }
        let status = self.refresh_workspace_pr(owner, workspace_id).await?;
        let Some(pr) = status.pr else {
            return Err(ServerError::conflict_kind(
                "no_pull_request",
                "this workspace has no pull request to watch",
            ));
        };
        match assess(&pr) {
            WatchAssessment::Merged => {
                return Err(ServerError::conflict_kind(
                    "pr_not_open",
                    "the pull request has already merged",
                ));
            }
            WatchAssessment::Closed => {
                return Err(ServerError::conflict_kind(
                    "pr_not_open",
                    "the pull request is closed",
                ));
            }
            _ => {}
        }
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
        let (harness, model) = sessions
            .iter()
            .find(|session| session.kind == CodeSessionKind::Interactive)
            .map(|session| (session.harness_kind, session.model.clone()))
            .unwrap_or((HarnessKind::ClaudeCode, None));
        let session = self
            .create_session_of_kind(
                owner,
                workspace_id,
                CodeSessionKind::Watch,
                harness,
                NewSessionSettings {
                    permission_mode: CodePermissionMode::Auto,
                    model,
                    // A watch task inherits the engine and model of the
                    // session that spawned it, but not an effort a person
                    // picked for their own conversation.
                    reasoning_effort: None,
                },
            )
            .await?;
        let now = Utc::now();
        let watch = CodeWatch {
            id: CodeWatchId::new(),
            owner: owner.clone(),
            workspace_id,
            session_id: session.id,
            pr_number: pr.number,
            state: CodeWatchState::Watching,
            detail: None,
            last_fix_head: None,
            cycles: 0,
            created_at: now,
            updated_at: now,
        };
        insert_watch(&self.db, &watch).await?;
        self.ensure_watch_sweep();
        emit_workspace_digests(&self.db, &self.bus, owner, workspace_id).await;
        Ok(watch)
    }

    /// Stop the workspace's active watch and end its session.
    pub(crate) async fn stop_watch(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<CodeWatch, ServerError> {
        let Some(mut watch) = latest_watch_for_workspace(&self.db, owner, workspace_id).await?
        else {
            return Err(ServerError::not_found("this workspace has no watch"));
        };
        if watch.state.is_terminal() {
            return Err(ServerError::conflict_kind(
                "watch_not_active",
                "the watch has already finished",
            ));
        }
        finish_watch(
            self,
            &mut watch,
            CodeWatchState::Stopped,
            Some("stopped by the user".to_owned()),
        )
        .await?;
        Ok(watch)
    }

    /// The workspace's most recent watch, terminal or not.
    pub(crate) async fn latest_watch(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<Option<CodeWatch>, ServerError> {
        Ok(latest_watch_for_workspace(&self.db, owner, workspace_id).await?)
    }
}

/// One pass over every active watch. Failures on one watch never stop the
/// others.
pub(crate) async fn sweep_watches(runtime: &Arc<CodeRuntime>) {
    let watches = match list_active_watches_all_owners(&runtime.db).await {
        Ok(watches) => watches,
        Err(err) => {
            warn!(error = %err, "code-mode watch sweep could not list watches");
            return;
        }
    };
    for mut watch in watches {
        if let Err(err) = sweep_one(runtime, &mut watch).await {
            warn!(
                watch = %watch.id,
                workspace = %watch.workspace_id,
                error = %err.message(),
                "code-mode watch sweep failed for one watch"
            );
        }
    }
}

async fn sweep_one(runtime: &Arc<CodeRuntime>, watch: &mut CodeWatch) -> Result<(), ServerError> {
    let owner = watch.owner.clone();
    let Some(session) = get_session(&runtime.db, &owner, watch.session_id).await? else {
        return finish_watch(
            runtime.as_ref(),
            watch,
            CodeWatchState::Failed,
            Some("the watch session is gone".to_owned()),
        )
        .await;
    };
    match session.lifecycle {
        CodeSessionLifecycle::Ended => {
            return finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Failed,
                Some("the watch session ended".to_owned()),
            )
            .await;
        }
        CodeSessionLifecycle::Fenced => {
            return finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Failed,
                Some("the watch session is fenced; reap it and start a new watch".to_owned()),
            )
            .await;
        }
        // A fix turn (or its recovery) is still in flight; check back next
        // sweep rather than hammering the host meanwhile.
        CodeSessionLifecycle::Running => return Ok(()),
        CodeSessionLifecycle::Created | CodeSessionLifecycle::Idle => {}
    }
    let workspace = match runtime.get_workspace(&owner, watch.workspace_id).await {
        Ok(workspace) => workspace,
        Err(_) => {
            return finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Failed,
                Some("the workspace is gone".to_owned()),
            )
            .await;
        }
    };
    if workspace.status != CodeWorkspaceStatus::Active {
        return finish_watch(
            runtime.as_ref(),
            watch,
            CodeWatchState::Failed,
            Some(format!("the workspace is {}", workspace.status.as_str())),
        )
        .await;
    }
    // Another session's turn owns the worktree right now; wait. The turn lock
    // in the worker is what actually serializes the checkout (record 55);
    // skipping the cycle here avoids waking a harness that would only queue.
    let sessions = list_sessions_for_workspace(&runtime.db, &owner, watch.workspace_id).await?;
    if sessions
        .iter()
        .any(|other| other.lifecycle == CodeSessionLifecycle::Running)
    {
        return Ok(());
    }
    let status = runtime
        .refresh_workspace_pr(&owner, watch.workspace_id)
        .await?;
    let Some(pr) = status.pr else {
        return park_watch(
            runtime.as_ref(),
            watch,
            "the pull request digest is unavailable; is GitHub CLI signed in?",
        )
        .await;
    };
    if pr.number != watch.pr_number {
        return finish_watch(
            runtime.as_ref(),
            watch,
            CodeWatchState::Failed,
            Some(format!(
                "the workspace's pull request changed from #{} to #{}",
                watch.pr_number, pr.number
            )),
        )
        .await;
    }
    match assess(&pr) {
        WatchAssessment::Merged => {
            finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Done,
                Some("the pull request merged".to_owned()),
            )
            .await
        }
        WatchAssessment::Closed => {
            finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Done,
                Some("the pull request closed without merging".to_owned()),
            )
            .await
        }
        WatchAssessment::Ready => {
            let done = finish_watch(
                runtime.as_ref(),
                watch,
                CodeWatchState::Done,
                Some("the pull request is ready; merging is yours".to_owned()),
            )
            .await;
            let _ = apply_attention(
                &runtime.db,
                &runtime.bus,
                &owner,
                watch.session_id,
                Attention::needs_you(
                    "the pull request is ready to merge",
                    AttentionSource::Structured,
                ),
                false,
            )
            .await;
            done
        }
        WatchAssessment::NeedsUser(reason) => park_watch(runtime.as_ref(), watch, reason).await,
        WatchAssessment::Waiting => {
            if watch.state != CodeWatchState::Watching || watch.detail.is_some() {
                watch.state = CodeWatchState::Watching;
                watch.detail = None;
                persist_watch(runtime.as_ref(), watch).await?;
            }
            Ok(())
        }
        WatchAssessment::Actionable(reason) => {
            let same_head = watch.last_fix_head.is_some()
                && watch.last_fix_head.as_deref() == pr.head_sha.as_deref();
            if same_head {
                // The previous fix turn ran against this same head and the
                // condition is still present: repeating it would loop.
                return park_watch(
                    runtime.as_ref(),
                    watch,
                    "a fix attempt did not resolve the problem",
                )
                .await;
            }
            watch.state = CodeWatchState::Fixing;
            watch.detail = Some(format!("fixing {}", reason.describe()));
            watch.last_fix_head = pr.head_sha.clone();
            watch.cycles = watch.cycles.saturating_add(1);
            persist_watch(runtime.as_ref(), watch).await?;
            let instruction = fix_turn_instruction(reason, &pr);
            let session_id = watch.session_id;
            let task_runtime = Arc::clone(runtime);
            let task_owner = owner.clone();
            // Detached: a fix turn can run for minutes and the sweep must
            // keep serving other watches. The next sweeps see the session
            // Running and stand by.
            tokio::spawn(async move {
                if let Err(err) = task_runtime
                    .submit_turn(&task_owner, session_id, instruction, None, None, Vec::new())
                    .await
                {
                    warn!(
                        session = %session_id,
                        error = %err.message(),
                        "code-mode watch fix turn failed to submit"
                    );
                }
            });
            Ok(())
        }
    }
}

/// Park a watch as blocked with a reason, surfacing `NeedsYou` once.
async fn park_watch(
    runtime: &CodeRuntime,
    watch: &mut CodeWatch,
    reason: &str,
) -> Result<(), ServerError> {
    if watch.state == CodeWatchState::Blocked && watch.detail.as_deref() == Some(reason) {
        return Ok(());
    }
    watch.state = CodeWatchState::Blocked;
    watch.detail = Some(reason.to_owned());
    persist_watch(runtime, watch).await?;
    let _ = apply_attention(
        &runtime.db,
        &runtime.bus,
        &watch.owner,
        watch.session_id,
        Attention::needs_you(reason, AttentionSource::Structured),
        false,
    )
    .await;
    Ok(())
}

/// Move a watch to a terminal state and end its session.
async fn finish_watch(
    runtime: &CodeRuntime,
    watch: &mut CodeWatch,
    state: CodeWatchState,
    detail: Option<String>,
) -> Result<(), ServerError> {
    watch.state = state;
    watch.detail = detail;
    persist_watch(runtime, watch).await?;
    let owner = watch.owner.clone();
    runtime.end_session_row(&owner, watch.session_id).await?;
    emit_workspace_digests(&runtime.db, &runtime.bus, &watch.owner, watch.workspace_id).await;
    Ok(())
}

async fn persist_watch(runtime: &CodeRuntime, watch: &mut CodeWatch) -> Result<(), ServerError> {
    watch.updated_at = Utc::now();
    save_watch(&runtime.db, watch).await?;
    emit_workspace_digests(&runtime.db, &runtime.bus, &watch.owner, watch.workspace_id).await;
    Ok(())
}

/// Abort the watch sweep when the runtime is dropped.
///
/// The loop holds a [`Weak`] runtime handle: an `Arc` would keep the runtime
/// alive from its own field and the guard's `Drop` could never run.
pub(crate) struct WatchSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl WatchSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(WATCH_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_watches(&runtime).await;
            }
        });
        Self(Some(handle))
    }
}

impl Drop for WatchSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{PullRequestCheck, PullRequestCheckBucket};

    fn base_pr() -> PullRequestDigest {
        PullRequestDigest {
            number: 12,
            url: Some("https://github.com/example/demo/pull/12".to_owned()),
            state: "open".to_owned(),
            title: Some("demo".to_owned()),
            checks_summary: None,
            checks: None,
            draft: Some(false),
            merged: Some(false),
            review_decision: None,
            mergeable: None,
            merge_state_status: None,
            head_branch: Some("feature".to_owned()),
            base_branch: Some("main".to_owned()),
            head_sha: Some("abc123".to_owned()),
            auto_merge_enabled: Some(false),
            in_merge_queue: None,
        }
    }

    fn check(bucket: PullRequestCheckBucket) -> PullRequestCheck {
        PullRequestCheck {
            name: "ci".to_owned(),
            bucket,
            detail: None,
            url: None,
        }
    }

    #[test]
    fn merged_and_closed_are_terminal() {
        let mut pr = base_pr();
        pr.merged = Some(true);
        assert_eq!(assess(&pr), WatchAssessment::Merged);
        let mut pr = base_pr();
        pr.state = "closed".to_owned();
        assert_eq!(assess(&pr), WatchAssessment::Closed);
    }

    #[test]
    fn conflicts_outrank_failing_checks() {
        let mut pr = base_pr();
        pr.mergeable = Some("conflicting".to_owned());
        pr.checks = Some(vec![check(PullRequestCheckBucket::Fail)]);
        assert_eq!(
            assess(&pr),
            WatchAssessment::Actionable(WatchReason::Conflicts)
        );
    }

    #[test]
    fn failing_checks_are_actionable_while_pending_wait() {
        let mut pr = base_pr();
        pr.checks = Some(vec![
            check(PullRequestCheckBucket::Fail),
            check(PullRequestCheckBucket::Pending),
        ]);
        assert_eq!(
            assess(&pr),
            WatchAssessment::Actionable(WatchReason::FailingChecks)
        );
        let mut pr = base_pr();
        pr.checks = Some(vec![check(PullRequestCheckBucket::Pending)]);
        assert_eq!(assess(&pr), WatchAssessment::Waiting);
    }

    #[test]
    fn drafts_and_required_reviews_need_the_user() {
        let mut pr = base_pr();
        pr.draft = Some(true);
        assert!(matches!(assess(&pr), WatchAssessment::NeedsUser(_)));
        let mut pr = base_pr();
        pr.review_decision = Some("review_required".to_owned());
        assert!(matches!(assess(&pr), WatchAssessment::NeedsUser(_)));
    }

    #[test]
    fn clean_and_mergeable_is_ready() {
        let mut pr = base_pr();
        pr.mergeable = Some("mergeable".to_owned());
        pr.merge_state_status = Some("clean".to_owned());
        assert_eq!(assess(&pr), WatchAssessment::Ready);
    }

    #[test]
    fn behind_is_actionable_and_queue_waits() {
        let mut pr = base_pr();
        pr.merge_state_status = Some("behind".to_owned());
        assert_eq!(
            assess(&pr),
            WatchAssessment::Actionable(WatchReason::Behind)
        );
        let mut pr = base_pr();
        pr.in_merge_queue = Some(true);
        assert_eq!(assess(&pr), WatchAssessment::Waiting);
    }

    #[test]
    fn fix_instruction_never_asks_to_merge() {
        let mut pr = base_pr();
        pr.checks = Some(vec![check(PullRequestCheckBucket::Fail)]);
        for reason in [
            WatchReason::FailingChecks,
            WatchReason::Conflicts,
            WatchReason::Behind,
            WatchReason::ChangesRequested,
        ] {
            let text = fix_turn_instruction(reason, &pr).to_ascii_lowercase();
            assert!(text.contains("do not merge"), "{reason:?}");
            assert!(!text.contains("enable auto-merge once"), "{reason:?}");
        }
    }
}
