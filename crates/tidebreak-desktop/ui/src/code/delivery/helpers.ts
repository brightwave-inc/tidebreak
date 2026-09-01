import {
  type CodeDeliveryPrViewFilters,
  codeDeliveryRepositoryKey,
  codeDeliveryRepositoryTarget,
  type CodeDeliveryRunViewFilters,
} from "../CodeDeliveryStore";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
  CodeDeliveryRunSummary,
  CodeDeliveryRunTarget,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
} from "../../api/types";
import type { PrBuiltInView, PullRequestGrouping } from "./views";
import {
  groupedPullRequestRows,
  type PullRequestListItem,
} from "./PullRequestList";

export function runBucket(
  conclusion: string | undefined,
  status: string,
): "pass" | "pending" | "fail" | "skipped" {
  if (conclusion === "success") return "pass";
  if (
    conclusion === "failure" ||
    conclusion === "timed_out" ||
    conclusion === "action_required" ||
    conclusion === "startup_failure"
  ) {
    return "fail";
  }
  if (status === "queued" || status === "in_progress" || status === "pending") {
    return "pending";
  }
  return "skipped";
}

export function runTone(
  value: string,
): "success" | "critical" | "warning" | "muted" {
  if (value === "success") return "success";
  if (
    value === "failure" ||
    value === "timed_out" ||
    value === "action_required" ||
    value === "startup_failure" ||
    value === "error"
  ) {
    return "critical";
  }
  if (value === "queued" || value === "in_progress" || value === "pending") {
    return "warning";
  }
  return "muted";
}

export function selectedRepositoryTargets(
  repositories: CodeGitHubRepositoryRef[],
  selected: string[],
  required?: CodeGitHubRepositoryTarget,
): CodeGitHubRepositoryTarget[] {
  const keys = new Set(selected);
  const targets = repositories
    .filter(
      (repository) =>
        keys.size === 0 || keys.has(codeDeliveryRepositoryKey(repository)),
    )
    .map(codeDeliveryRepositoryTarget);
  if (
    required &&
    !targets.some(
      (target) =>
        codeDeliveryRepositoryKey(target) ===
        codeDeliveryRepositoryKey(required),
    )
  ) {
    targets.push(required);
  }
  return targets;
}

export function pullRequestIdsInDisplayOrder(
  items: readonly CodeDeliveryPullRequestSummary[],
  grouping: PullRequestGrouping,
): string[] {
  return groupedPullRequestRows(items, grouping)
    .filter(
      (row): row is Extract<PullRequestListItem, { kind: "pull_request" }> =>
        row.kind === "pull_request",
    )
    .map((row) => row.row.item.id);
}

export function isEditableKeyboardTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

export function pullRequestMatchesTarget(
  item: CodeDeliveryPullRequestSummary,
  target: CodeDeliveryPullRequestTarget,
): boolean {
  return (
    item.number === target.number &&
    codeDeliveryRepositoryKey(item.repository) ===
      codeDeliveryRepositoryKey(target.repository)
  );
}

export function runMatchesTarget(
  item: CodeDeliveryRunSummary,
  target: CodeDeliveryRunTarget,
): boolean {
  return (
    item.kind === target.kind &&
    item.github_id === target.id &&
    codeDeliveryRepositoryKey(item.repository) ===
      codeDeliveryRepositoryKey(target.repository)
  );
}

export function positiveSearchInteger(value: unknown): number | undefined {
  const parsed =
    typeof value === "number"
      ? value
      : typeof value === "string" && value.trim() !== ""
        ? Number(value)
        : Number.NaN;
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined;
}

export function clonePrFilters(
  filters: CodeDeliveryPrViewFilters,
): CodeDeliveryPrViewFilters {
  return {
    ...filters,
    repositoryKeys: [...filters.repositoryKeys],
    states: [...filters.states],
    reviewStates: [...filters.reviewStates],
    checkStates: [...filters.checkStates],
    authors: [...filters.authors],
  };
}

/**
 * A built-in view's filters, with the viewer view pointed at the signed-in
 * login. Resolving to a plain `authors` entry is what keeps the author chip,
 * the filter count, and a saved copy of the view all reading the same thing.
 */
export function builtInPrFilters(
  view: PrBuiltInView,
  viewerLogin: string | undefined,
): CodeDeliveryPrViewFilters {
  const filters = clonePrFilters(view.filters);
  if (view.viewerAuthored && viewerLogin) filters.authors = [viewerLogin];
  return filters;
}

export function cloneRunFilters(
  filters: CodeDeliveryRunViewFilters,
): CodeDeliveryRunViewFilters {
  return {
    ...filters,
    repositoryKeys: [...filters.repositoryKeys],
    kinds: [...filters.kinds],
    statuses: [...filters.statuses],
    conclusions: [...filters.conclusions],
    workflows: [...filters.workflows],
    environments: [...filters.environments],
    branches: [...filters.branches],
    events: [...filters.events],
    actors: [...filters.actors],
  };
}

export function toggleValue<T>(values: T[], value: T, enabled: boolean): T[] {
  if (enabled) return values.includes(value) ? values : [...values, value];
  return values.filter((candidate) => candidate !== value);
}

export function commaList(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

/**
 * A wire token as a label: `timed_out` reads "Timed out".
 *
 * Sentence case, not title case. The repository writes UI text in sentence
 * case, and title-casing turned "review pending" into "Review Pending", which
 * read like a proper noun rather than a state.
 */
export function humanize(value: string): string {
  const words = value.replaceAll("_", " ").trim();
  if (!words) return words;
  return words[0]!.toUpperCase() + words.slice(1);
}

export function dedupeRows<T extends { id: string }>(items: T[]): T[] {
  return [...new Map(items.map((item) => [item.id, item])).values()];
}
