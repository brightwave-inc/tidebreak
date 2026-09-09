//! External pull-request delivery publishing (issue 3203).
//!
//! One authoritative writer turns durable pull-request facts and live-tier
//! changes into session-journal events on every session that holds an
//! external binding under the pull request's owner. The journal is replayable
//! and authorization is exactly the existing external stream: a grant may
//! replay only the sessions its own bindings name, and a revocation severs
//! the live socket. The per-PR delivery-state table suppresses duplicates for
//! the same fact, so repeated host observations of an unchanged state write
//! nothing more.

use tidebreak_core::db::code::{
    list_external_bindings_for_sessions, list_sessions_for_workspace,
    list_workspaces_by_status_all_owners, PrDeliveryFamily,
};
use tidebreak_core::{
    CodePullRequestFact, CodeWatch, Event, OwnerId, PullRequestCheck, PullRequestCheckFailure,
    PullRequestEventIdentity, PullRequestWatchEvent, Session,
};

use crate::code::runtime::CodeRuntime;

/// Names a delivery event type the per-PR publication cursor tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EventKind {
    Opened,
    ChecksPending,
    ChecksFailed,
    ReviewRequested,
    ChangesRequested,
    Approved,
    Mergeable,
    Merged,
    Watch,
    Review,
}

impl EventKind {
    pub(crate) fn as_family(self) -> PrDeliveryFamily {
        match self {
            Self::Opened => PrDeliveryFamily::Opened,
            Self::ChecksPending => PrDeliveryFamily::ChecksPending,
            Self::ChecksFailed => PrDeliveryFamily::ChecksFailed,
            Self::ReviewRequested => PrDeliveryFamily::ReviewRequested,
            Self::ChangesRequested => PrDeliveryFamily::ChangesRequested,
            Self::Approved => PrDeliveryFamily::Approved,
            Self::Mergeable => PrDeliveryFamily::Mergeable,
            Self::Merged => PrDeliveryFamily::Merged,
            Self::Watch => PrDeliveryFamily::Watch,
            Self::Review => PrDeliveryFamily::Review,
        }
    }
}

pub fn identity_of(fact: &CodePullRequestFact) -> PullRequestEventIdentity {
    PullRequestEventIdentity {
        host: fact.host.clone(),
        repo_owner: fact.repo_owner.clone(),
        repo_name: fact.repo_name.clone(),
        number: fact.number,
    }
}

#[allow(dead_code)]
pub(crate) fn opened_event(fact: &CodePullRequestFact) -> Event {
    Event::PullRequestOpened {
        pull_request: identity_of(fact),
        tenant: fact.owner.to_string(),
        title: fact.title.clone(),
        url: fact.url.clone(),
        draft: fact.draft,
        head_branch: fact.head_branch.clone(),
        base_branch: fact.base_branch.clone(),
    }
}

#[allow(dead_code)]
pub(crate) fn checks_pending_event(
    fact: &CodePullRequestFact,
    pending: &[PullRequestCheck],
) -> Event {
    Event::PullRequestChecksPending {
        pull_request: identity_of(fact),
        tenant: fact.owner.to_string(),
        head_sha: fact.head_sha.clone().unwrap_or_default(),
        pending: pending.iter().map(|check| check.name.clone()).collect(),
    }
}

#[allow(dead_code)]
pub(crate) fn checks_failed_event(
    fact: &CodePullRequestFact,
    failures: &[PullRequestCheck],
) -> Event {
    Event::PullRequestChecksFailed {
        pull_request: identity_of(fact),
        tenant: fact.owner.to_string(),
        head_sha: fact.head_sha.clone().unwrap_or_default(),
        failures: failures
            .iter()
            .map(|check| PullRequestCheckFailure {
                name: check.name.clone(),
                detail: check.detail.clone(),
                url: check.url.clone(),
            })
            .collect(),
    }
}

/// Enqueue one delivery event for every externally bound session that can
/// read this pull request. The outbox row is per session; a failed append
/// leaves that row pending and the delivery sweep retries it, so one
/// session's transient failure never suppresses another session's event.
pub async fn publish_pull_request_event(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    fact: &CodePullRequestFact,
    kind: EventKind,
    event: Event,
) {
    publish_pull_request_events(runtime, owner, fact, &[(kind, event)]).await;
}

