//! The trigger sweep: turn pull-request facts into claimed fires.
//!
//! A trigger is a durable row and the sweep is what drives it
//! ([record 60](../../../../docs/decisions/0060-triggers-are-durable-rules-on-pull-request-facts.md)).
//! Every tick reads the work list from the table rather than subscribing: the
//! event bus is a lossy `broadcast`, and a fact this misses is a message an
//! agent never gets.
//!
//! A fire is claimed only once its delivery is settled. The claim is
//! fingerprinted against `head_sha`, so claiming on a tick that could not
//! deliver would burn the edge and the message would never arrive: the
//! condition staying true is not a second edge. Holding the claim back is
//! what makes "retry on a later tick" work without a row to mark drained.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use tidebreak_core::db::code::{
    get_open_turn, get_repo, get_session, insert_trigger_fire, latest_turn,
    list_active_watches_all_owners, list_enabled_triggers_all_owners, list_sessions_for_workspace,
};
use tidebreak_core::{
    classify_trigger_condition, Attention, AttentionSource, CapLevel, CodeEvent, CodeSession,
    CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeTrigger, CodeTriggerAction,
    CodeTriggerCondition, CodeTriggerFire, CodeTurnId, CodeWorkspaceStatus, HarnessNoticeLevel,
    OwnerId, PullRequestCheck, PullRequestDigest, RepoId, WorkspaceId,
};
use tracing::{debug, warn};

use super::attention::apply_attention;
use super::delivery::{query_pull_requests, repository_target_from_local};
use super::runtime::CodeRuntime;
use super::session_worker::journal_event;
use crate::error::ServerError;
use crate::routes::code::types::{
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestSummary, CodeDeliveryWorkspaceLink,
};

/// How often the trigger sweep walks enabled triggers.
///
/// Offset from [`super::watch::WATCH_SWEEP_INTERVAL`] rather than equal to it:
/// both sweeps read GitHub, and landing them on the same tick would double the
/// burst a rate limit sees.
pub(crate) const TRIGGER_SWEEP_INTERVAL: Duration = Duration::from_secs(53);

/// One pass over every enabled trigger. A failure on one repository never
/// stops the others.
pub(crate) async fn sweep_triggers(runtime: &Arc<CodeRuntime>) {
    let triggers = match list_enabled_triggers_all_owners(&runtime.db).await {
        Ok(triggers) => triggers,
        Err(err) => {
            warn!(error = %err, "code-mode trigger sweep could not list triggers");
            return;
        }
    };
    if triggers.is_empty() {
        return;
    }

    // A watch is already acting on the same facts. Delivering beside it would
    // put two drivers on one loop, so its workspaces are skipped wholesale.
    let watched = match list_active_watches_all_owners(&runtime.db).await {
        Ok(watches) => watches
            .into_iter()
            .map(|watch| watch.workspace_id)
            .collect::<HashSet<_>>(),
        Err(err) => {
            // Firing beside an unknown watch is the failure this guard exists
            // to prevent, so a sweep that cannot read them does nothing.
            warn!(error = %err, "code-mode trigger sweep could not list watches");
            return;
        }
    };

    // Group by repository so each one is queried once per tick no matter how
    // many conditions the user armed on it.
    let mut by_repo: HashMap<(OwnerId, RepoId), Vec<CodeTrigger>> = HashMap::new();
    for trigger in triggers {
        by_repo
            .entry((trigger.owner.clone(), trigger.repo_id))
            .or_default()
            .push(trigger);
    }

    for ((owner, repo_id), triggers) in by_repo {
        if let Err(err) = sweep_repo(runtime, &owner, repo_id, &triggers, &watched).await {
            warn!(
                repo = %repo_id,
                error = %err.message(),
                "code-mode trigger sweep failed for one repository"
            );
        }
    }
}

