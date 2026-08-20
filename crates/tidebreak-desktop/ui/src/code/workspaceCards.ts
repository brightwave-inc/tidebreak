import type {
  Attention,
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
} from "../api/types";
import { attentionLabel, LIFECYCLE_LABELS } from "./labels";

/**
 * Pure presentation logic for workspace cards: grouping by repo, the state
 * line, and PR chip tones. The rail renders from these; list pages can reuse
 * them.
 */

export type WorkspaceGroup = {
  /** Null collects workspaces whose repo is missing from the catalog. */
  repo: CodeRepoSnapshot | null;
  workspaces: CodeWorkspaceSnapshot[];
};

/**
 * Group unarchived workspaces under their repo, in catalog repo order.
 * Repos without live workspaces get no group. Workspaces whose repo the
 * catalog does not know land in a trailing null-repo group rather than
 * disappearing.
 */
export function groupWorkspacesByRepo(
  repos: readonly CodeRepoSnapshot[],
  workspaces: readonly CodeWorkspaceSnapshot[],
): WorkspaceGroup[] {
  const byRepo = new Map<string, CodeWorkspaceSnapshot[]>();
  for (const workspace of workspaces) {
    if (workspace.status === "archived") continue;
    const listed = byRepo.get(workspace.repo_id);
    if (listed) {
      listed.push(workspace);
    } else {
      byRepo.set(workspace.repo_id, [workspace]);
    }
  }
  const groups: WorkspaceGroup[] = [];
  for (const repo of repos) {
    const listed = byRepo.get(repo.id);
    if (!listed) continue;
    byRepo.delete(repo.id);
    groups.push({ repo, workspaces: sortByCreated(listed, "asc") });
  }
  const orphans = sortByCreated([...byRepo.values()].flat(), "asc");
  if (orphans.length > 0) groups.push({ repo: null, workspaces: orphans });
  return groups;
}

export type WorkspaceSortMode = "by-repo" | "by-status" | "by-created";

export const WORKSPACE_SORT_MODES: readonly WorkspaceSortMode[] = [
  "by-repo",
  "by-status",
  "by-created",
];

export const WORKSPACE_SORT_MODE_LABELS: Record<WorkspaceSortMode, string> = {
  "by-repo": "By repo",
  "by-status": "By status",
  "by-created": "By created",
};

export function isWorkspaceSortMode(value: string): value is WorkspaceSortMode {
  return (WORKSPACE_SORT_MODES as readonly string[]).includes(value);
}

/** How much of a card the rail draws; the aria-label never shrinks with it. */
export type CardDensity = "compact" | "detailed";

export const CARD_DENSITIES: readonly CardDensity[] = ["compact", "detailed"];

export const CARD_DENSITY_LABELS: Record<CardDensity, string> = {
  compact: "Compact",
  detailed: "Detailed",
};

export function isCardDensity(value: string): value is CardDensity {
  return (CARD_DENSITIES as readonly string[]).includes(value);
}

export type WorkspaceStatusRank =
  | "needs_you"
  | "running"
  | "pr_open"
  | "done_unreviewed"
  | "idle"
  | "archived";

export const WORKSPACE_STATUS_RANK_ORDER: readonly WorkspaceStatusRank[] = [
  "needs_you",
  "running",
  "pr_open",
  "done_unreviewed",
  "idle",
  "archived",
];

export const WORKSPACE_STATUS_RANK_LABELS: Record<WorkspaceStatusRank, string> =
  {
    needs_you: "Needs you",
    running: "Running",
    pr_open: "PR open",
    done_unreviewed: "Done",
    idle: "Idle",
    archived: "Archived",
  };

/**
 * Rank a workspace for the by-status rail. Needs-you wins, then a running
 * engine, then an open PR, then done-unreviewed, then idle. Archived is last.
 * A digest may change the rank; viewing or selecting never does.
 */
