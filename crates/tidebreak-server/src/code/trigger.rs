//! The trigger sweep: turn pull-request facts into claimed fires.
//!
//! A trigger is a durable row and the sweep is what drives it
//! ([record 60](../../../../docs/decisions/0060-triggers-are-durable-rules-on-pull-request-facts.md)).
//! Every tick reads the work list from the table rather than subscribing: the
//! event bus is a lossy `broadcast`, and a fact this misses is a message an
//! agent never gets.
//!
//! A fire row leases delivery before the side effect. Explicit failures keep
//! the row pending with bounded backoff. Each sink records the stable delivery
//! id before acceptance, so an expired lease cannot repeat an accepted effect.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use futures::{stream, StreamExt};
use tidebreak_core::db::code::{
    acknowledge_trigger_fire_delivery, get_open_turn, get_session, insert_or_load_trigger_fire,
    insert_settled_trigger_fire, latest_turn, lease_trigger_fire_delivery,
    list_active_watches_all_owners, list_attributions_for_pull_requests,
    list_due_trigger_fire_deliveries_all_owners, list_enabled_triggers_all_owners,
    list_pull_request_facts_for_repo, list_sessions_for_workspace,
    reschedule_trigger_fire_delivery_failure, trigger_delivery_accepted, trigger_fire_heads_for_pr,
};
use tidebreak_core::{
    classify_trigger_condition, Attention, AttentionSource, CapLevel, CodeEvent,
    CodePullRequestFact, CodePullRequestId, CodeSession, CodeSessionId, CodeSessionKind,
    CodeSessionLifecycle, CodeTrigger, CodeTriggerAction, CodeTriggerCondition,
    CodeTriggerDeliveryId, CodeTriggerFire, CodeTriggerFireIdentity, CodeTriggerFirePayload,
    CodeTurnId, CodeWorkspaceStatus, HarnessNoticeLevel, OwnerId, PullRequestDigest, RepoId,
    WorkspaceId,
};
use tracing::{debug, warn};

use super::attention::apply_trigger_attention;
use super::delivery::{query_pull_requests_by_number, repository_target_from_local};
use super::runtime::CodeRuntime;
use super::session_worker::journal_event;
use crate::error::ServerError;
use crate::routes::code::types::{
    CodeDeliveryPullRequestSummary, CodeDeliveryPullRequestsPage, CodeDeliveryWorkspaceLink,
    CodeGitHubRepositoryRef, CodeGitHubRepositoryTarget,
};

/// How often the trigger sweep walks enabled triggers.
///
/// Offset from [`super::watch::WATCH_SWEEP_INTERVAL`] rather than equal to it:
/// both sweeps read GitHub, and landing them on the same tick would double the
/// burst a rate limit sees.
pub(crate) const TRIGGER_SWEEP_INTERVAL: Duration = Duration::from_secs(53);
const TRIGGER_REPOSITORY_READ_CONCURRENCY: usize = 4;
const TRIGGER_DUE_BATCH_LIMIT: u64 = 128;
const TRIGGER_DELIVERY_LEASE: chrono::Duration = chrono::Duration::seconds(53 * 3);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RepositoryKey {
    host: String,
    owner: String,
    name: String,
}

impl RepositoryKey {
    fn from_target(target: &CodeGitHubRepositoryTarget) -> Self {
        Self::new(&target.host, &target.owner, &target.name)
    }

    fn from_ref(repository: &CodeGitHubRepositoryRef) -> Self {
        Self::new(&repository.host, &repository.owner, &repository.name)
    }

    fn new(host: &str, owner: &str, name: &str) -> Self {
        Self {
            host: host.trim().to_ascii_lowercase(),
            owner: owner.trim().to_ascii_lowercase(),
            name: name.trim().trim_end_matches(".git").to_ascii_lowercase(),
        }
    }
}

struct RepositoryWork {
    repo_id: RepoId,
    triggers: Vec<CodeTrigger>,
    workspace_ids: HashSet<WorkspaceId>,
    pr_numbers: HashSet<u64>,
    /// Eligible workspaces by the pull-request number their column holds,
    /// so a durable-row fire aims at exactly the holders (decision 66).
    workspaces_by_number: HashMap<u64, HashSet<WorkspaceId>>,
}

