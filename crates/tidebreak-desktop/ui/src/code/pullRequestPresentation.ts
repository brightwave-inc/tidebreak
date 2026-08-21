import type {
  CodeDeliveryCheck,
  CodeDeliveryPullRequestSummary,
} from "../api/types";
import type { StatusTone } from "./statusTone";

/**
 * The one place a pull request's *lifecycle* is decided.
 *
 * `state` alone is not the answer a reader wants. A draft is `open` on the
 * wire but reads as its own thing on GitHub, and `review_decision` is empty
 * on everything that already settled — which is how every merged and closed
 * row came to render as "Review pending". Deriving the lifecycle once, here,
 * is what keeps the row icon, the row badge, the detail header, and the
 * filter chips from each reaching their own conclusion.
 */
export type PullRequestLifecycle = "draft" | "open" | "merged" | "closed";

export const PULL_REQUEST_LIFECYCLE_LABEL: Record<PullRequestLifecycle, string> =
  {
    draft: "Draft",
    open: "Open",
    merged: "Merged",
    closed: "Closed",
  };

/**
 * Lifecycle color. `merged` is its own status tone rather than a borrowed
 * success: a merged pull request is a settled outcome, not a passing one, and
 * the token set already carries the distinction.
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

type LifecycleInput = Pick<
  CodeDeliveryPullRequestSummary,
  "state" | "draft"
> &
  Partial<Pick<CodeDeliveryPullRequestSummary, "merged_at" | "closed_at">>;

export function pullRequestLifecycle(
  item: LifecycleInput,
): PullRequestLifecycle {
  const state = item.state.toLowerCase();
  // The merge timestamp outranks the state string. Hosts disagree on whether
  // a merged pull request reports MERGED or CLOSED, and only one of those
  // readings is ever right.
  if (state === "merged" || item.merged_at) return "merged";
  if (state === "closed") return "closed";
  return item.draft ? "draft" : "open";
}

/** When the pull request settled, or undefined while it is still open. */
export function pullRequestSettledAt(
  item: LifecycleInput,
): string | undefined {
  return item.merged_at ?? item.closed_at;
}

/**
 * What the Review column should say.
 *
 * Once a pull request settles, its review decision is history: the column
 * reports the outcome instead of a review that will never happen.
 */
export type PullRequestReviewSummary = {
  label: string;
  tone: StatusTone;
};

export function pullRequestReviewSummary(
  item: LifecycleInput & Pick<CodeDeliveryPullRequestSummary, "review_decision">,
): PullRequestReviewSummary {
  const lifecycle = pullRequestLifecycle(item);
  if (lifecycle === "merged" || lifecycle === "closed") {
    return {
      label: PULL_REQUEST_LIFECYCLE_LABEL[lifecycle],
      tone: PULL_REQUEST_LIFECYCLE_TONE[lifecycle],
    };
  }
  if (lifecycle === "draft") return { label: "Draft", tone: "neutral" };
  switch (item.review_decision) {
    case "approved":
      return { label: "Approved", tone: "ready" };
    case "changes_requested":
      return { label: "Changes requested", tone: "critical" };
    case "review_required":
      return { label: "Review required", tone: "warning" };
    default:
      return { label: "Review pending", tone: "warning" };
  }
}

export type CheckCounts = {
  total: number;
  passed: number;
  pending: number;
  failed: number;
  skipped: number;
};

/** One pass over the rollup. Rows call this per render. */
export function checkCounts(checks: readonly CodeDeliveryCheck[]): CheckCounts {
  const counts: CheckCounts = {
    total: checks.length,
    passed: 0,
    pending: 0,
    failed: 0,
    skipped: 0,
  };
  for (const check of checks) {
    if (check.bucket === "pass") counts.passed += 1;
    else if (check.bucket === "pending") counts.pending += 1;
    else if (check.bucket === "fail") counts.failed += 1;
    else counts.skipped += 1;
  }
  return counts;
}