/// Batch variant: all events for one observation, planned in order so a
/// reconnect replays the same sequence. Each family advances only on a real
/// state-token change; repeated identical observations enqueue nothing.
pub async fn publish_pull_request_events(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    fact: &CodePullRequestFact,
    events: &[(EventKind, Event)],
) {
    if events.is_empty() {
        return;
    }
    let sessions = delivery_sessions(runtime, owner, fact).await;
    for (kind, event) in events {
        let state_token = state_token_for(*kind, fact, event);
        for session in &sessions {
            let planned = match tidebreak_core::db::code::plan_delivery(
                &runtime.db,
                owner,
                session.id,
                &fact.host,
                &fact.repo_owner,
                &fact.repo_name,
                fact.number,
                kind.as_family(),
                &state_token,
                chrono::Utc::now(),
            )
            .await
            {
                Ok(planned) => planned,
                Err(error) => {
                    tracing::debug!(
                        session = %session.id,
                        error = %error,
                        "pull-request delivery planning failed"
                    );
                    continue;
                }
            };
            if let Some(occurrence) = planned {
                let _ = tidebreak_core::db::code::enqueue_delivery(
                    &runtime.db,
                    owner,
                    session.id,
                    &fact.host,
                    &fact.repo_owner,
                    &fact.repo_name,
                    fact.number,
                    kind.as_family(),
                    occurrence,
                    event,
                    chrono::Utc::now(),
                )
                .await;
            }
        }
    }
}