export function workspaceStatusRank(
  workspace: CodeWorkspaceSnapshot,
  digest: CodeSessionDigest | undefined,
): WorkspaceStatusRank {
  if (workspace.status === "archived") return "archived";
  if (digest?.attention.state.type === "needs_you") return "needs_you";
  if (digest?.lifecycle === "running") return "running";
  const pr = digest?.pr_state ?? workspace.pr;
  if (pr) {
    const tone = prTone(pr);
    if (tone === "open" || tone === "draft") return "pr_open";
  }
  if (digest?.attention.state.type === "done_unreviewed") {
    return "done_unreviewed";
  }
  return "idle";
}

export type StatusWorkspaceGroup = {
  rank: WorkspaceStatusRank;
  workspaces: CodeWorkspaceSnapshot[];
};

/**
 * Group workspaces by status rank. Archived rows stay visible here so triage
 * can scroll past live work to what has already been put away. Within a rank,
 * newest created_at wins; id breaks ties.
 */
export function groupWorkspacesByStatus(
  workspaces: readonly CodeWorkspaceSnapshot[],
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>,
): StatusWorkspaceGroup[] {
  const buckets = new Map<WorkspaceStatusRank, CodeWorkspaceSnapshot[]>();
  for (const rank of WORKSPACE_STATUS_RANK_ORDER) buckets.set(rank, []);
  for (const workspace of workspaces) {
    const rank = workspaceStatusRank(workspace, digests[workspace.id]);
    buckets.get(rank)?.push(workspace);
  }
  const groups: StatusWorkspaceGroup[] = [];
  for (const rank of WORKSPACE_STATUS_RANK_ORDER) {
    const listed = sortByCreated(buckets.get(rank) ?? [], "desc");
    if (listed.length === 0) continue;
    groups.push({ rank, workspaces: listed });
  }
  return groups;
}

/** Live workspaces, newest created_at first. Archived rows stay off this list. */
export function listWorkspacesByCreated(
  workspaces: readonly CodeWorkspaceSnapshot[],
): CodeWorkspaceSnapshot[] {
  return sortByCreated(
    workspaces.filter((workspace) => workspace.status !== "archived"),
    "desc",
  );
}

export type ArrangedWorkspaceGroup = {
  key: string;
  label: string | null;
  /** Set when the group is one repo, so the rail can link its header. */
  repoId?: string;
  workspaces: CodeWorkspaceSnapshot[];
};

/**
 * The one function the rail reads. Ordering is a pure function of created_at,
 * repo catalog order, and status rank — never catalog array order or digest
 * insertion order.
 *
 * Archived workspaces are off the rail unless asked for; when shown, they
 * are one trailing group in every mode rather than woven back into their
 * repo, so put-away work never interleaves with live triage.
 */
export function arrangeWorkspaces(
  mode: WorkspaceSortMode,
  repos: readonly CodeRepoSnapshot[],
  workspaces: readonly CodeWorkspaceSnapshot[],
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>,
  options?: { showArchived?: boolean },
): ArrangedWorkspaceGroup[] {
  const groups = arrangeLiveWorkspaces(mode, repos, workspaces, digests);
  if (!options?.showArchived) return groups;
  const archived = listArchivedWorkspaces(workspaces);
  if (archived.length === 0) return groups;
  return [...groups, { key: "archived", label: "Archived", workspaces: archived }];
}

function arrangeLiveWorkspaces(
  mode: WorkspaceSortMode,
  repos: readonly CodeRepoSnapshot[],
  workspaces: readonly CodeWorkspaceSnapshot[],
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>,
): ArrangedWorkspaceGroup[] {
  if (mode === "by-created") {
    return [
      {
        key: "created",
        label: null,
        workspaces: listWorkspacesByCreated(workspaces),
      },
    ];
  }
  if (mode === "by-status") {
    return groupWorkspacesByStatus(workspaces, digests)
      .filter((group) => group.rank !== "archived")
      .map((group) => ({
        key: group.rank,
        label: WORKSPACE_STATUS_RANK_LABELS[group.rank],
        workspaces: group.workspaces,
      }));
  }
  return groupWorkspacesByRepo(repos, workspaces).map((group) => ({
    key: group.repo?.id ?? "unknown-repo",
    label: group.repo?.display_name ?? "Other repos",
    repoId: group.repo?.id,
    workspaces: group.workspaces,
  }));
}

/**
 * Archived workspaces, most recently put away first. `archived_at` orders the
 * shelf; rows missing it (older servers) fall back to created_at.
 */