/** GitHub's own summary line: failures first, then work in flight. */
export function checkSummary(counts: CheckCounts): {
  label: string;
  tone: StatusTone;
} {
  if (counts.total === 0) return { label: "No checks", tone: "neutral" };
  if (counts.failed > 0) {
    return { label: `${counts.failed} failed`, tone: "critical" };
  }
  if (counts.pending > 0) {
    return { label: `${counts.pending} pending`, tone: "pending" };
  }
  if (counts.passed > 0) {
    return { label: `${counts.passed} passed`, tone: "ready" };
  }
  return { label: `${counts.skipped} skipped`, tone: "neutral" };
}

/**
 * Why a merge is blocked, in the words GitHub uses on the merge box.
 *
 * Returned as a sentence the panel can show next to a disabled button, so a
 * blocked merge explains itself instead of failing at the API.
 */
export function mergeBlockedReason(
  item: Pick<
    CodeDeliveryPullRequestSummary,
    "state" | "draft" | "mergeable" | "merge_state_status" | "checks"
  > &
    Partial<Pick<CodeDeliveryPullRequestSummary, "merged_at" | "closed_at">>,
): string | null {
  const lifecycle = pullRequestLifecycle(item);
  if (lifecycle === "merged") return "This pull request is already merged.";
  if (lifecycle === "closed") return "Reopen this pull request to merge it.";
  if (lifecycle === "draft") return "Mark this pull request ready to merge it.";
  if (item.mergeable === "conflicting" || item.merge_state_status === "dirty") {
    return "Resolve the conflicts with the base branch first.";
  }
  if (item.merge_state_status === "behind") {
    return "Update the branch from its base first.";
  }
  if (item.merge_state_status === "blocked") {
    return "A required review or check is still blocking the merge.";
  }
  const counts = checkCounts(item.checks);
  if (counts.failed > 0) return "Required checks are failing.";
  return null;
}

/** GitHub's diff-status vocabulary, as a short verb the row can show. */
export const FILE_STATUS_LABEL: Readonly<Record<string, string>> = {
  added: "Added",
  removed: "Removed",
  modified: "Modified",
  renamed: "Renamed",
  copied: "Copied",
  changed: "Changed",
  unchanged: "Unchanged",
};

export function fileStatusLabel(status: string): string {
  return FILE_STATUS_LABEL[status] ?? "Changed";
}

export function fileStatusTone(status: string): StatusTone {
  if (status === "added") return "ready";
  if (status === "removed") return "critical";
  return "neutral";
}

/**
 * A GitHub avatar for a login, when the host did not send one.
 *
 * Only well-formed logins are turned into URLs, so a display name never
 * becomes a request.
 */
export function githubAvatarUrl(
  author: string | undefined,
): string | undefined {
  if (!author || !/^[A-Za-z0-9-]+$/.test(author)) return undefined;
  return `https://github.com/${encodeURIComponent(author)}.png?size=64`;
}

const GITHUB_EMOJI: Readonly<Record<string, string>> = {
  "+1": "👍",
  "-1": "👎",
  bug: "🐛",
  checkered_flag: "🏁",
  eyes: "👀",
  fire: "🔥",
  heart: "❤️",
  heavy_check_mark: "✔️",
  laughing: "😆",
  memo: "📝",
  party_parrot: "🦜",
  rocket: "🚀",
  shipit: "🐿️",
  smile: "😄",
  sparkles: "✨",
  tada: "🎉",
  thinking: "🤔",
  warning: "⚠️",
  wave: "👋",
  white_check_mark: "✅",
  x: "❌",
};

/** Expand common GitHub emoji shortcodes outside inline and fenced code. */
export function expandGithubEmojiShortcodes(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g)
    .map((part) => {
      if (part.startsWith("`")) return part;
      return part.replace(/:([+\-a-z0-9_]+):/gi, (token, name: string) => {
        return GITHUB_EMOJI[name.toLowerCase()] ?? token;
      });
    })
    .join("");
}