#[derive(Default)]
struct EligibleWorkspaces {
    workspace_ids: HashSet<WorkspaceId>,
    pr_numbers: HashSet<u64>,
    workspaces_by_number: HashMap<u64, HashSet<WorkspaceId>>,
}

/// One pass over every enabled trigger. A failure on one repository never
/// stops the others.
pub(crate) async fn sweep_triggers(runtime: &Arc<CodeRuntime>) {
    retry_due_deliveries(runtime).await;

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

    // Build one workset per owner. The delivery query builds an owner-wide
    // workspace index, so calling it once per repository multiplies local git
    // reads by the repository count.
    let mut by_owner: HashMap<OwnerId, HashMap<RepoId, Vec<CodeTrigger>>> = HashMap::new();
    for trigger in triggers {
        by_owner
            .entry(trigger.owner.clone())
            .or_default()
            .entry(trigger.repo_id)
            .or_default()
            .push(trigger);
    }

    for (owner, repositories) in by_owner {
        if let Err(err) = sweep_owner(runtime, &owner, repositories, &watched).await {
            warn!(
                owner = %owner,
                error = %err.message(),
                "code-mode trigger sweep failed for one owner"
            );
        }
    }
}

/// Retry a bounded due page without consulting the pull request again.
async fn retry_due_deliveries(runtime: &Arc<CodeRuntime>) {
    let due = match list_due_trigger_fire_deliveries_all_owners(
        &runtime.db,
        Utc::now(),
        TRIGGER_DUE_BATCH_LIMIT,
    )
    .await
    {
        Ok(due) => due,
        Err(err) => {
            warn!(error = %err, "code-mode trigger sweep could not list due deliveries");
            return;
        }
    };
    for fire in due {
        if let Err(err) = lease_and_deliver(runtime, &fire.identity.owner, fire.delivery_id).await {
            warn!(
                delivery = %fire.delivery_id,
                trigger = %fire.identity.trigger_id,
                workspace = %fire.identity.workspace_id,
                error = %err.message(),
                "code-mode trigger sweep could not retry a due delivery"
            );
        }
    }
}

