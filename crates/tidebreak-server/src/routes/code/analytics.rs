//! Owner-scoped analytics for code mode.
//!
//! Local turn usage stays authoritative. Subscription quota data has its own
//! optional route because Model Gateway is not required for this report.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::extract::Query;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Deserialize;
use tidebreak_core::{
    CodePullRequestId, CodeSession, CodeSessionId, CodeTurnStatus, HarnessKind, RepoId, WorkspaceId,
};

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;

use super::types::{
    CodeAnalyticsDay, CodeAnalyticsHarness, CodeAnalyticsModel, CodeAnalyticsPricingCoverage,
    CodeAnalyticsRange, CodeAnalyticsRepository, CodeAnalyticsSnapshot, CodeAnalyticsTotals,
};

const PRICES_AS_OF: &str = "2026-08-21";

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CodeAnalyticsQuery {
    range: Option<CodeAnalyticsRange>,
    repo_id: Option<RepoId>,
}

/// Read code activity, token usage, pull-request outcomes, and local cost
/// estimates for the authenticated owner.
pub(crate) async fn analytics(
    code: ScopedCode,
    Query(query): Query<CodeAnalyticsQuery>,
) -> Result<Json<CodeAnalyticsSnapshot>, ServerError> {
    let range = query.range.unwrap_or(CodeAnalyticsRange::ThirtyDays);
    let through = Utc::now();
    let from = range_start(range, through);
    let (repos, workspaces, sessions, turns, pull_requests, attributions) = tokio::try_join!(
        code.list_repos(),
        code.list_workspaces(None),
        code.list_sessions(),
        code.list_turn_metrics(),
        code.list_pull_request_facts(),
        code.list_pull_request_attributions(),
    )?;

    if let Some(repo_id) = query.repo_id {
        if !repos.iter().any(|repo| repo.id == repo_id) {
            return Err(ServerError::not_found(format!(
                "code repository {repo_id} not found"
            )));
        }
    }

    Ok(Json(build_snapshot(
        range,
        from,
        through,
        query.repo_id,
        repos,
        workspaces,
        sessions,
        turns,
        pull_requests,
        attributions,
    )))
}

