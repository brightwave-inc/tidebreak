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
 * Whether a pull request in this state may be merged, or auto-merge armed.
 *
 * One table for every surface that offers merging. Decision 42 makes merging a
 * user action rather than an agent capability, so the answer to "may this
 * merge" has to be the same whether the reader clicks the review sidebar's
 * button, the workspace header's, or presses the chord — a second copy would
 * eventually let one of them offer a merge the others refuse.
 *
 * `explanation` is the sentence to show when the answer is no. `null` means
 * the pull request is either mergeable or already resolved, and there is
 * nothing to explain.
 */
export function prMergeControls(state: PrWorkflowState): {
  canMerge: boolean;
  canEnableAutoMerge: boolean;
  explanation: string | null;
} {
  switch (state) {
    case "ready":
      return { canMerge: true, canEnableAutoMerge: true, explanation: null };
    case "checking":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation:
          "GitHub is still determining mergeability. Merge stays unavailable until the pull request is explicitly ready.",
      };
    case "pending":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation: "Wait for the pending checks before merging directly.",
      };
    case "failing":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation: "Fix the failing checks before merging directly.",
      };
    case "conflict":
      return {
        canMerge: false,
        canEnableAutoMerge: false,
        explanation: "Resolve the merge conflicts before merging directly.",
      };
    case "behind":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation: "Update the branch from its base before merging directly.",
      };
    case "blocked":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation:
          "A review or repository requirement is still blocking a direct merge.",
      };
    case "changes_requested":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation: "Address the requested changes before merging directly.",
      };
    case "draft":
      return {
        canMerge: false,
        canEnableAutoMerge: false,
        explanation:
          "Mark the pull request ready for review on GitHub before merging it.",
      };
    case "queued":
      return {
        canMerge: false,
        canEnableAutoMerge: false,
        explanation: "This pull request is already waiting in the merge queue.",
      };
    case "auto_merge":
      return {
        canMerge: false,
        canEnableAutoMerge: false,
        explanation:
          "Auto-merge is already enabled and will merge after the remaining requirements pass.",
      };
    case "merged":
    case "closed":
      return {
        canMerge: false,
        canEnableAutoMerge: false,
        explanation: null,
      };
  }
}

/** The workflow actions an agent carries out, as opposed to an endpoint. */
export type PrPromptAction = Exclude<
  PrWorkflowAction,
  "watch_and_fix" | "mark_ready" | "merge"
>;

/**
 * Prompt actions run in the workspace's interactive session.
 *
 * Three of the workflow actions are deliberately absent. "Watch and fix"
 * starts a durable server-side watch task (`POST
 * /code/workspaces/{id}/watch`). Merging and readying a draft are
 * pull-request state changes, which decision 42 reserves for the user: they
 * run through their own endpoints, and excluding them from this type is what
 * stops a merge prompt from being wired back up by accident.
 */
export function prWorkflowPrompt(
  action: PrPromptAction,
  pr: PullRequestDigest,
): string {
  const number = `#${pr.number}`;
  const base = pr.base_branch?.trim() || "the base branch";
  const context = prWorkflowPromptContext(pr);
  let instruction: string;
  switch (action) {
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