/// Read one owner's local workset once, then fetch only the pull requests that
/// its eligible workspaces identify.
async fn sweep_owner(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    mut repositories: HashMap<RepoId, Vec<CodeTrigger>>,
    watched: &HashSet<WorkspaceId>,
) -> Result<(), ServerError> {
    let mut eligible: HashMap<RepoId, EligibleWorkspaces> = HashMap::new();
    for workspace in runtime.list_workspaces(owner, None).await? {
        if workspace.status == CodeWorkspaceStatus::Active
            && !watched.contains(&workspace.id)
            && repositories.contains_key(&workspace.repo_id)
        {
            let work = eligible.entry(workspace.repo_id).or_default();
            work.workspace_ids.insert(workspace.id);
            if let Some(pr) = workspace.pr.as_ref() {
                work.pr_numbers.insert(pr.number);
                work.workspaces_by_number
                    .entry(pr.number)
                    .or_default()
                    .insert(workspace.id);
            }
        }
    }

    // Fact-edge conditions read only the local store — the reconcile sweep
    // keeps facts fresh — so they run before the remote-read gate below,
    // which would drop a repository whose workspaces carry facts but no
    // persisted digest (decision 62).
    sweep_fact_edges(runtime, owner, &repositories, &eligible).await;

    // An exact remote read needs both an eligible workspace and its persisted
    // pull-request number. Exclude every other repository before reading its
    // origin or asking GitHub for anything.
    repositories.retain(|repo_id, _| {
        eligible
            .get(repo_id)
            .is_some_and(|work| !work.pr_numbers.is_empty())
    });
    if repositories.is_empty() {
        return Ok(());
    }

    let local_repositories = runtime
        .list_repos(owner)
        .await?
        .into_iter()
        .filter_map(|repo| {
            if repo.removed_at.is_some() {
                return None;
            }
            let triggers = repositories.remove(&repo.id)?;
            let eligible = eligible.remove(&repo.id)?;
            Some((repo, triggers, eligible))
        })
        .collect::<Vec<_>>();

    let resolved = stream::iter(local_repositories)
        .map(|(repo, triggers, eligible)| async move {
            match repository_target_from_local(&repo).await {
                Ok(target) => Some((
                    RepositoryKey::from_target(&target),
                    target,
                    RepositoryWork {
                        repo_id: repo.id,
                        triggers,
                        workspace_ids: eligible.workspace_ids,
                        pr_numbers: eligible.pr_numbers,
                        workspaces_by_number: eligible.workspaces_by_number,
                    },
                )),
                Err(message) => {
                    debug!(
                        repo = %repo.id,
                        error = %message,
                        "code-mode trigger skipped a repository without a GitHub origin"
                    );
                    None
                }
            }
        })
        .buffer_unordered(TRIGGER_REPOSITORY_READ_CONCURRENCY)
        .filter_map(async move |work| work)
        .collect::<Vec<_>>()
        .await;

    let mut work_by_repository: HashMap<RepositoryKey, Vec<RepositoryWork>> = HashMap::new();
    let mut reads_by_repository: HashMap<
        RepositoryKey,
        (CodeGitHubRepositoryTarget, HashSet<u64>),
    > = HashMap::new();
    for (key, target, work) in resolved {
        reads_by_repository
            .entry(key.clone())
            .and_modify(|(_, numbers)| numbers.extend(work.pr_numbers.iter().copied()))
            .or_insert_with(|| (target, work.pr_numbers.clone()));
        work_by_repository.entry(key).or_default().push(work);
    }

    let mut reads = reads_by_repository.into_iter().collect::<Vec<_>>();
    reads.sort_by(|(left, _), (right, _)| left.cmp(right));
    let reads = reads
        .into_iter()
        .map(|(_, (target, numbers))| {
            let mut numbers = numbers.into_iter().collect::<Vec<_>>();
            numbers.sort_unstable();
            (target, numbers)
        })
        .collect();
    sweep_pull_requests(runtime, owner, reads, &work_by_repository).await?;
    Ok(())
}

/// The fingerprint a `pr_opened` fire carries instead of a head SHA: the edge
/// is the pull request coming into existence, once, regardless of where its
/// head moves afterwards. The token cannot collide with a real SHA.
const PR_OPENED_FINGERPRINT: &str = "opened";

