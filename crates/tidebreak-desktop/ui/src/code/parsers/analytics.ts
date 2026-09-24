import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeAnalyticsDay,
  CodeAnalyticsHarness,
  CodeAnalyticsModel,
  CodeAnalyticsPricingCoverage,
  CodeAnalyticsRange,
  CodeAnalyticsRepository,
  CodeAnalyticsSnapshot,
  CodeAnalyticsTotals,
  CodeSubscriptionUsage,
} from "../../api/types";
import type {
  CodeAnalyticsDay as WireCodeAnalyticsDay,
  CodeAnalyticsHarness as WireCodeAnalyticsHarness,
  CodeAnalyticsModel as WireCodeAnalyticsModel,
  CodeAnalyticsPricingCoverage as WireCodeAnalyticsPricingCoverage,
  CodeAnalyticsRepository as WireCodeAnalyticsRepository,
  CodeAnalyticsSnapshot as WireCodeAnalyticsSnapshot,
  CodeAnalyticsTotals as WireCodeAnalyticsTotals,
} from "../../generated/wire";
import {
  nonEmptyLine,
  optionalLine,
  wireId,
  optionalWireId,
  nullableWireId,
  timestamp,
  optionalTimestamp,
  HARNESS_KINDS,
} from "./shared";

const USAGE_SOURCES = new Set<CodeSubscriptionUsage["source"]>([
  "model_gateway",
  "direct",
  "unavailable",
]);
const ANALYTICS_RANGES = new Set<CodeAnalyticsRange>([
  "7d",
  "30d",
  "90d",
  "all",
]);

export function parseCodeSubscriptionUsage(
  value: unknown,
): CodeSubscriptionUsage | null {
  if (
    !isRecord(value) ||
    !isMember(value.source, USAGE_SOURCES) ||
    !Array.isArray(value.providers)
  ) {
    return null;
  }
  const providers: CodeSubscriptionUsage["providers"] = [];
  for (const provider of value.providers) {
    if (
      !isRecord(provider) ||
      !wireId(provider.id) ||
      !nonEmptyLine(provider.label) ||
      !Array.isArray(provider.accounts)
    ) {
      return null;
    }
    const accounts = [];
    for (const account of provider.accounts) {
      if (
        !isRecord(account) ||
        !wireId(account.id) ||
        !nonEmptyLine(account.label) ||
        typeof account.is_own !== "boolean" ||
        !Array.isArray(account.windows)
      ) {
        return null;
      }
      const windows = [];
      for (const window of account.windows) {
        if (
          !isRecord(window) ||
          !wireId(window.key) ||
          !nonEmptyLine(window.label) ||
          !isFiniteNumber(window.used_percent)
        ) {
          return null;
        }
        windows.push({
          key: window.key,
          label: window.label,
          used_percent: window.used_percent,
        });
      }
      accounts.push({
        id: account.id,
        label: account.label,
        is_own: account.is_own,
        windows,
      });
    }
    providers.push({ id: provider.id, label: provider.label, accounts });
  }
  return {
    source: value.source,
    providers,
  };
}

function parseCodeAnalyticsTotals(value: unknown): CodeAnalyticsTotals | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsTotals>(value, [
      "sessions",
      "turns",
      "completed_turns",
      "failed_turns",
      "interrupted_turns",
      "running_turns",
      "input_tokens",
      "output_tokens",
      "cache_read_tokens",
      "cache_write_tokens",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.completed_turns) ||
    !isNonNegativeInteger(value.failed_turns) ||
    !isNonNegativeInteger(value.interrupted_turns) ||
    !isNonNegativeInteger(value.running_turns) ||
    !isNonNegativeInteger(value.input_tokens) ||
    !isNonNegativeInteger(value.output_tokens) ||
    !isNonNegativeInteger(value.cache_read_tokens) ||
    !isNonNegativeInteger(value.cache_write_tokens) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsTotals;
}

function parseCodeAnalyticsDay(value: unknown): CodeAnalyticsDay | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsDay>(value, [
      "date",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !timestamp(value.date) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsDay;
}

function parseCodeAnalyticsRepository(
  value: unknown,
): CodeAnalyticsRepository | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsRepository>(value, [
      "repo_id",
      "name",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !nullableWireId(value.repo_id) ||
    !nonEmptyLine(value.name) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsRepository;
}

function parseCodeAnalyticsModel(value: unknown): CodeAnalyticsModel | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsModel>(value, [
      "model_id",
      "harness_kind",
      "fast_mode",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "priced",
    ]) ||
    !optionalLine(value.model_id) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    typeof value.fast_mode !== "boolean" ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    typeof value.priced !== "boolean"
  ) {
    return null;
  }
  return value as CodeAnalyticsModel;
}

function parseCodeAnalyticsHarness(
  value: unknown,
): CodeAnalyticsHarness | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsHarness>(value, [
      "harness_kind",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
    ]) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd)
  ) {
    return null;
  }
  return value as CodeAnalyticsHarness;
}

function parseCodeAnalyticsPricing(
  value: unknown,
): CodeAnalyticsPricingCoverage | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsPricingCoverage>(value, [
      "priced_turns",
      "unpriced_turns",
      "priced_tokens",
      "unpriced_tokens",
      "prices_as_of",
    ]) ||
    !isNonNegativeInteger(value.priced_turns) ||
    !isNonNegativeInteger(value.unpriced_turns) ||
    !isNonNegativeInteger(value.priced_tokens) ||
    !isNonNegativeInteger(value.unpriced_tokens) ||
    !timestamp(value.prices_as_of)
  ) {
    return null;
  }
  return value as CodeAnalyticsPricingCoverage;
}

export function parseCodeAnalytics(
  value: unknown,
): CodeAnalyticsSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsSnapshot>(value, [
      "range",
      "from",
      "through",
      "repo_id",
      "totals",
      "daily",
      "repositories",
      "models",
      "harnesses",
      "pricing",
    ]) ||
    !isMember(value.range, ANALYTICS_RANGES) ||
    !optionalTimestamp(value.from) ||
    !timestamp(value.through) ||
    !optionalWireId(value.repo_id) ||
    !Array.isArray(value.daily) ||
    !Array.isArray(value.repositories) ||
    !Array.isArray(value.models) ||
    !Array.isArray(value.harnesses)
  ) {
    return null;
  }
  const totals = parseCodeAnalyticsTotals(value.totals);
  const pricing = parseCodeAnalyticsPricing(value.pricing);
  const daily = value.daily.map(parseCodeAnalyticsDay);
  const repositories = value.repositories.map(parseCodeAnalyticsRepository);
  const models = value.models.map(parseCodeAnalyticsModel);
  const harnesses = value.harnesses.map(parseCodeAnalyticsHarness);
  if (
    !totals ||
    !pricing ||
    daily.some((item) => item === null) ||
    repositories.some((item) => item === null) ||
    models.some((item) => item === null) ||
    harnesses.some((item) => item === null)
  ) {
    return null;
  }
  return {
    range: value.range,
    through: value.through,
    totals,
    daily: daily as CodeAnalyticsDay[],
    repositories: repositories as CodeAnalyticsRepository[],
    models: models as CodeAnalyticsModel[],
    harnesses: harnesses as CodeAnalyticsHarness[],
    pricing,
    ...(value.from !== undefined ? { from: value.from } : {}),
    ...(value.repo_id !== undefined ? { repo_id: value.repo_id } : {}),
  };
}