/// One repository: read its pull requests in bulk, then claim what matches.
async fn sweep_repo(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    repo_id: RepoId,
    triggers: &[CodeTrigger],
    watched: &HashSet<WorkspaceId>,
) -> Result<(), ServerError> {
    let Some(repo) = get_repo(&runtime.db, owner, repo_id).await? else {
        return Ok(());
    };
    if repo.removed_at.is_some() {
        return Ok(());
    }
    // A repository with no GitHub origin has no facts to sweep. That is a
    // registration the user made, not a failure worth logging every tick.
    let Ok(target) = repository_target_from_local(&repo).await else {
        return Ok(());
    };

    // Bulk, behind the delivery list cache. Reading per workspace instead
    // would invalidate the digest cache and spawn one `gh` call per
    // workspace per tick.
    let page = query_pull_requests(
        runtime,
        owner,
        CodeDeliveryPullRequestQuery {
            repositories: vec![target],
            search: None,
            states: Vec::new(),
            review_states: Vec::new(),
            check_states: Vec::new(),
            authors: Vec::new(),
            attention_only: false,
            ready_only: false,
            // Triggers apply to workspaces that have a pull request, so an
            // unlinked one is out of scope before any condition is read.
            tidebreak_linked: Some(true),
            updated_after: None,
            cursor: None,
            limit: None,
            // Never set: a sweep is not a user refresh, and the whole point of
            // reading here is to ride the cache the delivery surface fills.
            refresh: false,
        },
    )
    .await?;

    for item in &page.items {
        claim_fires(runtime, owner, triggers, item, watched).await;
    }
    Ok(())
}

/// Claim and deliver one fire per matching trigger per linked workspace.
async fn claim_fires(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    triggers: &[CodeTrigger],
    item: &CodeDeliveryPullRequestSummary,
    watched: &HashSet<WorkspaceId>,
) {
    // Without a head SHA the fire cannot be fingerprinted, and a fire that
    // cannot be bounded would repeat every tick.
    let Some(head_sha) = item.head_sha.clone() else {
        return;
    };
    let digest = digest_from(item);
    let Some(condition) = classify_trigger_condition(&digest) else {
        return;
    };
    let workspaces = linked_workspaces(&item.workspace_links, watched);
    if workspaces.is_empty() {
        return;
    }
    for trigger in triggers.iter().filter(|t| t.condition == condition) {
        for workspace_id in &workspaces {
            if let Err(err) =
                fire_one(runtime, owner, trigger, *workspace_id, &digest, &head_sha).await
            {
                warn!(
                    trigger = %trigger.id,
                    workspace = %workspace_id,
                    error = %err.message(),
                    "code-mode trigger sweep could not deliver a fire"
                );
            }
        }
    }
}

/// How this fire reaches the agent.
#[derive(Debug, Clone, Copy)]
enum Delivery {
    /// Interrupt the turn already running. Only where the harness declares it.
    Steer {
        session_id: CodeSessionId,
        turn_id: CodeTurnId,
    },
    /// Submit a turn. The workspace is quiet, so nothing is contended.
    Turn { session_id: CodeSessionId },
    /// Raise attention and leave the session alone.
    Notify { session_id: CodeSessionId },
}

impl Delivery {
    fn session_id(self) -> CodeSessionId {
        match self {
            Self::Steer { session_id, .. }
            | Self::Turn { session_id }
            | Self::Notify { session_id } => session_id,
        }
    }
}

/// Settle delivery, then claim, then act.
async fn fire_one(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    trigger: &CodeTrigger,
    workspace_id: WorkspaceId,
    digest: &PullRequestDigest,
    head_sha: &str,
) -> Result<(), ServerError> {
    // Settled first, deliberately. See the module note: the claim is the
    // commitment to act, not the observation of an edge.
    let Some(delivery) = plan_delivery(runtime, owner, workspace_id, trigger.action).await? else {
        return Ok(());
    };

    let fire = CodeTriggerFire {
        trigger_id: trigger.id,
        owner: owner.clone(),
        workspace_id,
        pr_number: digest.number,
        head_sha: head_sha.to_owned(),
        fired_at: Utc::now(),
    };
    // Already fired for this head. The condition still being true is not a
    // second edge.
    if !insert_trigger_fire(&runtime.db, &fire).await? {
        return Ok(());
    }
    debug!(
        trigger = %trigger.id,
        workspace = %workspace_id,
        pr = digest.number,
        condition = ?trigger.condition,
        delivery = ?delivery,
        "code-mode trigger fired"
    );

    let session_id = delivery.session_id();
    let message = trigger_message(trigger.condition, digest);
    match delivery {
        Delivery::Steer { turn_id, .. } => {
            runtime.steer(owner, session_id, turn_id, message).await?;
        }
        Delivery::Turn { .. } => {
            runtime
                .submit_turn(owner, session_id, message, None, None, Vec::new())
                .await?;
        }
        Delivery::Notify { .. } => {
            let _ = apply_attention(
                &runtime.db,
                &runtime.bus,
                owner,
                session_id,
                Attention::needs_you(
                    describe_condition(trigger.condition, digest.number),
                    AttentionSource::Structured,
                ),
                false,
            )
            .await;
        }
    }
    note_fire(runtime, owner, session_id, trigger, digest).await;
    Ok(())
}

