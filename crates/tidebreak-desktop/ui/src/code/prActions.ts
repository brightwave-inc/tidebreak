import type { PullRequestDigest } from "../api/types";
import { prTone } from "./workspaceCards";

export type PrWorkflowAction =
  | "watch_and_fix"
  | "mark_ready"
  | "merge"
  | "fix_errors"
  | "address_feedback"
  | "update_branch"
  | "resolve_conflicts";

export type PrWorkflowState =
  | "checking"
  | "ready"
  | "pending"
  | "failing"
  | "conflict"
  | "behind"
  | "blocked"
  | "changes_requested"
  | "draft"
  | "queued"
  | "auto_merge"
  | "merged"
  | "closed";

export type PrCheckCounts = {
  passing: number;
  pending: number;
  failing: number;
  skipped: number;
  total: number;
};

export type PrWorkflowStatus = {
  state: PrWorkflowState;
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

/** Only an explicit host-timeline signal counts as merge-queue membership. */
export function prIsQueued(pr: PullRequestDigest): boolean {
  return pr.in_merge_queue === true;
}

export function prIsBehind(pr: PullRequestDigest): boolean {
  return pr.merge_state_status?.trim().toLowerCase() === "behind";
}

export function prIsBlocked(pr: PullRequestDigest): boolean {
  const mergeState = pr.merge_state_status?.trim().toLowerCase();
  const review = pr.review_decision?.trim().toLowerCase();
  return (
    mergeState === "blocked" ||
    mergeState === "unstable" ||
    review === "review_required"
  );
}

export function prHasChangesRequested(pr: PullRequestDigest): boolean {
  return pr.review_decision?.trim().toLowerCase() === "changes_requested";
}

/** Classify a live pull request without choosing surface-specific actions. */
export function prWorkflowStatus(pr: PullRequestDigest): PrWorkflowStatus {
  const checks = prCheckCounts(pr);
  return { state: workflowState(pr, checks), checks };
}

/**
 * Prompt actions run in the workspace's interactive session. "Watch and fix"
 * is not one of them: it starts a durable server-side watch task instead
 * (`POST /code/workspaces/{id}/watch`).
 */
export function prWorkflowPrompt(
  action: Exclude<PrWorkflowAction, "watch_and_fix">,
  pr: PullRequestDigest,
): string {
  const number = `#${pr.number}`;
  const base = pr.base_branch?.trim() || "the base branch";
  const context = prWorkflowPromptContext(pr);
  let instruction: string;
  switch (action) {
    case "mark_ready":
      instruction = `Mark pull request ${number} ready for review. First confirm the current head is pushed and the pull request is still a draft. Do not merge it. Report the result.`;
      break;
    case "merge":
      instruction = `Merge pull request ${number} into ${base}. Re-check the current head SHA, required checks, reviews, conflicts, and queue requirements immediately before merging. Use the repository's preferred merge or merge-queue path. Do not change the branch unless the merge requires it. Report the result.`;
      break;
    case "fix_errors":
      instruction = `Pull request ${number} has failing checks. Inspect the latest failing CI logs for the current head SHA, reproduce the cause when practical, make the smallest safe fix in this workspace, run focused validation, commit, and push. Do not merge.`;
      break;
    case "address_feedback":
      instruction = `Pull request ${number} has requested changes. Inspect the latest unresolved review feedback, implement each actionable request in this workspace, run focused validation, commit, push, and reply where context is useful. Do not merge.`;
      break;
    case "update_branch":
      instruction = `Update pull request ${number} from ${base}. Fetch the latest base branch, rebase this workspace branch onto it, resolve any conflicts, run focused validation, and push the updated head. Do not merge.`;
      break;
    case "resolve_conflicts":
      instruction = `Pull request ${number} has merge conflicts with ${base}. Fetch and rebase onto ${base}, resolve every conflict in this workspace, run focused validation, commit if needed, and push the updated head. Do not merge the pull request.`;
      break;
  }
  return `${instruction}\n\n${context}`;
}

function prWorkflowPromptContext(pr: PullRequestDigest): string {
  const title = pr.title?.trim();
  const url = pr.url?.trim();
  const head = pr.head_branch?.trim() || "current workspace branch";
  const base = pr.base_branch?.trim() || "base branch";
  const checks = prCheckCounts(pr);
  const state = [
    pr.draft ? "draft" : pr.state.trim() || "open",
    pr.mergeable ? `mergeable: ${pr.mergeable}` : null,
    pr.merge_state_status ? `merge state: ${pr.merge_state_status}` : null,
    pr.review_decision ? `review: ${pr.review_decision}` : null,
    pr.auto_merge_enabled ? "auto-merge: on" : null,
    pr.in_merge_queue ? "merge queue: queued" : null,
  ].filter((value): value is string => Boolean(value));
  const lines = [
    `Pull request: #${pr.number}${title ? ` - ${title}` : ""}`,
    ...(url ? [`URL: ${url}`] : []),
    `Branch: ${head} -> ${base}`,
    `GitHub state: ${state.join(", ")}`,
    ...(checks.total > 0
      ? [
          `Checks: ${checks.passing} passing, ${checks.pending} pending, ${checks.failing} failing, ${checks.skipped} skipped`,
        ]
      : []),
  ];
  const activeChecks =
    pr.checks?.filter(
      (check) => check.bucket === "fail" || check.bucket === "pending",
    ) ?? [];
  if (activeChecks.length > 0) {
    lines.push(
      "Relevant checks:",
      ...activeChecks.map((check) => {
        const detail = check.detail?.trim();
        const checkUrl = check.url?.trim();
        return `- ${check.name} (${check.bucket === "fail" ? "failed" : "pending"})${detail ? `: ${detail}` : ""}${checkUrl ? `\n  ${checkUrl}` : ""}`;
      }),
    );
  }
  return lines.join("\n");
}

function workflowState(
  pr: PullRequestDigest,
  checks: PrCheckCounts,
): PrWorkflowState {
  const chip = prTone(pr);
  if (chip === "merged") return "merged";
  if (chip === "closed") return "closed";
  if (chip === "draft") return "draft";
  if (prHasConflicts(pr)) return "conflict";
  if (prHasChangesRequested(pr)) return "changes_requested";
  if (checks.failing > 0) return "failing";
  if (prIsQueued(pr)) return "queued";
  if (prIsBehind(pr)) return "behind";
  if (prIsBlocked(pr)) return "blocked";
  if (pr.auto_merge_enabled) return "auto_merge";
  if (checks.pending > 0) return "pending";
  const mergeable = pr.mergeable?.trim().toLowerCase();
  const mergeState = pr.merge_state_status?.trim().toLowerCase();
  return mergeable === "mergeable" && mergeState === "clean"
    ? "ready"
    : "checking";
}