/// Fire the fact-edge conditions from the durable store (decision 62).
///
/// `pr_opened` fires once per pull request whose host `created_at` and local
/// `first_seen_at` both postdate the trigger's arming — arming a trigger over
/// existing history stays silent. `pr_updated` fires once per distinct head;
/// the first observed head lands as a settled baseline row so nothing
/// notifies until the head actually moves. Both deliver to the eligible
/// workspaces holding a durable attribution to the pull request. Best-effort
/// throughout: a store failure skips the repository and the next tick
/// retries.
async fn sweep_fact_edges(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    repositories: &HashMap<RepoId, Vec<CodeTrigger>>,
    eligible: &HashMap<RepoId, EligibleWorkspaces>,
) {
    let interested: Vec<(RepoId, Vec<&CodeTrigger>)> = repositories
        .iter()
        .filter_map(|(repo_id, triggers)| {
            let fact_triggers: Vec<&CodeTrigger> = triggers
                .iter()
                .filter(|trigger| {
                    matches!(
                        trigger.condition,
                        CodeTriggerCondition::PrOpened | CodeTriggerCondition::PrUpdated
                    )
                })
                .collect();
            (!fact_triggers.is_empty()).then_some((*repo_id, fact_triggers))
        })
        .collect();
    if interested.is_empty() {
        return;
    }
    let repos: HashMap<RepoId, tidebreak_core::CodeRepo> = match runtime.list_repos(owner).await {
        Ok(repos) => repos.into_iter().map(|repo| (repo.id, repo)).collect(),
        Err(err) => {
            debug!(error = %err.message(), "fact-edge sweep could not list repositories");
            return;
        }
    };

    for (repo_id, triggers) in interested {
        let Some(work) = eligible.get(&repo_id) else {
            continue;
        };
        if work.workspace_ids.is_empty() {
            continue;
        }
        let Some(repo) = repos.get(&repo_id) else {
            continue;
        };
        // The reconcile sweep records the origin identity; a cold start falls
        // back to one local git read.
        let (host, repo_owner, repo_name) =
            match (&repo.origin_host, &repo.origin_owner, &repo.origin_name) {
                (Some(host), Some(repo_owner), Some(repo_name)) => {
                    (host.clone(), repo_owner.clone(), repo_name.clone())
                }
                _ => match repository_target_from_local(repo).await {
                    Ok(target) => (target.host, target.owner, target.name),
                    Err(message) => {
                        debug!(
                            repo = %repo.id,
                            error = %message,
                            "fact-edge sweep skipped a repository without a GitHub origin"
                        );
                        continue;
                    }
                },
            };
        let facts = match list_pull_request_facts_for_repo(
            &runtime.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
        )
        .await
        {
            Ok(facts) => facts,
            Err(err) => {
                debug!("fact-edge sweep could not read facts: {err}");
                continue;
            }
        };
        if facts.is_empty() {
            continue;
        }
        let ids: Vec<CodePullRequestId> = facts.iter().map(|fact| fact.id).collect();
        let attributions = match list_attributions_for_pull_requests(&runtime.db, owner, &ids).await
        {
            Ok(attributions) => attributions,
            Err(err) => {
                debug!("fact-edge sweep could not read attributions: {err}");
                continue;
            }
        };
        let mut workspaces_by_fact: HashMap<CodePullRequestId, Vec<WorkspaceId>> = HashMap::new();
        for attribution in attributions {
            if work.workspace_ids.contains(&attribution.workspace_id) {
                workspaces_by_fact
                    .entry(attribution.pull_request_id)
                    .or_default()
                    .push(attribution.workspace_id);
            }
        }

        for fact in &facts {
            let Some(targets) = workspaces_by_fact.get(&fact.id) else {
                continue;
            };
            for trigger in &triggers {
                for workspace_id in targets {
                    if let Err(err) =
                        fire_fact_edge(runtime, owner, trigger, *workspace_id, fact).await
                    {
                        warn!(
                            trigger = %trigger.id,
                            workspace = %workspace_id,
                            error = %err.message(),
                            "code-mode fact-edge fire failed"
                        );
                    }
                }
            }
        }
    }
}

/// Fire one fact edge for one trigger on one workspace, or record its
/// baseline.
async fn fire_fact_edge(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    trigger: &CodeTrigger,
    workspace_id: WorkspaceId,
    fact: &CodePullRequestFact,
) -> Result<(), ServerError> {
    match trigger.condition {
        CodeTriggerCondition::PrOpened => {
            if fact.created_at < trigger.created_at || fact.first_seen_at < trigger.created_at {
                return Ok(());
            }
            let digest = super::pr_facts::digest_from_fact(fact);
            fire_one(
                runtime,
                owner,
                trigger,
                workspace_id,
                &digest,
                PR_OPENED_FINGERPRINT,
            )
            .await
        }
        CodeTriggerCondition::PrUpdated => {
            // A settled pull request's head is history; merged and closed have
            // their own conditions.
            if fact.state != tidebreak_core::CodePullRequestState::Open {
                return Ok(());
            }
            let Some(head) = fact.head_sha.as_deref() else {
                return Ok(());
            };
            let heads = trigger_fire_heads_for_pr(
                &runtime.db,
                owner,
                trigger.id,
                workspace_id,
                fact.number,
            )
            .await?;
            if heads.iter().any(|known| known == head) {
                return Ok(());
            }
            if heads.is_empty() {
                // First sight is the baseline, never a notification.
                let identity = CodeTriggerFireIdentity {
                    trigger_id: trigger.id,
                    owner: owner.clone(),
                    workspace_id,
                    pr_number: fact.number,
                    head_sha: head.to_owned(),
                };
                insert_settled_trigger_fire(&runtime.db, &identity, Utc::now()).await?;
                return Ok(());
            }
            let digest = super::pr_facts::digest_from_fact(fact);
            fire_one(runtime, owner, trigger, workspace_id, &digest, head).await
        }
        _ => Ok(()),
    }
}