/// Which session a fire reaches and how, or `None` to try again next tick.
async fn plan_delivery(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
    action: CodeTriggerAction,
) -> Result<Option<Delivery>, ServerError> {
    let sessions = list_sessions_for_workspace(&runtime.db, owner, workspace_id).await?;
    let Some(target) = most_recently_active(runtime, owner, &sessions).await? else {
        return Ok(None);
    };
    if action == CodeTriggerAction::Notify {
        // Attention does not touch the worktree, so a busy workspace is fine.
        return Ok(Some(Delivery::Notify {
            session_id: target.id,
        }));
    }

    // Another session's turn owns the checkout. The turn lock in the worker is
    // what actually serializes it (record 55); standing down here keeps the
    // fire unclaimed so a later tick delivers it.
    let busy = sessions
        .iter()
        .any(|session| session.lifecycle == CodeSessionLifecycle::Running);
    if !busy {
        return Ok(Some(Delivery::Turn {
            session_id: target.id,
        }));
    }

    // Busy: steering is the only way in, and only where the engine takes it.
    if target.lifecycle != CodeSessionLifecycle::Running {
        return Ok(None);
    }
    let adapter = runtime.adapter(target.harness_kind)?;
    let probe = runtime.probe(adapter.as_ref()).await;
    if adapter.capabilities(&probe).mid_turn_steering != CapLevel::Supported {
        return Ok(None);
    }
    let Some(turn) = get_open_turn(&runtime.db, owner, target.id).await? else {
        return Ok(None);
    };
    Ok(Some(Delivery::Steer {
        session_id: target.id,
        turn_id: turn.id,
    }))
}

/// The workspace's most recently active interactive session.
///
/// Watch sessions are never a target: a watch is already acting on the same
/// facts, and delivering to it would put two drivers on one loop. Recency is
/// the last turn a session ran, falling back to when it was created, because
/// a session row carries no activity timestamp of its own.
async fn most_recently_active(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    sessions: &[CodeSession],
) -> Result<Option<CodeSession>, ServerError> {
    let mut best: Option<(chrono::DateTime<chrono::Utc>, CodeSession)> = None;
    for session in sessions {
        if session.kind != CodeSessionKind::Interactive {
            continue;
        }
        if matches!(
            session.lifecycle,
            CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
        ) {
            continue;
        }
        let at = latest_turn(&runtime.db, owner, session.id)
            .await?
            .map_or(session.created_at, |turn| turn.started_at);
        if best.as_ref().is_none_or(|(best_at, _)| at > *best_at) {
            best = Some((at, session.clone()));
        }
    }
    Ok(best.map(|(_, session)| session))
}

/// The message a fire delivers.
///
/// It names the trigger that fired and the fact that fired it, so the agent
/// never has to infer why it was interrupted, and it never reads as the user
/// speaking. Content discipline follows `fix_turn_instruction`: check names,
/// buckets, and URLs, never raw logs.
fn trigger_message(condition: CodeTriggerCondition, pr: &PullRequestDigest) -> String {
    let number = pr.number;
    let mut lines = vec![
        format!(
            "Tidebreak trigger: {}. Nobody typed this — a trigger you armed on \
             this repository fired because the fact below changed.",
            describe_condition(condition, number)
        ),
        String::new(),
    ];
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
    let failing = pr
        .checks
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Fail)
        .collect::<Vec<_>>();
    if !failing.is_empty() {
        lines.push("Failing checks:".to_owned());
        for check in failing {
            let mut line = format!("- {}", check.name);
            if let Some(url) = check.url.as_deref() {
                line.push_str(&format!(" ({url})"));
            }
            lines.push(line);
        }
    }
    lines.push(String::new());
    lines.push(
        "Decide whether to act on this now. Do not merge, enable auto-merge, or \
         change the pull request's draft or review state — those stay the user's."
            .to_owned(),
    );
    lines.join("\n")
}

