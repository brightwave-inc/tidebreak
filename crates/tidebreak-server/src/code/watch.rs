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
    list_pull_request_facts_for_repo, list_sessions_for_workspace, save_watch,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeSessionKind, CodeSessionLifecycle, CodeWatch,
    CodeWatchId, CodeWatchState, CodeWorkspaceStatus, HarnessKind, OwnerId, PermissionMode,
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
    // GitHub reports the merge state as blocked while required checks run
    // (decision 66): running checks are a wait, not a park.
    let pending = checks
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Pending)
        .count();
    if pending > 0 {
        return WatchAssessment::Waiting;
    }
    if pr.review_decision.as_deref().map(str::trim) == Some("review_required") {
        return WatchAssessment::NeedsUser("the pull request needs a review approval");
    }
    if merge_state == "blocked" || merge_state == "unstable" {
        return WatchAssessment::NeedsUser("a repository requirement is outstanding");
    }
    if pr.auto_merge_enabled == Some(true) {
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
                    permission_mode: PermissionMode::Auto,
                    model,
                    // A watch task inherits the engine and model of the
                    // session that spawned it, but not an effort a person
                    // picked for their own conversation.
                    reasoning_effort: None,
                    // Nor the premium. Fast mode is a spend choice made for a
                    // conversation someone is watching, and a watch task runs
                    // unattended where nobody is waiting on the tokens.
                    fast_mode: false,
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
        // A running fix turn no longer skips the sweep: the state read below
        // still happens, and only the transitions that need an idle worktree
        // hold (decision 66).
        CodeSessionLifecycle::Running
        | CodeSessionLifecycle::Created
        | CodeSessionLifecycle::Idle => {}
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
    // A turn in flight — the watch's own fix turn or another session's —
    // holds every transition below: submitting a fix turn would queue on the
    // worktree the turn owns (record 55), and parking or finishing mid-turn
    // would judge a head the turn is about to move. The state read still
    // runs (decision 66): the pull requests being actively fixed are exactly
    // the ones whose digest the reader is watching.
    let sessions = list_sessions_for_workspace(&runtime.db, &owner, watch.workspace_id).await?;
    let turn_in_flight = sessions
        .iter()
        .any(|other| other.lifecycle == CodeSessionLifecycle::Running);
    // The store answers first (decision 66): the reconcile sweep's one list
    // read per repository keeps the live tier fresh, and write-through keeps
    // the workspace column equal to it. Only a missing or stale tier pays a
    // host read here — which itself lands back on the store for every other
    // consumer.
    let pr = match fresh_stored_digest(runtime, &owner, &workspace).await {
        Some(digest) => Some(digest),
        None => {
            runtime
                .refresh_workspace_pr(&owner, watch.workspace_id)
                .await?
                .pr
        }
    };
    if turn_in_flight {
        return Ok(());
    }
    let Some(pr) = pr else {
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
            let settled_attention = settled_watch_block_attention(watch, &session.attention);
            if watch.state != CodeWatchState::Watching || watch.detail.is_some() {
                watch.state = CodeWatchState::Watching;
                watch.detail = None;
                persist_watch(runtime.as_ref(), watch).await?;
            }
            if let Some(attention) = settled_attention {
                let _ = apply_attention(
                    &runtime.db,
                    &runtime.bus,
                    &owner,
                    watch.session_id,
                    attention,
                    false,
                )
                .await;
            }
            Ok(())
        }
        WatchAssessment::Actionable(reason) => {
            if reason == WatchReason::Behind {
                // A stacked child is behind *because of its parent*: rebasing
                // onto a branch that moves with every parent push never
                // settles (decision 62). Park until the parent lands.
                if let Some(parent) =
                    stacked_parent_number(runtime.as_ref(), &owner, watch.workspace_id, &pr).await
                {
                    return park_watch(
                        runtime.as_ref(),
                        watch,
                        &format!("waiting on its parent pull request #{parent}"),
                    )
                    .await;
                }
            }
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
            let settled_attention = settled_watch_block_attention(watch, &session.attention);
            watch.state = CodeWatchState::Fixing;
            watch.detail = Some(format!("fixing {}", reason.describe()));
            watch.last_fix_head = pr.head_sha.clone();
            watch.cycles = watch.cycles.saturating_add(1);
            persist_watch(runtime.as_ref(), watch).await?;
            if let Some(attention) = settled_attention {
                let _ = apply_attention(
                    &runtime.db,
                    &runtime.bus,
                    &owner,
                    watch.session_id,
                    attention,
                    false,
                )
                .await;
            }
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

/// The workspace's stored pull-request digest, when the fact row behind it
/// confirms the live tier is fresh (decision 66). `None` sends the caller to
/// a host read: no stored digest, no joinable URL, no fact row, or a tier
/// older than the reconcile cadence promises.
async fn fresh_stored_digest(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace: &tidebreak_core::CodeWorkspace,
) -> Option<PullRequestDigest> {
    let stored = workspace.pr.as_ref()?;
    let url = stored.url.as_deref()?;
    let (host, repo_owner, repo_name, number) =
        super::pr_facts::pull_request_identity_from_url(url)?;
    let fact = tidebreak_core::db::code::get_pull_request_fact(
        &runtime.db,
        owner,
        &host,
        &repo_owner,
        &repo_name,
        number,
    )
    .await
    .ok()??;
    fresh_workspace_digest(stored, &fact, Utc::now())
}

/// Return the workspace's write-through digest when the fact row confirms
/// that its live tier is fresh.
///
/// The hot refresher can observe a merge before the slower reconcile pass
/// updates the fact snapshot. Rebuilding from that snapshot would reopen the
/// pull request for the watch until the next reconcile tick.
fn fresh_workspace_digest(
    stored: &PullRequestDigest,
    fact: &tidebreak_core::CodePullRequestFact,
    now: chrono::DateTime<Utc>,
) -> Option<PullRequestDigest> {
    let live = fact.live.as_ref()?;
    super::reconcile::live_tier_is_fresh(live, now).then(|| stored.clone())
}

/// Clear only the `NeedsYou` marker that this watch's previous blocked state
/// created. Approval and engine-failure attention belongs to the session and
/// must remain visible.
fn settled_watch_block_attention(watch: &CodeWatch, current: &Attention) -> Option<Attention> {
    if watch.state != CodeWatchState::Blocked {
        return None;
    }
    let reason = watch.detail.as_deref()?;
    match &current.state {
        AttentionState::NeedsYou {
            prompt,
            source: AttentionSource::Structured,
        } if prompt == reason => Some(Attention::new(
            AttentionState::Idle,
            AttentionSource::Lifecycle,
        )),
        _ => None,
    }
}

/// The open pull request this workspace's pull request is stacked on, when
/// the durable fact set knows one (decision 62). `None` on any missing link
/// — no base branch, unresolved origin, or a store failure — so the watch
/// behaves exactly as before when the answer is unknown.
pub(crate) async fn stacked_parent_number(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
    pr: &PullRequestDigest,
) -> Option<u64> {
    let base_branch = pr.base_branch.as_deref()?;
    let workspace = runtime.get_workspace(owner, workspace_id).await.ok()?;
    let repo = runtime.get_repo(owner, workspace.repo_id).await.ok()?;
    let (host, repo_owner, repo_name) =
        match (&repo.origin_host, &repo.origin_owner, &repo.origin_name) {
            (Some(host), Some(repo_owner), Some(repo_name)) => {
                (host.clone(), repo_owner.clone(), repo_name.clone())
            }
            _ => {
                let target = super::delivery::repository_target_from_local(&repo)
                    .await
                    .ok()?;
                (target.host, target.owner, target.name)
            }
        };
    let facts =
        list_pull_request_facts_for_repo(&runtime.db, owner, &host, &repo_owner, &repo_name)
            .await
            .ok()?;
    let parents = super::reconcile::stack_parents_by_head(&facts);
    parents
        .get(base_branch)
        .copied()
        .filter(|parent| *parent != pr.number)
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
    use tidebreak_core::{
        CodePullRequestFact, CodePullRequestId, CodePullRequestLiveState, CodePullRequestState,
        PullRequestCheck, PullRequestCheckBucket,
    };

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
    fn fresh_workspace_digest_keeps_a_newer_terminal_state() {
        let now = Utc::now();
        let mut stored = base_pr();
        stored.state = "merged".to_owned();
        stored.merged = Some(true);
        stored.auto_merge_enabled = Some(false);
        let fact = CodePullRequestFact {
            id: CodePullRequestId::new(),
            owner: OwnerId::local(),
            host: "github.com".to_owned(),
            repo_owner: "example".to_owned(),
            repo_name: "demo".to_owned(),
            number: stored.number,
            url: stored.url.clone().unwrap(),
            title: stored.title.clone().unwrap(),
            state: CodePullRequestState::Open,
            draft: false,
            author: None,
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            head_sha: stored.head_sha.clone(),
            created_at: now,
            updated_at: now,
            merged_at: None,
            closed_at: None,
            first_seen_at: now,
            last_seen_at: now,
            live: Some(CodePullRequestLiveState::from_digest(&stored, now)),
        };

        assert_eq!(fresh_workspace_digest(&stored, &fact, now), Some(stored));
    }

    #[test]
    fn resuming_a_watch_clears_only_its_own_block_attention() {
        let now = Utc::now();
        let reason = "the pull request needs a review approval";
        let watch = CodeWatch {
            id: CodeWatchId::new(),
            owner: OwnerId::local(),
            workspace_id: WorkspaceId::new(),
            session_id: tidebreak_core::CodeSessionId::new(),
            pr_number: 12,
            state: CodeWatchState::Blocked,
            detail: Some(reason.to_owned()),
            last_fix_head: None,
            cycles: 0,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            settled_watch_block_attention(
                &watch,
                &Attention::needs_you(reason, AttentionSource::Structured),
            ),
            Some(Attention::new(
                AttentionState::Idle,
                AttentionSource::Lifecycle,
            ))
        );
        assert_eq!(
            settled_watch_block_attention(
                &watch,
                &Attention::needs_you("an approval is waiting", AttentionSource::Structured),
            ),
            None
        );
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
    fn running_checks_wait_even_while_the_merge_state_is_blocked() {
        // The decision-66 screenshot: GitHub says blocked whenever required
        // checks are still running, so blocked plus a required review while
        // checks run is a wait, not a park.
        let mut pr = base_pr();
        pr.merge_state_status = Some("blocked".to_owned());
        pr.review_decision = Some("review_required".to_owned());
        pr.checks = Some(vec![
            check(PullRequestCheckBucket::Pass),
            check(PullRequestCheckBucket::Pending),
        ]);
        assert_eq!(assess(&pr), WatchAssessment::Waiting);
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