/// Consume the durable rows first (decision 66): a fresh live tier answers
/// without a host read, and only the rows the store cannot answer freshly
/// fall back to one exact-number read — which itself lands back on the
/// store through the delivery path's persistence.
async fn sweep_pull_requests(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    repositories: Vec<(CodeGitHubRepositoryTarget, Vec<u64>)>,
    work_by_repository: &HashMap<RepositoryKey, Vec<RepositoryWork>>,
) -> Result<(), ServerError> {
    let now = Utc::now();
    let mut residual: Vec<(CodeGitHubRepositoryTarget, Vec<u64>)> = Vec::new();
    for (target, numbers) in repositories {
        let key = RepositoryKey::from_target(&target);
        let Some(repository_work) = work_by_repository.get(&key) else {
            continue;
        };
        let repo_facts = match tidebreak_core::db::code::list_pull_request_facts_for_repo(
            &runtime.db,
            owner,
            &target.host,
            &target.owner,
            &target.name,
        )
        .await
        {
            Ok(facts) => facts,
            Err(err) => {
                debug!(error = %err, "code-mode trigger sweep could not read fact rows");
                residual.push((target, numbers));
                continue;
            }
        };
        let parents = super::reconcile::stack_parents_by_head(&repo_facts);
        let mut stale = Vec::new();
        for number in numbers {
            let fresh = repo_facts.iter().find(|fact| {
                fact.number == number
                    && fact
                        .live
                        .as_ref()
                        .is_some_and(|live| super::reconcile::live_tier_is_fresh(live, now))
            });
            let Some(fact) = fresh else {
                stale.push(number);
                continue;
            };
            let digest = super::pr_facts::digest_from_fact(fact);
            let stack_parent = parents
                .get(&fact.base_branch)
                .copied()
                .filter(|parent| *parent != fact.number);
            for work in repository_work {
                claim_fires_from_row(runtime, owner, work, &digest, stack_parent).await;
            }
        }
        if !stale.is_empty() {
            residual.push((target, stale));
        }
    }
    if residual.is_empty() {
        return Ok(());
    }
    let page = query_pull_requests_by_number(runtime, owner, residual).await?;
    if !github_available(owner, &page) {
        return Ok(());
    }
    warn_source_errors(owner, &page);
    for item in &page.items {
        let key = RepositoryKey::from_ref(&item.repository);
        let Some(repository_work) = work_by_repository.get(&key) else {
            continue;
        };
        for work in repository_work {
            claim_fires(runtime, owner, work, item).await;
        }
    }
    Ok(())
}

fn github_available(owner: &OwnerId, page: &CodeDeliveryPullRequestsPage) -> bool {
    if !page.capability.found {
        warn!(
            owner = %owner,
            remediation = %page.capability.remediation,
            "code-mode trigger sweep cannot read GitHub because gh is unavailable"
        );
        return false;
    }
    if page.capability.authenticated != Some(true) {
        warn!(
            owner = %owner,
            remediation = %page.capability.remediation,
            "code-mode trigger sweep cannot read GitHub because gh is signed out"
        );
        return false;
    }
    true
}

fn warn_source_errors(owner: &OwnerId, page: &CodeDeliveryPullRequestsPage) {
    for error in &page.errors {
        let repository = error
            .repository
            .as_ref()
            .map(|target| format!("{}/{}/{}", target.host, target.owner, target.name))
            .unwrap_or_else(|| "unknown".to_owned());
        warn!(
            owner = %owner,
            repository = %repository,
            kind = %error.kind,
            error = %error.message,
            "code-mode trigger sweep could not read one GitHub repository"
        );
    }
}