/// One phrase naming the fact, shared by the message and the notification.
fn describe_condition(condition: CodeTriggerCondition, number: u64) -> String {
    match condition {
        CodeTriggerCondition::ChecksFailed => format!("checks failed on #{number}"),
        CodeTriggerCondition::Conflicts => format!("#{number} has merge conflicts"),
        CodeTriggerCondition::ChangesRequested => format!("changes requested on #{number}"),
        CodeTriggerCondition::ReviewRequired => format!("#{number} is waiting on review"),
        CodeTriggerCondition::Behind => format!("#{number} is behind its base"),
        CodeTriggerCondition::ReadyToMerge => format!("#{number} is ready to merge"),
        CodeTriggerCondition::Merged => format!("#{number} merged"),
        CodeTriggerCondition::Closed => format!("#{number} closed without merging"),
    }
}

/// Journal the fire so the transcript says why the agent got something.
///
/// A `HarnessNotice` rather than a variant of its own, following
/// `note_permission_mode`: the journal already uses it for "something moved on
/// this session" lines that no harness produced.
async fn note_fire(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    session_id: CodeSessionId,
    trigger: &CodeTrigger,
    digest: &PullRequestDigest,
) {
    let Ok(Some(session)) = get_session(&runtime.db, owner, session_id).await else {
        return;
    };
    let _ = journal_event(
        &runtime.db,
        &runtime.bus,
        owner,
        session_id,
        session.spawn_epoch,
        CodeEvent::HarnessNotice {
            level: HarnessNoticeLevel::Info,
            message: format!(
                "trigger {} fired: {}",
                trigger.id,
                describe_condition(trigger.condition, digest.number)
            ),
        },
    )
    .await;
}

/// Active workspaces this pull request is exactly on, minus watched ones.
fn linked_workspaces(
    links: &[CodeDeliveryWorkspaceLink],
    watched: &HashSet<WorkspaceId>,
) -> Vec<WorkspaceId> {
    links
        .iter()
        // A fuzzy link is a branch-name guess. Firing on one would wake an
        // agent about someone else's pull request.
        .filter(|link| link.exact)
        .filter(|link| link.status == CodeWorkspaceStatus::Active)
        .filter(|link| !watched.contains(&link.workspace_id))
        .map(|link| link.workspace_id)
        .collect()
}

/// The bulk summary read as the digest the classifier is written against.
///
/// Both paths lowercase their host tokens already — `normalized_optional` here
/// and `lower_token` in `gh.rs` — so the tokens pass straight through.
fn digest_from(item: &CodeDeliveryPullRequestSummary) -> PullRequestDigest {
    PullRequestDigest {
        number: item.number,
        url: Some(item.url.clone()),
        state: item.state.clone(),
        title: Some(item.title.clone()),
        checks_summary: None,
        checks: Some(
            item.checks
                .iter()
                .map(|check| PullRequestCheck {
                    name: check.name.clone(),
                    bucket: check.bucket,
                    detail: check.detail.clone(),
                    url: check.url.clone(),
                })
                .collect(),
        ),
        draft: Some(item.draft),
        // `state` alone cannot separate merged from closed on every host
        // response, which is why the summary carries `merged_at`.
        merged: Some(item.merged_at.is_some()),
        review_decision: item.review_decision.clone(),
        mergeable: item.mergeable.clone(),
        merge_state_status: item.merge_state_status.clone(),
        head_branch: Some(item.head_branch.clone()),
        base_branch: Some(item.base_branch.clone()),
        head_sha: item.head_sha.clone(),
        auto_merge_enabled: Some(item.auto_merge_enabled),
        in_merge_queue: None,
    }
}

/// Abort the trigger sweep when the runtime is dropped.
///
/// The loop holds a [`Weak`] runtime handle: an `Arc` would keep the runtime
/// alive from its own field and the guard's `Drop` could never run.
pub(crate) struct TriggerSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl TriggerSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TRIGGER_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_triggers(&runtime).await;
            }
        });
        Self(Some(handle))
    }
}