#[allow(clippy::too_many_arguments)]
fn build_snapshot(
    range: CodeAnalyticsRange,
    from: Option<DateTime<Utc>>,
    through: DateTime<Utc>,
    repo_filter: Option<RepoId>,
    repos: Vec<tidebreak_core::CodeRepo>,
    workspaces: Vec<tidebreak_core::CodeWorkspace>,
    sessions: Vec<CodeSession>,
    turns: Vec<tidebreak_core::db::code::CodeTurnMetric>,
    pull_requests: Vec<tidebreak_core::CodePullRequestFact>,
    attributions: Vec<tidebreak_core::CodePullRequestAttribution>,
) -> CodeAnalyticsSnapshot {
    let workspace_repos: HashMap<WorkspaceId, RepoId> = workspaces
        .iter()
        .map(|workspace| (workspace.id, workspace.repo_id))
        .collect();
    let session_by_id: HashMap<CodeSessionId, &CodeSession> = sessions
        .iter()
        .filter(|session| {
            workspace_repos
                .get(&session.workspace_id)
                .is_some_and(|repo_id| repo_filter.is_none_or(|filter| filter == *repo_id))
        })
        .map(|session| (session.id, session))
        .collect();

    let mut totals = CodeAnalyticsTotals::default();
    let mut pricing = PricingAccumulator::default();
    let mut daily: BTreeMap<NaiveDate, DailyAccumulator> = BTreeMap::new();
    let mut repo_metrics: HashMap<RepoId, MetricsAccumulator> = repos
        .iter()
        .filter(|repo| repo_filter.is_none_or(|filter| filter == repo.id))
        .map(|repo| (repo.id, MetricsAccumulator::default()))
        .collect();
    let mut model_metrics: HashMap<ModelKey, ModelAccumulator> = HashMap::new();
    let mut harness_metrics: HashMap<HarnessKind, MetricsAccumulator> = HashMap::new();
    let mut active_sessions = HashSet::new();

    for session in session_by_id.values() {
        if in_range(session.created_at, from, through) {
            active_sessions.insert(session.id);
            let date = session.created_at.date_naive();
            daily.entry(date).or_default().sessions.insert(session.id);
        }
    }

    for turn in turns {
        let Some(session) = session_by_id.get(&turn.session_id).copied() else {
            continue;
        };
        if !in_range(turn.started_at, from, through) {
            continue;
        }
        let Some(repo_id) = workspace_repos.get(&session.workspace_id).copied() else {
            continue;
        };
        active_sessions.insert(session.id);
        let date = turn.started_at.date_naive();
        let day = daily.entry(date).or_default();
        day.sessions.insert(session.id);
        day.turns = day.turns.saturating_add(1);

        totals.turns = totals.turns.saturating_add(1);
        match turn.status {
            CodeTurnStatus::Completed => {
                totals.completed_turns = totals.completed_turns.saturating_add(1)
            }
            CodeTurnStatus::Failed => totals.failed_turns = totals.failed_turns.saturating_add(1),
            CodeTurnStatus::Interrupted => {
                totals.interrupted_turns = totals.interrupted_turns.saturating_add(1)
            }
            CodeTurnStatus::Running => {
                totals.running_turns = totals.running_turns.saturating_add(1)
            }
        }

        let usage = turn.usage.unwrap_or_default();
        let tokens = UsageTotals::from_usage(&usage);
        totals.input_tokens = totals.input_tokens.saturating_add(tokens.input);
        totals.output_tokens = totals.output_tokens.saturating_add(tokens.output);
        totals.cache_read_tokens = totals.cache_read_tokens.saturating_add(tokens.cache_read);
        totals.cache_write_tokens = totals.cache_write_tokens.saturating_add(tokens.cache_write);
        totals.total_tokens = totals.total_tokens.saturating_add(tokens.total);
        day.total_tokens = day.total_tokens.saturating_add(tokens.total);

        let canonical_model = turn.model.as_deref().and_then(canonical_model_id);
        let rate = (!turn.fast_mode)
            .then(|| canonical_model.and_then(price_for_canonical))
            .flatten();
        let cost_millimicrousd = rate
            .map(|rate| rate.cost_millimicrousd(tokens))
            .unwrap_or(0);
        pricing.record(tokens.total, cost_millimicrousd, rate.is_some());
        day.cost_millimicrousd = day.cost_millimicrousd.saturating_add(cost_millimicrousd);

        let repo = repo_metrics.entry(repo_id).or_default();
        repo.sessions.insert(session.id);
        repo.turns = repo.turns.saturating_add(1);
        repo.tokens = repo.tokens.saturating_add(tokens.total);
        repo.cost_millimicrousd = repo.cost_millimicrousd.saturating_add(cost_millimicrousd);

        let harness = harness_metrics.entry(session.harness_kind).or_default();
        harness.sessions.insert(session.id);
        harness.turns = harness.turns.saturating_add(1);
        harness.tokens = harness.tokens.saturating_add(tokens.total);
        harness.cost_millimicrousd = harness
            .cost_millimicrousd
            .saturating_add(cost_millimicrousd);

        let model_id = canonical_model
            .map(str::to_owned)
            .or_else(|| turn.model.clone());
        let model = model_metrics
            .entry(ModelKey {
                model_id,
                harness_kind: session.harness_kind,
                fast_mode: turn.fast_mode,
            })
            .or_default();
        model.sessions.insert(session.id);
        model.turns = model.turns.saturating_add(1);
        model.tokens = model.tokens.saturating_add(tokens.total);
        model.cost_millimicrousd = model.cost_millimicrousd.saturating_add(cost_millimicrousd);
        model.priced |= rate.is_some();
    }

    totals.sessions = as_u64(active_sessions.len());
    for session_id in active_sessions {
        let Some(session) = session_by_id.get(&session_id).copied() else {
            continue;
        };
        let Some(repo_id) = workspace_repos.get(&session.workspace_id).copied() else {
            continue;
        };
        repo_metrics
            .entry(repo_id)
            .or_default()
            .sessions
            .insert(session.id);
        harness_metrics
            .entry(session.harness_kind)
            .or_default()
            .sessions
            .insert(session.id);
    }

    let attributed_repos = attributed_repositories(&attributions, &workspace_repos);
    for pull_request in pull_requests {
        let Some(repo_ids) = attributed_repos.get(&pull_request.id) else {
            continue;
        };
        let matching_repos = repo_ids
            .iter()
            .copied()
            .filter(|repo_id| repo_filter.is_none_or(|filter| filter == *repo_id))
            .collect::<Vec<_>>();
        if matching_repos.is_empty() {
            continue;
        }
        if in_range(pull_request.created_at, from, through) {
            totals.pull_requests_opened = totals.pull_requests_opened.saturating_add(1);
            let day = daily
                .entry(pull_request.created_at.date_naive())
                .or_default();
            day.pull_requests_opened = day.pull_requests_opened.saturating_add(1);
            for repo_id in &matching_repos {
                let metrics = repo_metrics.entry(*repo_id).or_default();
                metrics.pull_requests_opened = metrics.pull_requests_opened.saturating_add(1);
            }
        }
        if let Some(merged_at) = pull_request.merged_at {
            if in_range(merged_at, from, through) {
                totals.pull_requests_merged = totals.pull_requests_merged.saturating_add(1);
                let day = daily.entry(merged_at.date_naive()).or_default();
                day.pull_requests_merged = day.pull_requests_merged.saturating_add(1);
                for repo_id in &matching_repos {
                    let metrics = repo_metrics.entry(*repo_id).or_default();
                    metrics.pull_requests_merged = metrics.pull_requests_merged.saturating_add(1);
                }
            }
        }
    }

    totals.estimated_cost_microusd = microusd(pricing.cost_millimicrousd);

    let mut repositories = repos
        .into_iter()
        .filter(|repo| repo_filter.is_none_or(|filter| filter == repo.id))
        .map(|repo| {
            let metrics = repo_metrics.remove(&repo.id).unwrap_or_default();
            CodeAnalyticsRepository {
                repo_id: repo.id,
                name: repo.display_name,
                sessions: as_u64(metrics.sessions.len()),
                turns: metrics.turns,
                total_tokens: metrics.tokens,
                estimated_cost_microusd: microusd(metrics.cost_millimicrousd),
                pull_requests_opened: metrics.pull_requests_opened,
                pull_requests_merged: metrics.pull_requests_merged,
            }
        })
        .collect::<Vec<_>>();
    repositories.sort_by(|left, right| {
        right
            .total_tokens
            .cmp(&left.total_tokens)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    let mut models = model_metrics
        .into_iter()
        .map(|(key, metrics)| CodeAnalyticsModel {
            model_id: key.model_id,
            harness_kind: key.harness_kind,
            fast_mode: key.fast_mode,
            sessions: as_u64(metrics.sessions.len()),
            turns: metrics.turns,
            total_tokens: metrics.tokens,
            estimated_cost_microusd: microusd(metrics.cost_millimicrousd),
            priced: metrics.priced,
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        right.total_tokens.cmp(&left.total_tokens).then_with(|| {
            left.model_id
                .as_deref()
                .unwrap_or("")
                .cmp(right.model_id.as_deref().unwrap_or(""))
        })
    });

    let mut harnesses = harness_metrics
        .into_iter()
        .map(|(harness_kind, metrics)| CodeAnalyticsHarness {
            harness_kind,
            sessions: as_u64(metrics.sessions.len()),
            turns: metrics.turns,
            total_tokens: metrics.tokens,
            estimated_cost_microusd: microusd(metrics.cost_millimicrousd),
        })
        .collect::<Vec<_>>();
    harnesses.sort_by_key(|item| item.harness_kind.as_str());

    let daily = daily_rows(daily, from, through);
    CodeAnalyticsSnapshot {
        range,
        from,
        through,
        repo_id: repo_filter,
        totals,
        daily,
        repositories,
        models,
        harnesses,
        pricing: CodeAnalyticsPricingCoverage {
            priced_turns: pricing.priced_turns,
            unpriced_turns: pricing.unpriced_turns,
            priced_tokens: pricing.priced_tokens,
            unpriced_tokens: pricing.unpriced_tokens,
            prices_as_of: PRICES_AS_OF.to_owned(),
        },
    }
}

fn attributed_repositories(
    attributions: &[tidebreak_core::CodePullRequestAttribution],
    workspace_repos: &HashMap<WorkspaceId, RepoId>,
) -> HashMap<CodePullRequestId, HashSet<RepoId>> {
    let mut result: HashMap<CodePullRequestId, HashSet<RepoId>> = HashMap::new();
    for attribution in attributions {
        if let Some(repo_id) = workspace_repos.get(&attribution.workspace_id) {
            result
                .entry(attribution.pull_request_id)
                .or_default()
                .insert(*repo_id);
        }
    }
    result
}

fn daily_rows(
    mut days: BTreeMap<NaiveDate, DailyAccumulator>,
    from: Option<DateTime<Utc>>,
    through: DateTime<Utc>,
) -> Vec<CodeAnalyticsDay> {
    let start = from
        .map(|value| value.date_naive())
        .or_else(|| days.keys().next().copied())
        .unwrap_or_else(|| through.date_naive());
    let end = through.date_naive();
    let mut rows = Vec::new();
    let mut date = start;
    while date <= end {
        let day = days.remove(&date).unwrap_or_default();
        rows.push(CodeAnalyticsDay {
            date: date.format("%Y-%m-%d").to_string(),
            sessions: as_u64(day.sessions.len()),
            turns: day.turns,
            total_tokens: day.total_tokens,
            estimated_cost_microusd: microusd(day.cost_millimicrousd),
            pull_requests_opened: day.pull_requests_opened,
            pull_requests_merged: day.pull_requests_merged,
        });
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    rows
}

fn range_start(range: CodeAnalyticsRange, through: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let days = match range {
        CodeAnalyticsRange::SevenDays => 7,
        CodeAnalyticsRange::ThirtyDays => 30,
        CodeAnalyticsRange::NinetyDays => 90,
        CodeAnalyticsRange::All => return None,
    };
    Some(through - Duration::days(days - 1))
}

fn in_range(at: DateTime<Utc>, from: Option<DateTime<Utc>>, through: DateTime<Utc>) -> bool {
    from.is_none_or(|start| at >= start) && at <= through
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageTotals {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total: u64,
}

impl UsageTotals {
    fn from_usage(usage: &tidebreak_core::CodeUsage) -> Self {
        let total = usage
            .input_tokens
            .saturating_add(usage.output_tokens)
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
        Self {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
            total,
        }
    }
}

/// Price in thousandths of a microdollar per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriceRate {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl PriceRate {
    fn cost_millimicrousd(self, usage: UsageTotals) -> u128 {
        u128::from(usage.input)
            .saturating_mul(u128::from(self.input))
            .saturating_add(u128::from(usage.output).saturating_mul(u128::from(self.output)))
            .saturating_add(
                u128::from(usage.cache_read).saturating_mul(u128::from(self.cache_read)),
            )
            .saturating_add(
                u128::from(usage.cache_write).saturating_mul(u128::from(self.cache_write)),
            )
    }
}

fn canonical_model_id(model: &str) -> Option<&'static str> {
    let model = model.trim().to_ascii_lowercase();
    if model_matches(&model, "gpt-5.6-sol") || model_matches(&model, "gpt-5.6") {
        return Some("gpt-5.6-sol");
    }
    for id in [
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "claude-fable-5",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-4-5",
    ] {
        if model_matches(&model, id) || model_has_dated_suffix(&model, id) {
            return Some(id);
        }
    }
    None
}

fn model_matches(value: &str, id: &str) -> bool {
    value == id
        || value.ends_with(&format!("::{id}"))
        || value.ends_with(&format!("/{id}"))
        || value.ends_with(&format!(".{id}"))
        || value.ends_with(&format!("-{id}"))
}

fn model_has_dated_suffix(value: &str, id: &str) -> bool {
    let Some(at) = value.rfind(id) else {
        return false;
    };
    let suffix = &value[at + id.len()..];
    suffix
        .strip_prefix('-')
        .is_some_and(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
}

fn price_for_canonical(model: &str) -> Option<PriceRate> {
    match model {
        "gpt-5.6-sol" => Some(PriceRate {
            input: 5_000,
            output: 30_000,
            cache_read: 500,
            cache_write: 0,
        }),
        "gpt-5.6-terra" => Some(PriceRate {
            input: 2_000,
            output: 12_000,
            cache_read: 200,
            cache_write: 0,
        }),
        "gpt-5.6-luna" => Some(PriceRate {
            input: 200,
            output: 1_200,
            cache_read: 20,
            cache_write: 0,
        }),
        "claude-fable-5" => Some(PriceRate {
            input: 10_000,
            output: 50_000,
            cache_read: 1_000,
            cache_write: 12_500,
        }),
        "claude-opus-5" => Some(PriceRate {
            input: 5_000,
            output: 25_000,
            cache_read: 500,
            cache_write: 6_250,
        }),
        "claude-sonnet-5" => Some(PriceRate {
            input: 2_000,
            output: 10_000,
            cache_read: 200,
            cache_write: 2_500,
        }),
        "claude-haiku-4-5" => Some(PriceRate {
            input: 1_000,
            output: 5_000,
            cache_read: 100,
            cache_write: 1_250,
        }),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct DailyAccumulator {
    sessions: HashSet<CodeSessionId>,
    turns: u64,
    total_tokens: u64,
    cost_millimicrousd: u128,
    pull_requests_opened: u64,
    pull_requests_merged: u64,
}

#[derive(Debug, Default)]
struct MetricsAccumulator {
    sessions: HashSet<CodeSessionId>,
    turns: u64,
    tokens: u64,
    cost_millimicrousd: u128,
    pull_requests_opened: u64,
    pull_requests_merged: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModelKey {
    model_id: Option<String>,
    harness_kind: HarnessKind,
    fast_mode: bool,
}

#[derive(Debug, Default)]
struct ModelAccumulator {
    sessions: HashSet<CodeSessionId>,
    turns: u64,
    tokens: u64,
    cost_millimicrousd: u128,
    priced: bool,
}

#[derive(Debug, Default)]
struct PricingAccumulator {
    priced_turns: u64,
    unpriced_turns: u64,
    priced_tokens: u64,
    unpriced_tokens: u64,
    cost_millimicrousd: u128,
}

impl PricingAccumulator {
    fn record(&mut self, tokens: u64, cost_millimicrousd: u128, priced: bool) {
        if tokens == 0 {
            return;
        }
        if priced {
            self.priced_turns = self.priced_turns.saturating_add(1);
            self.priced_tokens = self.priced_tokens.saturating_add(tokens);
            self.cost_millimicrousd = self.cost_millimicrousd.saturating_add(cost_millimicrousd);
        } else {
            self.unpriced_turns = self.unpriced_turns.saturating_add(1);
            self.unpriced_tokens = self.unpriced_tokens.saturating_add(tokens);
        }
    }
}

fn microusd(millimicrousd: u128) -> u64 {
    let rounded = millimicrousd.saturating_add(500) / 1_000;
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_prices_cover_curated_gateway_ids() {
        assert_eq!(
            canonical_model_id("model_gateway::gpt-5.6"),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            canonical_model_id("anthropic-us-claude-opus-5"),
            Some("claude-opus-5")
        );
        assert_eq!(
            canonical_model_id("claude-sonnet-5-20260801"),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            canonical_model_id("accounts/fireworks/models/glm-5p2"),
            None
        );
    }

    #[test]
    fn price_math_uses_integer_microdollars() {
        let rate = price_for_canonical("claude-sonnet-5").unwrap();
        let cost = rate.cost_millimicrousd(UsageTotals {
            input: 1_000_000,
            output: 100_000,
            cache_read: 500_000,
            cache_write: 10_000,
            total: 1_610_000,
        });
        assert_eq!(microusd(cost), 3_125_000);
    }
}
