//! The trigger sweep: turn pull-request facts into claimed fires.
//!
//! A trigger is a durable row and the sweep is what drives it
//! ([record 60](../../../../docs/decisions/0060-triggers-are-durable-rules-on-pull-request-facts.md)).
//! Every tick reads the work list from the table rather than subscribing: the
//! event bus is a lossy `broadcast`, and a fact this misses is a message an
//! agent never gets.
//!
//! This module only *claims* fires. Acting on one is the delivery slice.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use tidebreak_core::db::code::{
    get_repo, insert_trigger_fire, list_active_watches_all_owners, list_enabled_triggers_all_owners,
};
use tidebreak_core::{
    classify_trigger_condition, CodeTrigger, CodeTriggerFire, CodeWorkspaceStatus, OwnerId,
    PullRequestCheck, PullRequestDigest, RepoId, WorkspaceId,
};
use tracing::{debug, warn};

use super::delivery::{query_pull_requests, repository_target_from_local};
use super::runtime::CodeRuntime;
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

/// Claim one fire per matching trigger per linked workspace.
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
    let Some(condition) = classify_trigger_condition(&digest_from(item)) else {
        return;
    };
    let workspaces = linked_workspaces(&item.workspace_links, watched);
    if workspaces.is_empty() {
        return;
    }
    for trigger in triggers.iter().filter(|t| t.condition == condition) {
        for workspace_id in &workspaces {
            let fire = CodeTriggerFire {
                trigger_id: trigger.id,
                owner: owner.clone(),
                workspace_id: *workspace_id,
                pr_number: item.number,
                head_sha: head_sha.clone(),
                fired_at: Utc::now(),
            };
            match insert_trigger_fire(&runtime.db, &fire).await {
                Ok(true) => debug!(
                    trigger = %trigger.id,
                    workspace = %workspace_id,
                    pr = item.number,
                    condition = ?condition,
                    "code-mode trigger fired"
                ),
                // Already fired for this head. The condition being still true
                // is not a second edge.
                Ok(false) => {}
                Err(err) => warn!(
                    trigger = %trigger.id,
                    workspace = %workspace_id,
                    error = %err,
                    "code-mode trigger sweep could not claim a fire"
                ),
            }
        }
    }
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