/// Claim and deliver fires for one durable row's digest (decision 66): the
/// same classification and the same stacked-child hold as the fetched path,
/// aimed at the eligible workspaces whose column holds the pull request.
async fn claim_fires_from_row(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    work: &RepositoryWork,
    digest: &PullRequestDigest,
    stack_parent: Option<u64>,
) {
    // Without a head SHA the fire cannot be fingerprinted, and a fire that
    // cannot be bounded would repeat every tick.
    let Some(head_sha) = digest.head_sha.clone() else {
        return;
    };
    let Some(condition) = classify_trigger_condition(digest) else {
        return;
    };
    // A stacked child is behind or blocked *because of its parent*
    // (decision 62). Firing Behind or ReviewRequired at it would send an
    // agent to rebase onto a branch that moves with every parent push.
    if stack_parent.is_some()
        && matches!(
            condition,
            CodeTriggerCondition::Behind | CodeTriggerCondition::ReviewRequired
        )
    {
        debug!(
            number = digest.number,
            parent = stack_parent,
            "code-mode trigger held a stacked child's fire"
        );
        return;
    }
    let Some(workspaces) = work.workspaces_by_number.get(&digest.number) else {
        return;
    };
    for trigger in work
        .triggers
        .iter()
        .filter(|trigger| trigger.condition == condition)
    {
        for workspace_id in workspaces {
            if let Err(err) =
                fire_one(runtime, owner, trigger, *workspace_id, digest, &head_sha).await
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

/// Claim and deliver one fire per matching trigger per linked workspace.
async fn claim_fires(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    work: &RepositoryWork,
    item: &CodeDeliveryPullRequestSummary,
) {
    // Without a head SHA the fire cannot be fingerprinted, and a fire that
    // cannot be bounded would repeat every tick.
    let Some(head_sha) = item.head_sha.clone() else {
        return;
    };
    let digest = super::delivery::digest_from_summary(item);
    let Some(condition) = classify_trigger_condition(&digest) else {
        return;
    };
    // A stacked child is behind or blocked *because of its parent* — the
    // summary carries the parent from the durable fact set (decision 62).
    // Firing Behind or ReviewRequired at it would send an agent to rebase
    // onto a branch that moves with every parent push.
    if item.stack_parent_number.is_some()
        && matches!(
            condition,
            CodeTriggerCondition::Behind | CodeTriggerCondition::ReviewRequired
        )
    {
        debug!(
            number = item.number,
            parent = item.stack_parent_number,
            "code-mode trigger held a stacked child's fire"
        );
        return;
    }
    let workspaces = linked_workspaces(&item.workspace_links, work.repo_id, &work.workspace_ids);
    if workspaces.is_empty() {
        return;
    }
    for trigger in work
        .triggers
        .iter()
        .filter(|trigger| trigger.condition == condition)
    {
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

/// Capture one immutable pull-request edge, then drive its outbox row.
async fn fire_one(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    trigger: &CodeTrigger,
    workspace_id: WorkspaceId,
    digest: &PullRequestDigest,
    head_sha: &str,
) -> Result<(), ServerError> {
    let identity = CodeTriggerFireIdentity {
        trigger_id: trigger.id,
        owner: owner.clone(),
        workspace_id,
        pr_number: digest.number,
        head_sha: head_sha.to_owned(),
    };
    let payload = CodeTriggerFirePayload {
        action: trigger.action,
        condition: trigger.condition,
        message: trigger_message(trigger.condition, digest),
    };
    let now = Utc::now();
    let Some(fire) = insert_or_load_trigger_fire(&runtime.db, &identity, &payload, now).await?
    else {
        return Ok(());
    };
    lease_and_deliver(runtime, owner, fire.delivery_id).await
}

/// Lease one pending row by id, then deliver from its stored payload.
async fn lease_and_deliver(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
) -> Result<(), ServerError> {
    let now = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    let Some(fire) = lease_trigger_fire_delivery(
        &runtime.db,
        owner,
        delivery_id,
        lease_token,
        now,
        now + TRIGGER_DELIVERY_LEASE,
    )
    .await?
    else {
        return Ok(());
    };
    deliver_leased_fire(runtime, fire, lease_token).await
}

/// Acknowledge or reschedule one leased row without rebuilding its event from
/// current GitHub state.
async fn deliver_leased_fire(
    runtime: &Arc<CodeRuntime>,
    fire: CodeTriggerFire,
    lease_token: uuid::Uuid,
) -> Result<(), ServerError> {
    let owner = &fire.identity.owner;
    let payload = fire
        .payload
        .as_ref()
        .ok_or_else(|| ServerError::internal("pending trigger delivery has no payload"))?;

    // A sink may have accepted this id before the process could acknowledge
    // the outbox. Settle that durable boundary before replanning: the session,
    // turn, or selected sink may have changed while the lease was unavailable.
    if trigger_delivery_accepted(&runtime.db, owner, fire.delivery_id).await? {
        if !acknowledge_trigger_fire_delivery(
            &runtime.db,
            owner,
            fire.delivery_id,
            lease_token,
            Utc::now(),
        )
        .await?
        {
            warn!(
                delivery = %fire.delivery_id,
                "code-mode trigger could not acknowledge a previously accepted delivery"
            );
        }
        return Ok(());
    }

    let Some(delivery) =
        plan_delivery(runtime, owner, fire.identity.workspace_id, payload.action).await?
    else {
        reschedule_delivery_failure(
            runtime,
            &fire,
            lease_token,
            "no eligible session can accept this trigger delivery",
        )
        .await;
        return Ok(());
    };

    let session_id = delivery.session_id();
    if let Err(delivery_error) = deliver_fire(runtime, &fire, payload, delivery, lease_token).await
    {
        reschedule_delivery_failure(runtime, &fire, lease_token, delivery_error.message()).await;
        return Err(delivery_error);
    }
    if !acknowledge_trigger_fire_delivery(
        &runtime.db,
        owner,
        fire.delivery_id,
        lease_token,
        Utc::now(),
    )
    .await?
    {
        warn!(
            delivery = %fire.delivery_id,
            "code-mode trigger sink accepted a delivery after its lease became stale"
        );
    }
    debug!(
        trigger = %fire.identity.trigger_id,
        workspace = %fire.identity.workspace_id,
        pr = fire.identity.pr_number,
        condition = ?payload.condition,
        delivery = ?delivery,
        "code-mode trigger fired"
    );
    note_fire(
        runtime,
        owner,
        session_id,
        &fire.identity,
        payload.condition,
    )
    .await;
    Ok(())
}

async fn reschedule_delivery_failure(
    runtime: &Arc<CodeRuntime>,
    fire: &CodeTriggerFire,
    lease_token: uuid::Uuid,
    error: &str,
) {
    match reschedule_trigger_fire_delivery_failure(
        &runtime.db,
        &fire.identity.owner,
        fire.delivery_id,
        lease_token,
        Utc::now(),
        error,
    )
    .await
    {
        Ok(Some(retry_at)) => debug!(
            delivery = %fire.delivery_id,
            retry_at = %retry_at,
            "code-mode trigger delivery scheduled for retry"
        ),
        Ok(None) => warn!(
            delivery = %fire.delivery_id,
            "code-mode trigger delivery lost its lease before failure reschedule"
        ),
        Err(reschedule_error) => warn!(
            trigger = %fire.identity.trigger_id,
            workspace = %fire.identity.workspace_id,
            pr = fire.identity.pr_number,
            error = %reschedule_error,
            "code-mode trigger could not reschedule a failed delivery"
        ),
    }
}

async fn deliver_fire(
    runtime: &Arc<CodeRuntime>,
    fire: &CodeTriggerFire,
    payload: &CodeTriggerFirePayload,
    delivery: Delivery,
    lease_token: uuid::Uuid,
) -> Result<(), ServerError> {
    let owner = &fire.identity.owner;
    let delivery_id = fire.delivery_id;
    let message = payload.message.clone();
    let session_id = delivery.session_id();
    match delivery {
        Delivery::Steer { turn_id, .. } => {
            runtime
                .steer_trigger(
                    owner,
                    session_id,
                    turn_id,
                    message,
                    delivery_id,
                    lease_token,
                )
                .await?;
        }
        Delivery::Turn { .. } => {
            runtime
                .submit_trigger_turn(owner, session_id, message, delivery_id, lease_token)
                .await?;
        }
        Delivery::Notify { .. } => {
            apply_trigger_attention(
                &runtime.db,
                &runtime.bus,
                owner,
                session_id,
                delivery_id,
                lease_token,
                Attention::needs_you(
                    describe_condition(payload.condition, fire.identity.pr_number),
                    AttentionSource::Structured,
                ),
            )
            .await?;
        }
    }
    Ok(())
}

/// Which session a fire reaches and how, or `None` to retry after backoff.
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
    // outbox pending so a later lease delivers it.
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
        CodeTriggerCondition::PrOpened => format!("pull request #{number} opened"),
        CodeTriggerCondition::PrUpdated => format!("#{number} has a new head"),
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
    identity: &CodeTriggerFireIdentity,
    condition: CodeTriggerCondition,
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
                identity.trigger_id,
                describe_condition(condition, identity.pr_number)
            ),
        },
    )
    .await;
}

/// Active workspaces this pull request is exactly on, minus watched ones.
fn linked_workspaces(
    links: &[CodeDeliveryWorkspaceLink],
    repo_id: RepoId,
    eligible: &HashSet<WorkspaceId>,
) -> Vec<WorkspaceId> {
    links
        .iter()
        // A fuzzy link is a branch-name guess. Firing on one would wake an
        // agent about someone else's pull request.
        .filter(|link| link.exact)
        .filter(|link| link.status == CodeWorkspaceStatus::Active)
        .filter(|link| link.repo_id == repo_id)
        .filter(|link| eligible.contains(&link.workspace_id))
        .map(|link| link.workspace_id)
        .collect()
}

/// The bulk summary read as the digest the classifier is written against.
///
/// Both paths lowercase their host tokens already — `normalized_optional` here
/// and `lower_token` in `gh.rs` — so the tokens pass straight through.
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

    use crate::code::delivery::digest_from_summary;
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
            in_merge_queue: None,
            comment_count: None,
            checks: Vec::new(),
            attention_reasons: Vec::new(),
            ready_to_merge: true,
            workspace_links: Vec::new(),
            stack_parent_number: None,
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
            relation: None,
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

        let digest = digest_from_summary(&item);
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
            classify_trigger_condition(&digest_from_summary(&item)),
            Some(CodeTriggerCondition::Merged)
        );

        let mut closed = summary();
        closed.state = "closed".to_owned();
        assert_eq!(
            classify_trigger_condition(&digest_from_summary(&closed)),
            Some(CodeTriggerCondition::Closed)
        );
    }

    #[test]
    fn only_exact_active_unwatched_workspaces_are_targets() {
        let exact_active = link(true, CodeWorkspaceStatus::Active);
        let mut fuzzy = link(false, CodeWorkspaceStatus::Active);
        fuzzy.repo_id = exact_active.repo_id;
        let mut archived = link(true, CodeWorkspaceStatus::Archived);
        archived.repo_id = exact_active.repo_id;
        let other_repo = link(true, CodeWorkspaceStatus::Active);

        let eligible = HashSet::from([
            exact_active.workspace_id,
            fuzzy.workspace_id,
            archived.workspace_id,
            other_repo.workspace_id,
        ]);
        let links = vec![exact_active.clone(), fuzzy, archived, other_repo];

        let targets = linked_workspaces(&links, exact_active.repo_id, &eligible);
        assert_eq!(targets, vec![exact_active.workspace_id]);
    }

    /// A fuzzy link is a branch-name guess. Firing on one would wake an agent
    /// about somebody else's pull request.
    #[test]
    fn a_fuzzy_link_alone_produces_no_target() {
        let fuzzy = link(false, CodeWorkspaceStatus::Active);
        let eligible = HashSet::from([fuzzy.workspace_id]);
        assert!(
            linked_workspaces(std::slice::from_ref(&fuzzy), fuzzy.repo_id, &eligible).is_empty()
        );
    }

    #[test]
    fn repository_keys_match_case_insensitively() {
        let target = CodeGitHubRepositoryTarget {
            host: "GitHub.COM".to_owned(),
            owner: "Example".to_owned(),
            name: "Demo.git".to_owned(),
        };
        let mut repository = repository();
        repository.host = "github.com".to_owned();
        repository.owner = "example".to_owned();
        repository.name = "demo".to_owned();

        assert_eq!(
            RepositoryKey::from_target(&target),
            RepositoryKey::from_ref(&repository)
        );
    }
}
