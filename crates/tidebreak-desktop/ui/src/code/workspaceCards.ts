import type {
  Attention,
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  CodeWorkspaceStatus,
} from "../api/types";
import { attentionLabel, LIFECYCLE_LABELS } from "./labels";
import { prCompactStatusLabel, pullRequestLifecycle } from "./prState";
import type { StatusTone } from "./statusTone";

/**
 * Pure presentation logic for workspace cards: grouping by repo, the state
 * line, and PR chip tones. The rail renders from these; list pages can reuse
 * them.
 */

/**
 * Whether a workspace has been put away rather than being live work.
 *
 * Archived and released are both reclaim tiers — released has simply given up
 * more (its branch, not only its checkout). Every surface that asks "is this
 * still on the rail?" wants both, so ask it here rather than comparing to
 * `"archived"` and silently letting released workspaces read as live.
 */
export function isPutAway(workspace: CodeWorkspaceSnapshot): boolean {
  return workspace.status === "archived" || workspace.status === "released";
}

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
    if (isPutAway(workspace)) continue;
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
  | "setup_failed"
  | "idle"
  | "archived";

export const WORKSPACE_STATUS_RANK_ORDER: readonly WorkspaceStatusRank[] = [
  "needs_you",
  "running",
  "pr_open",
  "done_unreviewed",
  "setup_failed",
  "idle",
  "archived",
];

export const WORKSPACE_STATUS_RANK_LABELS: Record<WorkspaceStatusRank, string> =
  {
    needs_you: "Needs you",
    running: "Running",
    pr_open: "PR open",
    done_unreviewed: "Done",
    setup_failed: "Setup failed",
    idle: "Idle",
    archived: "Archived",
  };

export const WORKSPACE_STATUS_RANK_TONES: Record<
  WorkspaceStatusRank,
  StatusTone
> = {
  needs_you: "critical",
  running: "running",
  pr_open: "ready",
  done_unreviewed: "neutral",
  setup_failed: "warning",
  idle: "neutral",
  archived: "neutral",
};

export function isWorkspaceStatusRank(
  value: string,
): value is WorkspaceStatusRank {
  return (WORKSPACE_STATUS_RANK_ORDER as readonly string[]).includes(value);
}

/**
 * Rank a workspace for the by-status rail. Needs-you wins, then a running
 * engine, then an open PR, then done-unreviewed, then a workspace whose setup
 * script failed, then idle. Archived is last. A digest may change the rank;
 * viewing or selecting never does.
 *
 * Stalled and fenced join needs-you so the card mark and the group agree.
 * Idle or ended sessions with turns join Done, matching
 * `attentionMarkForDigest`. A failed setup ranks below live work because the
 * checkout survives — but above idle, because nothing else on the card says
 * the script never finished.
 */
export function workspaceStatusRank(
  workspace: CodeWorkspaceSnapshot,
  digest: CodeSessionDigest | undefined,
): WorkspaceStatusRank {
  if (isPutAway(workspace)) return "archived";
  const attentionType = digest?.attention.state.type;
  if (
    attentionType === "needs_you" ||
    attentionType === "stalled" ||
    attentionType === "fenced"
  ) {
    return "needs_you";
  }
  if (digest?.lifecycle === "running") return "running";
  const pr = digest?.pr_state ?? workspace.pr;
  if (pr) {
    const lifecycle = pullRequestLifecycle(pr);
    if (lifecycle === "open" || lifecycle === "draft") return "pr_open";
  }
  if (
    attentionType === "done_unreviewed" ||
    (digest !== undefined && digest.turn_count > 0)
  ) {
    return "done_unreviewed";
  }
  if (workspace.status === "setup_failed") return "setup_failed";
  return "idle";
}

export type WorkspaceCardStatus = {
  rank: WorkspaceStatusRank;
  tone: StatusTone;
  label: string;
};

/** The rank, tone, and label the card and by-status headers share. */
export function workspaceCardStatus(
  workspace: CodeWorkspaceSnapshot,
  digest: CodeSessionDigest | undefined,
): WorkspaceCardStatus {
  const rank = workspaceStatusRank(workspace, digest);
  return {
    rank,
    tone: WORKSPACE_STATUS_RANK_TONES[rank],
    label: WORKSPACE_STATUS_RANK_LABELS[rank],
  };
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
    workspaces.filter((workspace) => !isPutAway(workspace)),
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
 * Archived workspaces are always off the rail. Their full catalog lives on
 * the dedicated Archive page, so put-away work never interleaves with live
 * triage.
 */
export function arrangeWorkspaces(
  mode: WorkspaceSortMode,
  repos: readonly CodeRepoSnapshot[],
  workspaces: readonly CodeWorkspaceSnapshot[],
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>,
): ArrangedWorkspaceGroup[] {
  return arrangeLiveWorkspaces(mode, repos, workspaces, digests);
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
    .filter((workspace) => isPutAway(workspace))
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
  // A created session with no turns and no engine is still an empty
  // workspace. Everything else belongs to an agent, including a parked
  // idle one — hiding that row makes the card look unused.
  if (digest.lifecycle === "created" && digest.turn_count === 0) {
    return digest.harness_kind !== undefined;
  }
  return true;
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
      if (
        (digest.lifecycle === "idle" || digest.lifecycle === "ended") &&
        digest.turn_count > 0
      ) {
        return "Done";
      }
      return LIFECYCLE_LABELS[digest.lifecycle];
  }
}