impl Drop for TriggerSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{CodeTriggerCondition, PullRequestCheckBucket};

    use crate::routes::code::types::{CodeDeliveryCheck, CodeGitHubRepositoryRef};

    fn repository() -> CodeGitHubRepositoryRef {
        CodeGitHubRepositoryRef {
            host: "github.com".to_owned(),
            owner: "example".to_owned(),
            name: "demo".to_owned(),
            name_with_owner: "example/demo".to_owned(),
            url: "https://github.com/example/demo".to_owned(),
            default_branch: Some("main".to_owned()),
            tidebreak_repo_id: None,
        }
    }

    fn summary() -> CodeDeliveryPullRequestSummary {
        CodeDeliveryPullRequestSummary {
            id: "PR_1".to_owned(),
            repository: repository(),
            number: 12,
            url: "https://github.com/example/demo/pull/12".to_owned(),
            title: "demo".to_owned(),
            state: "open".to_owned(),
            draft: false,
            author: Some("someone".to_owned()),
            author_avatar_url: None,
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            head_sha: Some("abc123".to_owned()),
            review_decision: None,
            mergeable: Some("mergeable".to_owned()),
            merge_state_status: Some("clean".to_owned()),
            auto_merge_enabled: false,
            checks: Vec::new(),
            attention_reasons: Vec::new(),
            ready_to_merge: true,
            workspace_links: Vec::new(),
            labels: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            merged_at: None,
            closed_at: None,
        }
    }

    fn link(exact: bool, status: CodeWorkspaceStatus) -> CodeDeliveryWorkspaceLink {
        CodeDeliveryWorkspaceLink {
            workspace_id: WorkspaceId::new(),
            repo_id: RepoId::new(),
            title: "work".to_owned(),
            branch_name: "feature".to_owned(),
            status,
            exact,
        }
    }

    /// The conversion is the whole reason the bulk read can drive the
    /// classifier. A dropped or mis-cased field would silently classify as
    /// something else, which is a wrong message to a real agent.
    #[test]
    fn the_bulk_summary_classifies_as_the_digest_would() {
        let mut item = summary();
        item.checks = vec![CodeDeliveryCheck {
            name: "test".to_owned(),
            bucket: PullRequestCheckBucket::Fail,
            detail: Some("failing".to_owned()),
            url: Some("https://github.com/example/demo/runs/1".to_owned()),
            workflow_run_id: Some(1),
        }];

        let digest = digest_from(&item);
        assert_eq!(digest.number, 12);
        assert_eq!(digest.head_sha.as_deref(), Some("abc123"));
        assert_eq!(digest.mergeable.as_deref(), Some("mergeable"));
        assert_eq!(digest.merge_state_status.as_deref(), Some("clean"));
        assert_eq!(digest.draft, Some(false));
        assert_eq!(digest.checks.as_deref().map(<[_]>::len), Some(1));
        assert_eq!(
            classify_trigger_condition(&digest),
            Some(CodeTriggerCondition::ChecksFailed)
        );
    }

    /// `state` alone cannot separate merged from closed on every host
    /// response, so the conversion reads `merged_at` rather than trusting it.
    #[test]
    fn a_merged_pull_request_reads_as_merged_not_closed() {
        let mut item = summary();
        item.state = "closed".to_owned();
        item.merged_at = Some(Utc::now());

        assert_eq!(
            classify_trigger_condition(&digest_from(&item)),
            Some(CodeTriggerCondition::Merged)
        );

        let mut closed = summary();
        closed.state = "closed".to_owned();
        assert_eq!(
            classify_trigger_condition(&digest_from(&closed)),
            Some(CodeTriggerCondition::Closed)
        );
    }

    #[test]
    fn only_exact_active_unwatched_workspaces_are_targets() {
        let exact_active = link(true, CodeWorkspaceStatus::Active);
        let fuzzy = link(false, CodeWorkspaceStatus::Active);
        let archived = link(true, CodeWorkspaceStatus::Archived);
        let watched_link = link(true, CodeWorkspaceStatus::Active);

        let watched = HashSet::from([watched_link.workspace_id]);
        let links = vec![exact_active.clone(), fuzzy, archived, watched_link.clone()];

        let targets = linked_workspaces(&links, &watched);
        assert_eq!(targets, vec![exact_active.workspace_id]);
    }

    /// A fuzzy link is a branch-name guess. Firing on one would wake an agent
    /// about somebody else's pull request.
    #[test]
    fn a_fuzzy_link_alone_produces_no_target() {
        let links = vec![link(false, CodeWorkspaceStatus::Active)];
        assert!(linked_workspaces(&links, &HashSet::new()).is_empty());
    }
}
