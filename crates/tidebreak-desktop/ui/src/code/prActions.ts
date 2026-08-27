import type {
  CodeCheckLog,
  CodeDeliveryPullRequestSummary,
  PullRequestDigest,
} from "../api/types";
import {
  checkCounts,
  prHasChangesRequested,
  prHasConflicts,
  prIsBehind,
  pullRequestLifecycle,
  prStatus,
  type CheckCounts,
  type PrGate,
} from "./prState";
import { renderWorkflowPrompt } from "./workflowPrompts";

export type PrWorkflowAction =
  | "watch_and_fix"
  | "mark_ready"
  | "merge"
  | "fix_errors"
  | "address_feedback"
  | "update_branch"
  | "resolve_conflicts";

export type PrWorkflowStatus = {
  state: PrGate;
  checks: CheckCounts;
};

/**
 * Classify a live pull request without choosing surface-specific actions.
 * The state itself comes from `prState.ts` — the one ladder — so every
 * surface that asks gets the same answer.
 */
export function prWorkflowStatus(pr: PullRequestDigest): PrWorkflowStatus {
  const status = prStatus(pr);
  return { state: status.gate, checks: status.checks };
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
export type PrDirectMergeKind =
  | "merge"
  | "enable_auto_merge"
  | "merge_when_ready";

export type PrDirectMergeAction = {
  kind: PrDirectMergeKind;
  label: string;
  /** True when the API call arms auto-merge or the merge queue. */
  auto: boolean;
};

/**
 * The one merge button a surface may show.
 *
 * GitHub offers three distinct host actions: merge now, enable auto-merge, or
 * add the pull request to a merge queue ("Merge when ready"). A ready
 * pull request on a queue repository is the queue action, not a direct merge.
 * Already-queued and already-armed pull requests offer nothing.
 */
export function prDirectMergeAction(
  pr: PullRequestDigest,
  options?: { hasMergeQueue?: boolean; suppressAutoMerge?: boolean },
): PrDirectMergeAction | null {
  const { state } = prWorkflowStatus(pr);
  const controls = prMergeControls(state);
  if (
    state === "queued" ||
    state === "auto_merge" ||
    pr.auto_merge_enabled === true ||
    pr.in_merge_queue === true
  ) {
    return null;
  }
  if (options?.hasMergeQueue) {
    if (!controls.canMerge && !controls.canEnableAutoMerge) return null;
    return {
      kind: "merge_when_ready",
      label: "Merge when ready",
      auto: true,
    };
  }
  if (controls.canMerge) {
    return { kind: "merge", label: "Merge", auto: false };
  }
  if (controls.canEnableAutoMerge && !pr.auto_merge_enabled) {
    if (options?.suppressAutoMerge) return null;
    return {
      kind: "enable_auto_merge",
      label: "Enable auto-merge",
      auto: true,
    };
  }
  return null;
}

/** True when this repository already has a pull request in its merge queue. */
export function deliveryRepositoryHasMergeQueue(
  items: readonly CodeDeliveryPullRequestSummary[],
  repository: CodeDeliveryPullRequestSummary["repository"],
): boolean {
  return items.some(
    (item) =>
      item.in_merge_queue === true &&
      item.repository.host === repository.host &&
      item.repository.owner === repository.owner &&
      item.repository.name === repository.name,
  );
}

export function prMergeControls(state: PrGate): {
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
          "A repository requirement is still blocking a direct merge.",
      };
    case "needs_approval":
      return {
        canMerge: false,
        canEnableAutoMerge: true,
        explanation:
          "The pull request needs a review approval before merging directly.",
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
 * A delivery row in the digest vocabulary the workflow prompts consume.
 *
 * The assignment is structural and checked: every digest field the prompt
 * builder reads is present on the delivery summary under the same name. If
 * either wire type drifts, this stops compiling rather than silently
 * producing prompts with holes.
 */
export function deliveryPullRequestDigest(
  summary: CodeDeliveryPullRequestSummary,
): PullRequestDigest {
  return summary;
}

export type PrAgentQuickAction = {
  action: PrPromptAction;
  label: string;
};

/**
 * The agent-runnable chores this pull request currently has, in the order a
 * reader resolves them: conflicts block everything, then failing checks, then
 * review feedback, then a stale base. A settled pull request has none.
 *
 * More than one can apply at once — a conflicting PR with failing checks
 * offers both — because each runs as its own prompt.
 */
export function prAgentQuickActions(
  pr: PullRequestDigest,
): PrAgentQuickAction[] {
  const lifecycle = pullRequestLifecycle(pr);
  if (lifecycle === "merged" || lifecycle === "closed") return [];
  const items: PrAgentQuickAction[] = [];
  if (prHasConflicts(pr)) {
    items.push({ action: "resolve_conflicts", label: "Resolve conflicts" });
  }
  if (checkCounts(pr).failing > 0) {
    items.push({ action: "fix_errors", label: "Fix failing checks" });
  }
  if (prHasChangesRequested(pr)) {
    items.push({
      action: "address_feedback",
      label: "Address review feedback",
    });
  }
  if (prIsBehind(pr)) {
    items.push({ action: "update_branch", label: "Update branch from base" });
  }
  return items;
}

/**
 * The fresh-agent variant of a workflow prompt.
 *
 * A fresh workspace is cut from the pull request's head commit, but onto its
 * own Tidebreak branch — the server never checks out a shared branch
 * directly. A bare "push" would therefore publish the wrong branch and leave
 * the pull request untouched, so the suffix names the real push target.
 */
export function prFreshAgentPrompt(
  action: PrPromptAction,
  pr: PullRequestDigest,
  logs: readonly CodeCheckLog[] = [],
): string {
  const head = pr.head_branch?.trim();
  const suffix = head
    ? `\n\nThis workspace was just created from \`${head}\`, the pull request's head branch, but the workspace branch itself is new and local. Publish your work to the pull request with \`git push origin HEAD:${head}\`. Do not push the workspace branch under its own name, and do not open a new pull request.`
    : "";
  return `${prWorkflowPrompt(action, pr, logs)}${suffix}`;
}

/**
 * Prompt actions run in the workspace's interactive session.
 *
 * Three of the workflow actions are deliberately absent. "Watch and fix"
 * starts a durable server-side watch task (`POST
 * /code/workspaces/{id}/watch`). Merging and readying a draft are
 * pull-request state changes, which decision 42 reserves for the user: they
 * run through their own endpoints, and excluding them from this type is what
 * stops a merge prompt from being wired back up by accident.
 *
 * Settings can replace the instruction. Live pull-request context is still
 * appended after it.
 */
export function prWorkflowPrompt(
  action: PrPromptAction,
  pr: PullRequestDigest,
  logs: readonly CodeCheckLog[] = [],
): string {
  const number = `#${pr.number}`;
  const base = pr.base_branch?.trim() || "the base branch";
  const context = prWorkflowPromptContext(pr);
  const instruction = renderWorkflowPrompt(
    action,
    { pr: number, base },
    { logsAttached: logs.length > 0 },
  );
  const attached = checkLogSection(logs);
  return `${instruction}\n\n${context}${attached}`;
}

/**
 * Name the downloaded logs after the context block.
 *
 * The paths sit outside the Git worktree, in the private storage the session
 * is already allowed to read, so the prompt carries paths rather than bytes —
 * the same bargain a fork transcript makes. A log the fetch could not reach is
 * simply absent; the check itself is still named above.
 */
function checkLogSection(logs: readonly CodeCheckLog[]): string {
  if (logs.length === 0) return "";
  const lines = logs.map((log) => {
    const size = `${Math.max(1, Math.round(log.byte_len / 1024))} KB`;
    const note = log.truncated ? `tail, ${size}` : size;
    return `- \`${log.path}\` — ${log.check} (${note})`;
  });
  return [
    "",
    "",
    "Failure logs already downloaded for you:",
    ...lines,
    "",
    "Read these before running anything — they are the job logs for this head. A failing check not listed here has no downloaded log; fetch that one yourself if you need it.",
  ].join("\n");
}

function prWorkflowPromptContext(pr: PullRequestDigest): string {
  const title = pr.title?.trim();
  const url = pr.url?.trim();
  const head = pr.head_branch?.trim() || "current workspace branch";
  const base = pr.base_branch?.trim() || "base branch";
  const checks = checkCounts(pr);
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
