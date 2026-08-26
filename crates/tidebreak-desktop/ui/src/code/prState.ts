import type { StatusTone } from "./statusTone";

/**
 * The one place a pull request's state becomes an answer.
 *
 * A pull request answers four questions at once — where it sits in its
 * lifecycle, what stands between it and the base branch, who owns the next
 * move, and what its checks say — and every surface used to reach its own
 * conclusion, which is how merged came to render green in one place, blue in
 * another, and purple in a third. Derive the answer once here and paint from
 * it; a component that picks its own tone for a state is a bug.
 *
 * Both wire shapes feed the same functions: the workspace digest and the
 * delivery summary both satisfy {@link PrStateInput}, so no surface needs an
 * adapter or its own copy of the ladder.
 */

/** One check as either wire shape reports it. */
type CheckLike = { bucket: string; name?: string };
export type PrStateInput = {
  state: string;
  draft?: boolean | null;
  merged?: boolean | null;
  merged_at?: string | null;
  closed_at?: string | null;
  review_decision?: string | null;
  mergeable?: string | null;
  merge_state_status?: string | null;
  auto_merge_enabled?: boolean | null;
  in_merge_queue?: boolean | null;
  checks?: readonly CheckLike[] | null;
  checks_summary?: string | null;
};

/** A host token, trimmed and lowercased. Hosts send every casing. */
function hostToken(value: string | null | undefined): string {
  return value?.trim().toLowerCase() ?? "";
}

// ---------------------------------------------------------------------------

export type PullRequestLifecycle = "draft" | "open" | "merged" | "closed";

export const PULL_REQUEST_LIFECYCLE_LABEL: Record<
  PullRequestLifecycle,
  string
> = {
  draft: "Draft",
  open: "Open",
  merged: "Merged",
  closed: "Closed",
};

/**
 * Lifecycle color, in GitHub's own vocabulary: green open, gray draft,
 * purple merged, red closed. A settled pull request is a settled color on
 * every surface; nothing downstream may recolor it.
 */
export const PULL_REQUEST_LIFECYCLE_TONE: Record<
  PullRequestLifecycle,
  StatusTone
> = {
  draft: "neutral",
  open: "ready",
  merged: "merged",
  closed: "critical",
};

export function pullRequestLifecycle(pr: PrStateInput): PullRequestLifecycle {
  const state = hostToken(pr.state);
  // The merge evidence outranks the state token: hosts disagree on whether a
  // merged pull request reports MERGED or CLOSED, and only one reading is
  // ever right. `closed_at` is set on merged pull requests too, so it says
  // "settled", not "closed" — the merge checks run first.
  if (state === "merged" || pr.merged === true || pr.merged_at) {
    return "merged";
  }
  if (state === "closed" || pr.closed_at) return "closed";
  return pr.draft ? "draft" : "open";
}