/**
 * Copy for the workspace card's session line.
 *
 * A live turn names what it is doing right now: the command, path, or query
 * the engine is waiting on when the server captured one, the activity
 * category otherwise. A parked turn prefers the recap when one exists — that
 * is the complete-state read — and falls back to the short lifecycle word so
 * the agent is still visible. No tallies: how many turns have run is not
 * what a reader scanning the rail wants to know.
 */
export function sessionActivityLineLabel(digest: CodeSessionDigest): string {
  if (digest.attention.state.type === "needs_you") {
    return digest.attention.state.prompt || "Needs you";
  }
  if (digest.lifecycle === "running") {
    // Subagents keep their count: the child rows name them. Everything else
    // shows the tool's own subject when the digest carries one, so a reader
    // sees `cargo test -p tidebreak-server` rather than "Shell running".
    const detail = digest.activity_detail?.trim();
    if (detail && !hasRunningSubagents(digest)) return detail;
    return sessionActivityLabel(digest);
  }
  const recap = digest.recap?.trim();
  if (recap) return recap;
  return sessionRowLabel(digest);
}

/** Coarse running-state copy: the activity category, never a tool subject. */
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

function hasRunningSubagents(digest: CodeSessionDigest): boolean {
  return (
    digest.activity === "subagents" ||
    (digest.subagents?.some((entry) => entry.status === "running") ?? false)
  );
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
  pr?: {
    number: number;
    state: string;
    draft?: boolean;
    in_merge_queue?: boolean;
  };
  terminalOpen?: boolean;
  workspaceStatus?: CodeWorkspaceStatus;
}): string {
  const parts = [input.title];
  if (input.workspaceStatus === "creating") parts.push("Creating workspace");
  if (input.workspaceStatus === "setup_failed") parts.push("Setup failed");
  if (
    input.attention &&
    input.attention.state.type !== "working" &&
    input.attention.state.type !== "idle"
  ) {
    parts.push(attentionLabel(input.attention));
  }
  if (input.session?.lifecycle === "running") {
    parts.push(sessionActivityLabel(input.session));
  } else if (
    input.session &&
    (input.attention?.state.type === "working" ||
      input.attention?.state.type === "idle")
  ) {
    parts.push(sessionRowLabel(input.session));
  }
  if (input.pr) {
    parts.push(
      `Pull request #${input.pr.number} ${prCompactStatusLabel(input.pr)}`,
    );
  }
  if (input.terminalOpen) parts.push("Terminal open");
  parts.push(input.repoName);
  if (input.branchName) parts.push(input.branchName);
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
  "bg-icon-blue/80",
  "bg-icon-cyan/80",
  "bg-icon-amber/80",
  "bg-icon-rose/80",
  "bg-icon-violet/80",
  "bg-icon-green/80",
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

/**
 * The chip suffix for a workspace's attributed pull-request set
 * (decision 77): a count once there is more than one, nothing otherwise —
 * the single-PR chip already names its one pull request.
 */
export function workspacePrChipSummary(
  prCount: number | undefined,
): string | null {
  if (prCount === undefined || prCount <= 1) return null;
  return `${prCount} PRs`;
}

/**
 * The sibling workspace this one's branch is stacked on (decision 77): its
 * base ref names that sibling's branch in the same repository. An `origin/`
 * prefix strips, the way pull-request creation resolves the base. Stacks
 * are creatable today by basing a workspace on another's branch; this is
 * the read side.
 */
export function workspaceStackParent(
  workspace: { id: string; repo_id: string; base_ref: string },
  siblings: readonly {
    id: string;
    repo_id: string;
    branch_name: string;
    title: string;
  }[],
): { id: string; title: string } | null {
  const base = workspace.base_ref.replace(/^origin\//, "");
  if (!base) return null;
  const parent = siblings.find(
    (sibling) =>
      sibling.id !== workspace.id &&
      sibling.repo_id === workspace.repo_id &&
      sibling.branch_name === base,
  );
  return parent ? { id: parent.id, title: parent.title } : null;
}