export function listArchivedWorkspaces(
  workspaces: readonly CodeWorkspaceSnapshot[],
): CodeWorkspaceSnapshot[] {
  return workspaces
    .filter((workspace) => workspace.status === "archived")
    .sort((left, right) => {
      const byTime = (right.archived_at ?? right.created_at).localeCompare(
        left.archived_at ?? left.created_at,
      );
      if (byTime !== 0) return byTime;
      return left.id.localeCompare(right.id);
    });
}

function sortByCreated(
  workspaces: readonly CodeWorkspaceSnapshot[],
  direction: "asc" | "desc",
): CodeWorkspaceSnapshot[] {
  const sign = direction === "asc" ? 1 : -1;
  return [...workspaces].sort((left, right) => {
    const byTime = left.created_at.localeCompare(right.created_at);
    if (byTime !== 0) return sign * byTime;
    return sign * left.id.localeCompare(right.id);
  });
}

/** True when the rail should draw the nested session row. */
export function isSessionRowWorthy(
  digest: CodeSessionDigest | undefined,
): digest is CodeSessionDigest {
  if (!digest) return false;
  if (digest.lifecycle === "running" || digest.lifecycle === "fenced") {
    return true;
  }
  const type = digest.attention.state.type;
  return type === "needs_you" || type === "stalled";
}

/** Short lifecycle word for the nested session row. */
export function sessionRowLabel(digest: CodeSessionDigest): string {
  if (digest.attention.state.type === "needs_you") return "Needs you";
  if (digest.lifecycle === "running") return sessionActivityLabel(digest);
  switch (digest.attention.state.type) {
    case "stalled":
      return "Stalled";
    case "fenced":
      return "Fenced";
    case "done_unreviewed":
      return "Done";
    case "manual":
      return "Pinned";
    default:
      return LIFECYCLE_LABELS[digest.lifecycle];
  }
}

/** Precise running-state copy for a workspace row. */
export function sessionActivityLabel(digest: CodeSessionDigest): string {
  const runningSubagents =
    digest.subagents?.filter((entry) => entry.status === "running").length ?? 0;
  if (runningSubagents > 0 || digest.activity === "subagents") {
    if (runningSubagents === 1) return "1 subagent working";
    if (runningSubagents > 1) return `${runningSubagents} subagents working`;
    return "Subagents working";
  }
  switch (digest.activity) {
    case "shell":
      return "Shell running";
    case "monitor":
      return "Monitoring";
    case "file":
      return "Working with files";
    case "search":
      return "Searching";
    case "tool":
      return "Tool running";
    case "agent":
    case undefined:
      return "Agent working";
  }
}

/**
 * The word a watch child row shows. The watch's own state beats lifecycle —
 * a watch is "running" for hours, but "Fixing ×3" is what it is doing —
 * except when it needs you, which outranks everything on a triage rail.
 * Digests from a server without watch enrichment still use watch-specific
 * wording instead of borrowing the interactive session's activity label.
 */
export function watchRowLabel(digest: CodeSessionDigest): string {
  if (digest.attention.state.type === "needs_you") return "Needs you";
  switch (digest.watch_state) {
    case "watching":
      return "Watching";
    case "fixing":
      return digest.watch_cycles !== undefined && digest.watch_cycles > 1
        ? `Fixing ×${digest.watch_cycles}`
        : "Fixing";
    case "blocked":
      return "Blocked";
    case "done":
      return "Done";
    case "stopped":
      return "Stopped";
    case "failed":
      return "Failed";
    default:
      if (digest.lifecycle === "running") return "Watching";
      return sessionRowLabel(digest);
  }
}

/**
 * The whole card as one line, for readers who get the name and nothing else.
 *
 * A card is a title, a state, and two identifiers spread across three rows and
 * a glyph rail. Left to compose itself the button would announce only its
 * title — the state glyphs are the point of the rail, and a triage read that
 * cannot hear "needs you" is not a triage read. Order follows the card: what
 * it is, then what it wants, then which checkout it is.
 */