/** When the pull request settled, or undefined while it is still open. */
export function pullRequestSettledAt(pr: PrStateInput): string | undefined {
  return pr.merged_at ?? pr.closed_at ?? undefined;
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

export type PullRequestReviewSummary = { label: string; tone: StatusTone };

/**
 * What the Review column should say.
 *
 * Once a pull request settles, its review decision is history: the column
 * reports the outcome instead of a review that will never happen. A review
 * requirement renders neutral, matching GitHub — it is a fact about the
 * branch rules, not a warning.
 */
export function pullRequestReviewSummary(
  pr: PrStateInput,
): PullRequestReviewSummary {
  const lifecycle = pullRequestLifecycle(pr);
  if (lifecycle === "merged" || lifecycle === "closed") {
    return {
      label: PULL_REQUEST_LIFECYCLE_LABEL[lifecycle],
      tone: PULL_REQUEST_LIFECYCLE_TONE[lifecycle],
    };
  }
  if (lifecycle === "draft") return { label: "Draft", tone: "neutral" };
  switch (hostToken(pr.review_decision)) {
    case "approved":
      return { label: "Approved", tone: "ready" };
    case "changes_requested":
      return { label: "Changes requested", tone: "critical" };
    case "review_required":
      return { label: "Review required", tone: "neutral" };
    default:
      return { label: "Review pending", tone: "neutral" };
  }
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

export type CheckCounts = {
  total: number;
  passing: number;
  pending: number;
  failing: number;
  skipped: number;
};

/**
 * One pass over the check rollup — the single counter. Falls back to the
 * one-line summary the host sends when it did not report individual checks.
 */
export function checkCounts(
  pr: Pick<PrStateInput, "checks" | "checks_summary">,
): CheckCounts {
  const checks = pr.checks ?? [];
  if (checks.length > 0) {
    const counts: CheckCounts = {
      total: checks.length,
      passing: 0,
      pending: 0,
      failing: 0,
      skipped: 0,
    };
    for (const check of checks) {
      const bucket = hostToken(check.bucket);
      if (bucket === "pass") counts.passing += 1;
      else if (bucket === "pending") counts.pending += 1;
      else if (bucket === "fail") counts.failing += 1;
      else counts.skipped += 1;
    }
    return counts;
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

/** GitHub's own summary line: failures first, then work in flight. */
export function checkSummary(counts: CheckCounts): {
  label: string;
  tone: StatusTone;
} {
  if (counts.total === 0) return { label: "No checks", tone: "neutral" };
  if (counts.failing > 0) {
    return { label: `${counts.failing} failed`, tone: "critical" };
  }
  if (counts.pending > 0) {
    return { label: `${counts.pending} pending`, tone: "pending" };
  }
  if (counts.passing > 0) {
    return { label: `${counts.passing} passed`, tone: "ready" };
  }
  return { label: `${counts.skipped} skipped`, tone: "neutral" };
}

/** The same counts as compact prose, for detail text: `2 passing · 1 failing`. */
export function checkSummaryText(counts: CheckCounts): string {
  const parts: string[] = [];
  if (counts.passing > 0) parts.push(`${counts.passing} passing`);
  if (counts.pending > 0) parts.push(`${counts.pending} pending`);
  if (counts.failing > 0) parts.push(`${counts.failing} failing`);
  if (counts.skipped > 0) parts.push(`${counts.skipped} skipped`);
  return parts.join(" · ");
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/**
 * The next move, as one state. Terminal lifecycles appear here too, so a
 * consumer can switch on `gate` alone.
 */
export type PrGate =
  | "checking"
  | "ready"
  | "pending"
  | "failing"
  | "conflict"
  | "behind"
  | "blocked"
  | "needs_approval"
  | "changes_requested"
  | "draft"
  | "queued"
  | "auto_merge"
  | "merged"
  | "closed";

export const PR_GATE_LABEL: Record<PrGate, string> = {
  checking: "Checking status",
  ready: "Ready to merge",
  pending: "Checks running",
  failing: "Checks failed",
  conflict: "Resolve conflicts",
  behind: "Update branch",
  blocked: "Merge blocked",
  needs_approval: "Needs approval",
  changes_requested: "Changes requested",
  draft: "Draft",
  queued: "In merge queue",
  auto_merge: "Auto-merge on",
  merged: "Merged",
  closed: "Closed",
};

/**
 * Gate color. Queued and auto-merge ride `pending`'s blue on purpose: the
 * host owns the next move, which is "in flight, waiting on something
 * external" — exactly what the info tone means. Amber stays reserved for
 * states the reader can act on.
 */
export const PR_GATE_TONE: Record<PrGate, StatusTone> = {
  checking: "neutral",
  ready: "ready",
  pending: "pending",
  failing: "critical",
  conflict: "critical",
  behind: "warning",
  blocked: "warning",
  needs_approval: "warning",
  changes_requested: "critical",
  draft: "neutral",
  queued: "pending",
  auto_merge: "pending",
  merged: "merged",
  closed: "critical",
};

export type PullRequestListGroup =
  | "attention"
  | "ready"
  | "waiting"
  | "handed_off"
  | "draft"
  | "done";

/** Who owns the next move: the reader, the host, or nobody. */
export const PR_GATE_GROUP: Record<PrGate, PullRequestListGroup> = {
  conflict: "attention",
  changes_requested: "attention",
  failing: "attention",
  behind: "attention",
  blocked: "attention",
  needs_approval: "waiting",
  pending: "waiting",
  checking: "waiting",
  queued: "handed_off",
  auto_merge: "handed_off",
  ready: "ready",
  draft: "draft",
  merged: "done",
  closed: "done",
};

export function prHasConflicts(pr: PrStateInput): boolean {
  return (
    hostToken(pr.mergeable) === "conflicting" ||
    hostToken(pr.merge_state_status) === "dirty"
  );
}

/** Only an explicit host signal counts as merge-queue membership. */
export function prIsQueued(pr: PrStateInput): boolean {
  return pr.in_merge_queue === true;
}

export function prIsBehind(pr: PrStateInput): boolean {
  return hostToken(pr.merge_state_status) === "behind";
}

export function prIsBlocked(pr: PrStateInput): boolean {
  const mergeState = hostToken(pr.merge_state_status);
  return mergeState === "blocked" || mergeState === "unstable";
}

/** GitHub still wants a review approval on this pull request. */
export function prNeedsApproval(pr: PrStateInput): boolean {
  return (
    hostToken(pr.review_decision) === "review_required" &&
    hostToken(pr.merge_state_status) === "blocked"
  );
}

export function prHasChangesRequested(pr: PrStateInput): boolean {
  return hostToken(pr.review_decision) === "changes_requested";
}

/**
 * The single ladder. A conflicted tree blocks every other fix, then human
 * feedback, then failing checks — a queue entry does not erase a failure the
 * reader still has to act on. Queue membership outranks the softer states
 * below it, because GitHub has accepted the pull request for merging and
 * stale branch data should not argue with that. Pending checks outrank the
 * generic block because GitHub reports `blocked` while required checks run —
 * without that rule every open pull request reads "Merge blocked" for its
 * whole life (decision 66).
 */
export function prGate(pr: PrStateInput): PrGate {
  const lifecycle = pullRequestLifecycle(pr);
  if (lifecycle === "merged") return "merged";
  if (lifecycle === "closed") return "closed";
  if (lifecycle === "draft") return "draft";
  if (prHasConflicts(pr)) return "conflict";
  if (prHasChangesRequested(pr)) return "changes_requested";
  const checks = checkCounts(pr);
  if (checks.failing > 0) return "failing";
  if (prIsQueued(pr)) return "queued";
  if (prIsBehind(pr)) return "behind";
  if (checks.pending > 0) return "pending";
  if (prNeedsApproval(pr)) return "needs_approval";
  if (prIsBlocked(pr)) return "blocked";
  if (pr.auto_merge_enabled) return "auto_merge";
  return hostToken(pr.mergeable) === "mergeable" &&
    hostToken(pr.merge_state_status) === "clean"
    ? "ready"
    : "checking";
}

/**
 * The whole answer at once: lifecycle, gate, the headline a row should show,
 * the delivery group, and the check counts. Everything a surface paints comes
 * from this object or from the table lookups above — never from the raw host
 * fields.
 */
export type PrStatus = {
  lifecycle: PullRequestLifecycle;
  gate: PrGate;
  headline: { label: string; tone: StatusTone };
  group: PullRequestListGroup;
  checks: CheckCounts;
};

export function prStatus(pr: PrStateInput): PrStatus {
  const gate = prGate(pr);
  return {
    lifecycle: pullRequestLifecycle(pr),
    gate,
    headline: { label: PR_GATE_LABEL[gate], tone: PR_GATE_TONE[gate] },
    group: PR_GATE_GROUP[gate],
    checks: checkCounts(pr),
  };
}

// ---------------------------------------------------------------------------
// Blockers
// ---------------------------------------------------------------------------

/**
 * Every reason a merge is blocked, in the words GitHub uses on the merge
 * box. The merge box lists all of them; a headline picks one. Empty for a
 * pull request with nothing standing between it and the base branch.
 */
export function mergeBlockedReasons(pr: PrStateInput): string[] {
  const lifecycle = pullRequestLifecycle(pr);
  if (lifecycle === "merged") return ["This pull request is already merged."];
  if (lifecycle === "closed") return ["Reopen this pull request to merge it."];
  if (lifecycle === "draft")
    return ["Mark this pull request ready to merge it."];
  const reasons: string[] = [];
  const checks = checkCounts(pr);
  if (prHasConflicts(pr)) {
    reasons.push("Resolve the conflicts with the base branch first.");
  }
  if (prHasChangesRequested(pr)) {
    reasons.push("Address the requested changes before merging.");
  }
  if (checks.failing > 0) {
    reasons.push(
      checks.failing === 1
        ? "Fix the failing check before merging."
        : `Fix the ${checks.failing} failing checks before merging.`,
    );
  }
  if (prIsBehind(pr)) {
    reasons.push("Update the branch from its base first.");
  }
  if (checks.pending > 0) {
    reasons.push(
      checks.pending === 1
        ? "Wait for the running check before merging."
        : `Wait for the ${checks.pending} running checks before merging.`,
    );
  }
  if (prNeedsApproval(pr)) {
    reasons.push(
      "The pull request needs a review approval before merging directly.",
    );
  }
  if (reasons.length === 0 && prIsBlocked(pr)) {
    reasons.push("A repository requirement is still blocking a direct merge.");
  }
  return reasons;
}

// ---------------------------------------------------------------------------
// Chips
// ---------------------------------------------------------------------------

/** The Badge variant a status tone paints with. One map, no local copies. */
export type StatusToneBadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "success"
  | "warning"
  | "critical"
  | "info"
  | "merged"
  | "live";

export const STATUS_TONE_BADGE_VARIANT: Record<
  StatusTone,
  StatusToneBadgeVariant
> = {
  neutral: "outline",
  running: "live",
  ready: "success",
  pending: "info",
  warning: "warning",
  critical: "critical",
  merged: "merged",
};

export type PrStateChipKey = "lifecycle" | "review" | "queue" | "auto_merge";

export type PrStateChip = {
  key: PrStateChipKey;
  label: string;
  tone: StatusTone;
};

/**
 * The chips a status surface shows, in order: lifecycle first, then the
 * review verdict, then queue or auto-merge. Queue membership is its own chip
 * rather than a replacement for the lifecycle word — a queued pull request
 * is still open, and a single chip that says "Queued" erases that.
 */
export function prStateChips(pr: PrStateInput): PrStateChip[] {
  const lifecycle = pullRequestLifecycle(pr);
  const chips: PrStateChip[] = [
    {
      key: "lifecycle",
      label: PULL_REQUEST_LIFECYCLE_LABEL[lifecycle],
      tone: PULL_REQUEST_LIFECYCLE_TONE[lifecycle],
    },
  ];
  if (lifecycle === "open" || lifecycle === "draft") {
    const decision = hostToken(pr.review_decision);
    if (decision === "approved") {
      chips.push({ key: "review", label: "Approved", tone: "ready" });
    } else if (decision === "changes_requested") {
      chips.push({
        key: "review",
        label: "Changes requested",
        tone: "critical",
      });
    } else if (decision === "review_required") {
      chips.push({ key: "review", label: "Review required", tone: "neutral" });
    }
    if (prIsQueued(pr)) {
      chips.push({ key: "queue", label: "In merge queue", tone: "pending" });
    } else if (pr.auto_merge_enabled) {
      chips.push({
        key: "auto_merge",
        label: "Auto-merge on",
        tone: "pending",
      });
    }
  }
  return chips;
}

/**
 * The one word a compact surface shows. Queue membership is the more
 * specific truth, so it stands in for the lifecycle word there — but the
 * lifecycle glyph keeps its own color, so "open, queued" still reads at a
 * glance.
 */
export function prCompactStatusLabel(pr: PrStateInput): string {
  if (prIsQueued(pr) && pullRequestLifecycle(pr) === "open") {
    return "In merge queue";
  }
  return PULL_REQUEST_LIFECYCLE_LABEL[pullRequestLifecycle(pr)];
}

/** The tone that word paints with on a compact surface. */
export function prCompactStatusTone(pr: PrStateInput): StatusTone {
  if (prIsQueued(pr) && pullRequestLifecycle(pr) === "open") {
    return "pending";
  }
  return PULL_REQUEST_LIFECYCLE_TONE[pullRequestLifecycle(pr)];
}
