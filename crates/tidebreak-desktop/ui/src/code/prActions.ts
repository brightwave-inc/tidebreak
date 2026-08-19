import type { PullRequestDigest } from "../api/types";
import { prTone } from "./workspaceCards";

export type PrBarAction =
  | "watch_and_fix"
  | "merge"
  | "fix_errors"
  | "resolve_conflicts";

export type PrBarTone =
  | "ready"
  | "pending"
  | "failing"
  | "conflict"
  | "draft"
  | "merged"
  | "closed";

export type PrCheckCounts = {
  passing: number;
  pending: number;
  failing: number;
  skipped: number;
  total: number;
};

export type PrBarModel = {
  number: number;
  url?: string | null;
  status: string;
  tone: PrBarTone;
  actions: PrBarAction[];
  checks: PrCheckCounts;
};

/** Counts from individual checks when present, else the one-line summary. */
export function prCheckCounts(pr: PullRequestDigest): PrCheckCounts {
  const checks = pr.checks ?? [];
  if (checks.length > 0) {
    const passing = checks.filter((check) => check.bucket === "pass").length;
    const pending = checks.filter((check) => check.bucket === "pending").length;
    const failing = checks.filter((check) => check.bucket === "fail").length;
    const skipped = checks.filter((check) => check.bucket === "skipped").length;
    return { passing, pending, failing, skipped, total: checks.length };
  }
  const summary = pr.checks_summary ?? "";
  const passing = Number(/(\d+) passing/.exec(summary)?.[1] ?? 0);
  const pending = Number(/(\d+) pending/.exec(summary)?.[1] ?? 0);
  const failing = Number(/(\d+) failing/.exec(summary)?.[1] ?? 0);
  const skipped = Number(/(\d+) skipped/.exec(summary)?.[1] ?? 0);
  return {
    passing,
    pending,
    failing,
    skipped,
    total: passing + pending + failing + skipped,
  };
}

export function prHasConflicts(pr: PullRequestDigest): boolean {
  const mergeable = pr.mergeable?.toLowerCase();
  const mergeState = pr.merge_state_status?.toLowerCase();
  return mergeable === "conflicting" || mergeState === "dirty";
}

/**
 * What the inspector bar shows for a live PR: one status phrase, the check
 * count, and the actions that insert a prompt.
 */
export function prBarModel(pr: PullRequestDigest): PrBarModel {
  const checks = prCheckCounts(pr);
  const tone = barTone(pr, checks);
  return {
    number: pr.number,
    url: pr.url,
    status: barStatus(tone, checks),
    tone,
    actions: barActions(tone),
    checks,
  };
}

export function prBarPrompt(
  action: PrBarAction,
  pr: PullRequestDigest,
): string {
  const number = `#${pr.number}`;
  const base = pr.base_branch?.trim() || "the base branch";
  switch (action) {
    case "watch_and_fix":
      return `Watch pull request ${number} until it is mergeable. This watch runs in the current task, so stay on this workspace and branch. On every cycle, associate checks with the current head SHA and inspect required checks, actionable review feedback, draft status, mergeability, and whether the branch is behind ${base}. Wait efficiently while work is pending. When a check fails, a reviewer requests a concrete change, conflicts appear, or the branch must be updated, diagnose it, make the smallest safe fix, fetch and rebase onto ${base} when needed, resolve conflicts, run focused validation, commit if needed, push, and keep watching against the new head SHA. If the pull request is a draft, mark it ready for review before enabling auto-merge when that action is authorized; otherwise report the required authorization as the blocker. Enable auto-merge once required checks and automated reviews are green. Do not stop merely because checks are pending. Stop only when the pull request is merged or when a required human approval, missing permission, external outage, or product decision blocks progress; in that case report the exact blocker.`;
    case "merge":
      return `Merge pull request ${number} into ${base}. Use this workspace's existing merge path. Do not change the branch unless the merge requires it. Report the result.`;
    case "fix_errors":
      return `Pull request ${number} has failing checks. Inspect the failing CI, fix the cause in this workspace, and push. Do not merge.`;
    case "resolve_conflicts":
      return `Pull request ${number} has merge conflicts with ${base}. Rebase or merge ${base}, resolve every conflict in this workspace, and push. Do not merge the pull request.`;
  }
}

export function prBarActionLabel(action: PrBarAction): string {
  switch (action) {
    case "watch_and_fix":
      return "Watch and fix";
    case "merge":
      return "Merge";
    case "fix_errors":
      return "Fix errors";
    case "resolve_conflicts":
      return "Resolve conflicts";
  }
}

function barTone(pr: PullRequestDigest, checks: PrCheckCounts): PrBarTone {
  const chip = prTone(pr);
  if (chip === "merged") return "merged";
  if (chip === "closed") return "closed";
  if (prHasConflicts(pr)) return "conflict";
  if (checks.failing > 0) return "failing";
  if (chip === "draft") return "draft";
  if (checks.pending > 0) return "pending";
  return "ready";
}

function barStatus(tone: PrBarTone, checks: PrCheckCounts): string {
  switch (tone) {
    case "ready":
      return "Ready to merge";
    case "pending":
      return checks.pending === 1
        ? "1 check pending"
        : `${checks.pending} checks pending`;
    case "failing":
      return checks.failing === 1
        ? "1 check failing"
        : `${checks.failing} checks failing`;
    case "conflict":
      return "Conflicts";
    case "draft":
      return "Draft";
    case "merged":
      return "Merged";
    case "closed":
      return "Closed";
  }
}

function barActions(tone: PrBarTone): PrBarAction[] {
  if (tone === "merged" || tone === "closed") return [];
  const contextual =
    tone === "conflict"
      ? "resolve_conflicts"
      : tone === "failing"
        ? "fix_errors"
        : "merge";
  return [
    "watch_and_fix",
    contextual,
    ...(["merge", "fix_errors", "resolve_conflicts"] as const).filter(
      (action) => action !== contextual,
    ),
  ];
}