export function workspaceCardLabel(input: {
  title: string;
  repoName: string;
  branchName: string;
  attention?: Attention;
  session?: CodeSessionDigest;
  pr?: { number: number; state: string; draft?: boolean };
  terminalOpen?: boolean;
}): string {
  const parts = [input.title];
  if (input.attention && input.attention.state.type !== "working") {
    parts.push(attentionLabel(input.attention));
  }
  if (input.session?.lifecycle === "running") {
    parts.push(sessionActivityLabel(input.session));
  }
  if (input.pr) {
    parts.push(`Pull request #${input.pr.number} ${prToneLabel(input.pr)}`);
  }
  if (input.terminalOpen) parts.push("Terminal open");
  parts.push(input.repoName, input.branchName);
  return parts.join(" · ");
}

/**
 * Keep the head and tail of a git name so the unique suffix survives a
 * narrow rail. One ellipsis, never a CSS end-truncate on branches.
 */
export function middleTruncate(text: string, maxChars: number): string {
  if (maxChars < 3 || text.length <= maxChars) return text;
  const budget = maxChars - 1;
  const head = Math.ceil(budget / 2);
  const tail = Math.floor(budget / 2);
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

/** Compact age for a session row: "now", "12m", "1h", "3d". */
export function formatCompactAge(
  iso: string,
  nowMs: number = Date.now(),
): string | null {
  const then = Date.parse(iso);
  if (!Number.isFinite(then)) return null;
  const sec = Math.max(0, Math.floor((nowMs - then) / 1000));
  if (sec < 45) return "now";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

const REPO_ACCENT_CLASSES = [
  "bg-sky-500/80",
  "bg-teal-500/80",
  "bg-amber-500/80",
  "bg-rose-500/80",
  "bg-indigo-500/80",
  "bg-lime-600/80",
] as const;

/** Stable identity swatch for a repo chip. Not a status color. */
export function repoAccentClass(id: string): string {
  let hash = 0;
  for (let index = 0; index < id.length; index += 1) {
    hash = (hash * 31 + id.charCodeAt(index)) | 0;
  }
  const swatch =
    REPO_ACCENT_CLASSES[Math.abs(hash) % REPO_ACCENT_CLASSES.length];
  return swatch ?? REPO_ACCENT_CLASSES[0];
}

export type PrChipTone = "open" | "draft" | "merged" | "closed" | "other";

/** Host state token → chip tone. Unknown tokens stay neutral. */
export function prChipTone(state: string): PrChipTone {
  const token = state.trim().toLowerCase();
  if (token === "open") return "open";
  if (token === "draft") return "draft";
  if (token === "merged") return "merged";
  if (token === "closed") return "closed";
  return "other";
}

/**
 * The tone for a whole digest: the draft flag wins over the state token,
 * since gh reports a draft PR's state as plain "open".
 */
export function prTone(pr: { state: string; draft?: boolean }): PrChipTone {
  const tone = prChipTone(pr.state);
  return tone === "open" && pr.draft ? "draft" : tone;
}

/** The word a badge shows for a tone; unknown tones show the raw token. */
export function prToneLabel(
  pr: { state: string; draft?: boolean },
): string {
  switch (prTone(pr)) {
    case "open":
      return "Open";
    case "draft":
      return "Draft";
    case "merged":
      return "Merged";
    case "closed":
      return "Closed";
    case "other":
      return pr.state;
  }
}

/** Icon (text) classes per tone, for the PR mark itself. */
export const PR_ICON_TONE_CLASSES: Record<PrChipTone, string> = {
  open: "text-success",
  draft: "text-muted-foreground",
  merged: "text-purple-600 dark:text-purple-400",
  closed: "text-critical",
  other: "text-muted-foreground",
};

/**
 * Chip classes per tone. Open, draft, and closed ride the semantic theme
 * tokens; merged has no token, so it pins GitHub's purple in both themes.
 */
export const PR_CHIP_TONE_CLASSES: Record<PrChipTone, string> = {
  open: "bg-success-background text-success-foreground-muted",
  draft: "bg-muted text-muted-foreground",
  merged: "bg-purple-100 text-purple-700 dark:bg-purple-500/20 dark:text-purple-300",
  closed: "bg-critical-background text-critical-foreground-muted",
  other: "bg-muted text-muted-foreground",
};