fn state_token_for(kind: EventKind, fact: &CodePullRequestFact, event: &Event) -> String {
    let head = fact.head_sha.clone().unwrap_or_default();
    match kind {
        EventKind::Opened => format!("{head}/open"),
        EventKind::ChecksPending => {
            if let Event::PullRequestChecksPending { pending, .. } = event {
                format!("{head}/pending/{:?}", pending)
            } else {
                format!("{head}/pending")
            }
        }
        EventKind::ChecksFailed => {
            if let Event::PullRequestChecksFailed { failures, .. } = event {
                format!(
                    "{head}/failed/{}",
                    failures
                        .iter()
                        .map(|check| check.name.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else {
                format!("{head}/failed")
            }
        }
        EventKind::ReviewRequested => format!("{head}/review_requested"),
        EventKind::ChangesRequested => {
            if let Event::PullRequestChangesRequested { findings, .. } = event {
                format!(
                    "{head}/changes/{}",
                    findings
                        .iter()
                        .filter_map(|f| f.id.as_deref())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else {
                format!("{head}/changes")
            }
        }
        EventKind::Approved => {
            if let Event::PullRequestApproved { reviewers, .. } = event {
                format!("{head}/approved/{}", reviewers.join(","))
            } else {
                format!("{head}/approved")
            }
        }
        EventKind::Mergeable => format!("{head}/mergeable"),
        EventKind::Merged => {
            if let Event::PullRequestMerged { merged_at, .. } = event {
                format!("{head}/merged/{merged_at}")
            } else {
                format!("{head}/merged")
            }
        }
        EventKind::Watch => {
            if let Event::PullRequestWatch { watch, .. } = event {
                format!(
                    "{}/{}/{}",
                    watch.state,
                    watch.detail.as_deref().unwrap_or(""),
                    watch.cycles
                )
            } else {
                String::new()
            }
        }
        EventKind::Review => {
            if let Event::PullRequestReview { review, .. } = event {
                format!(
                    "{head}/review/{}",
                    review.id.map_or_else(String::new, |id| id.to_string())
                )
            } else {
                String::new()
            }
        }
    }
}

/// The externally bound sessions under this owner that should receive PR
/// delivery events: every active workspace with a durable attribution to
/// this pull request (decision 77). Attribution, not the workspace's current
/// digest, is the tie that survives several PRs per workspace and facts out
/// of the current checkout.
async fn delivery_sessions(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    fact: &CodePullRequestFact,
) -> Vec<Session> {
    let Ok(workspaces) = list_workspaces_by_status_all_owners(
        &runtime.db,
        tidebreak_core::CodeWorkspaceStatus::Active,
    )
    .await
    else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for workspace in workspaces {
        if workspace.owner != *owner {
            continue;
        }
        let Ok(attributed) =
            tidebreak_core::db::code::list_attributed_facts_for_workspace(
                &runtime.db,
                owner,
                workspace.id,
            )
            .await
        else {
            continue;
        };
        let holds = attributed.iter().any(|(candidate, _)| {
            candidate.host.eq_ignore_ascii_case(&fact.host)
                && candidate.repo_owner.eq_ignore_ascii_case(&fact.repo_owner)
                && candidate.repo_name.eq_ignore_ascii_case(&fact.repo_name)
                && candidate.number == fact.number
        });
        if !holds {
            continue;
        }
        let Ok(rows) = list_sessions_for_workspace(&runtime.db, owner, workspace.id).await else {
            continue;
        };
        let ids: Vec<_> = rows.iter().map(|session| session.id).collect();
        let Ok(bindings) = list_external_bindings_for_sessions(&runtime.db, owner, &ids).await
        else {
            continue;
        };
        if bindings.is_empty() {
            continue;
        }
        sessions.extend(rows.into_iter().filter(|session| {
            session.lifecycle != tidebreak_core::SessionLifecycle::Ended
                && session.lifecycle != tidebreak_core::SessionLifecycle::Fenced
        }));
    }
    sessions
}

/// Publish the watch transition event for one watch row.
pub async fn publish_watch_transition(runtime: &CodeRuntime, watch: &CodeWatch, trigger: &str) {
    let Some(fact) = watch_fact(runtime, watch).await else {
        return;
    };
    let event = Event::PullRequestWatch {
        pull_request: identity_of(&fact),
        tenant: fact.owner.to_string(),
        watch: PullRequestWatchEvent {
            state: watch.state.as_str().to_owned(),
            detail: watch.detail.clone(),
            last_fix_head: watch.last_fix_head.clone(),
            cycles: watch.cycles,
            trigger: trigger.to_owned(),
        },
    };
    let _ = publish_pull_request_event(runtime, &watch.owner, &fact, EventKind::Watch, event).await;
}

/// Resolve the durable fact behind a watch row by its PR identity.
async fn watch_fact(runtime: &CodeRuntime, watch: &CodeWatch) -> Option<CodePullRequestFact> {
    let workspace =
        tidebreak_core::db::code::get_workspace(&runtime.db, &watch.owner, watch.workspace_id)
            .await
            .ok()
            .flatten()?;
    let url = workspace.pr.as_ref()?.url.as_deref()?;
    let (host, repo_owner, repo_name, number) =
        crate::code::pr_facts::pull_request_identity_from_url(url)?;
    tidebreak_core::db::code::get_pull_request_fact(
        &runtime.db,
        &watch.owner,
        &host,
        &repo_owner,
        &repo_name,
        number,
    )
    .await
    .ok()
    .flatten()
}

/// Build the delivery events one digest observation produced from its live
/// tier, in publication order.
///
/// `old` is the tier before the write. The host's language is authoritative:
/// `checks_pending` fires when a pending check exists, `checks_failed` when
/// a failing one does, `review_requested` when the decision says so,
/// `changes_requested`/`approved` from the decision word, and `mergeable`
/// when the host says the branch is mergeable. Review-bot reviews arrive as
/// their own `review` events wherever the caller observed them (issue 3203).
#[allow(dead_code)]
pub(crate) fn events_from_live_transition(
    fact: &CodePullRequestFact,
    old: Option<&tidebreak_core::CodePullRequestLiveState>,
) -> Vec<(EventKind, Event)> {
    let Some(live) = fact.live.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let checks = live.checks.as_deref().unwrap_or(&[]);
    let pending: Vec<_> = checks
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Pending)
        .cloned()
        .collect();
    let failures: Vec<_> = checks
        .iter()
        .filter(|check| check.bucket == tidebreak_core::PullRequestCheckBucket::Fail)
        .cloned()
        .collect();
    if !pending.is_empty() {
        out.push((
            EventKind::ChecksPending,
            checks_pending_event(fact, &pending),
        ));
    }
    if !failures.is_empty() {
        out.push((
            EventKind::ChecksFailed,
            checks_failed_event(fact, &failures),
        ));
    }
    let review = live.review_decision.as_deref().unwrap_or("");
    match review {
        "review_required" => out.push((
            EventKind::ReviewRequested,
            Event::PullRequestReviewRequested {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
            },
        )),
        // `changes_requested` is emitted by [`Self::review_delivery_events`]
        // with the findings; the decision word alone would carry an empty
        // count and double-publish when comments arrive.
        "approved" => out.push((
            EventKind::Approved,
            Event::PullRequestApproved {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
                reviewers: Vec::new(),
            },
        )),
        _ => {}
    }
    if live.mergeable.as_deref() == Some("mergeable") {
        out.push((
            EventKind::Mergeable,
            Event::PullRequestMergeable {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
            },
        ));
    }
    if fact.state == tidebreak_core::CodePullRequestState::Merged {
        out.push((
            EventKind::Merged,
            Event::PullRequestMerged {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
                merged_at: fact.merged_at.map(|at| at.to_rfc3339()).unwrap_or_default(),
            },
        ));
    }
    // Opened is a snapshot edge, not a live word: emit it once from the
    // fact's first observation. The caller suppresses repeated observations
    // through the per-PR state row.
    if old.is_none() && fact.state == tidebreak_core::CodePullRequestState::Open {
        out.insert(0, (EventKind::Opened, opened_event(fact)));
    }
    out
}

/// Drain pending delivery rows for one session: append each event to that
/// session's journal and mark the row delivered in the same transaction.
///
/// Runs on the existing sweep cadence and on attach, so a row that failed
/// once is retried without losing ordering: the oldest pending row goes
/// first, and each append is fenced by the session's current spawn epoch.
pub async fn sweep_deliveries_for_session(runtime: &CodeRuntime, session: &Session) {
    if session.lifecycle == tidebreak_core::SessionLifecycle::Ended
        || session.lifecycle == tidebreak_core::SessionLifecycle::Fenced
    {
        return;
    }
    let Ok(pending) = tidebreak_core::db::code::pending_deliveries_for_session(
        &runtime.db,
        &session.owner,
        session.id,
    )
    .await
    else {
        return;
    };
    for (family, occurrence, event) in pending {
        match tidebreak_core::db::code::deliver_outbox_row(
            &runtime.db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            family,
            occurrence,
            event.clone(),
        )
        .await
        {
            Ok(Some(seq)) => {
                runtime.bus.publish(
                    session.id,
                    tidebreak_core::code::SequencedEvent { seq, event },
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(
                    session = %session.id,
                    family = %family.as_str(),
                    occurrence = %occurrence,
                    error = %error,
                    "pull-request delivery retry failed"
                );
                break;
            }
        }
    }
}

/// Build review and finding delivery events from a comments read.
///
/// Review submissions (including review bots) become their own `review`
/// events; `changes_requested` carries the findings counted from submitted
/// review bodies and inline comments. Comments with no body contribute
/// nothing, and a comment whose stable id cannot be read still renders but
/// its dedup falls back to author+timestamp.
pub(crate) fn review_delivery_events(
    fact: &CodePullRequestFact,
    comments: &[tidebreak_core::PullRequestComment],
) -> Vec<(EventKind, Event)> {
    let mut out = Vec::new();
    let mut findings = Vec::new();
    for comment in comments {
        if comment.kind == tidebreak_core::PullRequestCommentKind::Inline
            || comment.kind == tidebreak_core::PullRequestCommentKind::Review
        {
            let summary = comment.body.trim();
            if !summary.is_empty() {
                findings.push(tidebreak_core::PullRequestReviewFinding {
                    id: comment.id.clone(),
                    author: comment.author.clone(),
                    path: comment.path.clone(),
                    summary: summary
                        .chars()
                        .take(tidebreak_core::MAX_TOOL_SUMMARY_CHARS)
                        .collect(),
                });
            }
        }
        if comment.kind != tidebreak_core::PullRequestCommentKind::Review {
            continue;
        }
        let body = comment.body.trim();
        if body.is_empty() {
            continue;
        }
        let state = comment
            .review_state
            .as_deref()
            .unwrap_or("COMMENTED")
            .to_ascii_uppercase();
        out.push((
            EventKind::Review,
            Event::PullRequestReview {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
                review: tidebreak_core::PullRequestReviewEvent {
                    id: comment.id.as_deref().and_then(|id| id.parse().ok()),
                    author: comment.author.clone().unwrap_or_default(),
                    state,
                    body: Some(body.to_owned()),
                    submitted_at: comment.created_at.clone(),
                },
            },
        ));
    }
    let live_decision = fact
        .live
        .as_ref()
        .and_then(|live| live.review_decision.as_deref());
    if live_decision == Some("changes_requested") {
        out.push((
            EventKind::ChangesRequested,
            Event::PullRequestChangesRequested {
                pull_request: identity_of(fact),
                tenant: fact.owner.to_string(),
                reviewers: comments
                    .iter()
                    .filter(|comment| {
                        comment.kind == tidebreak_core::PullRequestCommentKind::Review
                    })
                    .filter_map(|comment| comment.author.clone())
                    .collect::<Vec<_>>(),
                findings,
            },
        ));
    }
    out
}
