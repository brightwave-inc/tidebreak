import type {
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import {
  checkCounts,
  prStatus,
  pullRequestReviewSummary,
  type PullRequestLifecycle,
} from "./prState";
import { recoveryDigest } from "./sessionRecovery";
import { sessionTreeWaitLabel } from "./sessionTree";
import { digestStatusTone, type StatusTone } from "./statusTone";
import {
  conversationSourceLabel,
  conversationTitle,
  isPutAway,
  isReadyToMergeAttention,
  isSessionRowWorthy,
  readyToMergeNotice,
  sessionActivityLineLabel,
  sessionStatusRank,
  watchRowLabel,
  workspaceStatusRank,
} from "./workspaceCards";

/**
 * What the Code home leads with, derived from state the app already holds.
 *
 * The home is an Orienting surface: it answers "what needs me now?" before
 * anything else. It reads three things and nothing more — each workspace's
 * digest from the live updates channel (attention and run state), the pull
 * request that digest carries as the one store projects it (decision 66),
 * and the workspace-less conversations on the same channel. It calls no
 * endpoint of its own and owns no timer, so opening it costs nothing against
 * the reader's GitHub budget.
 *
 * Every live workspace and conversation lands in exactly one section, the
 * strongest claim first: a need, then live work, then a pull request that is
 * ready to merge, then recent work.
 */

export type CodeHomeSectionId =
  | "needs_you"
  | "running"
  | "ready_to_merge"
  | "recent";

export const CODE_HOME_SECTION_ORDER: readonly CodeHomeSectionId[] = [
  "needs_you",
  "running",
  "ready_to_merge",
  "recent",
];

export const CODE_HOME_SECTION_LABELS: Record<CodeHomeSectionId, string> = {
  needs_you: "Needs you",
  running: "Running",
  ready_to_merge: "Ready to merge",
  recent: "Recent work",
};

/** Rows a section shows before it offers the rest behind View all. */
export const CODE_HOME_SECTION_LIMIT = 5;

/** A pull request as Delivery addresses it: its repository and its number. */
export type CodeHomePullRequestTarget = {
  repository: { host: string; owner: string; name: string };
  number: number;
};

/** Where an item opens. */
export type CodeHomeTarget =
  | { kind: "workspace"; workspaceId: string; task?: string }
  | { kind: "session"; sessionId: string }
  | ({ kind: "pull_request" } & CodeHomePullRequestTarget);

/**
 * The one mark a row leads with. A digest draws the rail's own session glyph,
 * so the home and the rail never disagree about a conversation.
 */
export type CodeHomeGlyph =
  | { kind: "session"; digest: CodeSessionDigest }
  | { kind: "pull_request"; lifecycle: PullRequestLifecycle; tone: StatusTone }
  | { kind: "alert"; tone: StatusTone }
  | { kind: "live" }
  | { kind: "idle" };

/** Why an item is in its section, in the tone that paints it. */
export type CodeHomeStatus = {
  label: string;
  tone: StatusTone;
  /** A turn is moving right now: muted ink with the live shimmer. */
  live?: boolean;
};

export type CodeHomeItem = {
  key: string;
  section: CodeHomeSectionId;
  title: string;
  status: CodeHomeStatus | null;
  /** The repository, or where a conversation came from. */
  context: string | null;
  /** What tells two items apart once the titles match. */
  reference:
    | { kind: "branch"; name: string }
    | { kind: "pull_request"; number: number }
    | null;
  glyph: CodeHomeGlyph;
  /** Newest turn start, or creation before the first turn. */
  activityAt: string | null;
  target: CodeHomeTarget;
};

export type CodeHomeSections = Record<CodeHomeSectionId, CodeHomeItem[]> & {
  /** Every live workspace and top-level conversation, whatever its section. */
  total: number;
};

export type CodeHomeInput = {
  repos: readonly Pick<CodeRepoSnapshot, "id" | "display_name">[];
  workspaces: readonly CodeWorkspaceSnapshot[];
  /** One digest per workspace, as the rail collapses them. */
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>;
  /** Watch digests, keyed workspace → session. */
  watches?: Readonly<
    Record<string, Readonly<Record<string, CodeSessionDigest>>>
  >;
  /** Conversations that belong to no workspace. */
  conversations?: readonly CodeSessionDigest[];
};

export function codeHomeSections(input: CodeHomeInput): CodeHomeSections {
  const sections: CodeHomeSections = {
    needs_you: [],
    running: [],
    ready_to_merge: [],
    recent: [],
    total: 0,
  };
  const repoNames = new Map(
    input.repos.map((repo) => [repo.id, repo.display_name] as const),
  );
  for (const workspace of input.workspaces) {
    if (!isHomeWorkspace(workspace)) continue;
    const item = workspaceItem(
      workspace,
      input.digests[workspace.id],
      Object.values(input.watches?.[workspace.id] ?? {}),
      repoNames.get(workspace.repo_id) ?? workspace.repo_display_name ?? null,
    );
    sections[item.section].push(item);
    sections.total += 1;
  }
  const conversations = input.conversations ?? [];
  const sessionIds = new Set(conversations.map((digest) => digest.session));
  for (const digest of conversations) {
    // A child conversation is drawn under its parent on the rail, so it only
    // earns a row of its own here when it is the one waiting on the reader.
    const child =
      digest.parent_session !== undefined &&
      digest.parent_session !== digest.session &&
      sessionIds.has(digest.parent_session);
    const item = conversationItem(digest);
    if (child && item.section !== "needs_you") continue;
    sections[item.section].push(item);
    if (!child) sections.total += 1;
  }
  sections.needs_you.sort(byUrgency);
  sections.running.sort(byRecency);
  sections.ready_to_merge.sort(byRecency);
  sections.recent.sort(byRecency);
  return sections;
}

/** A workspace the home lists: live, and past the create step. */
function isHomeWorkspace(workspace: CodeWorkspaceSnapshot): boolean {
  return (
    !isPutAway(workspace) &&
    workspace.status !== "creating" &&
    workspace.status !== "archiving"
  );
}

function workspaceItem(
  workspace: CodeWorkspaceSnapshot,
  rawDigest: CodeSessionDigest | undefined,
  watches: readonly CodeSessionDigest[],
  repoName: string | null,
): CodeHomeItem {
  const digest = rawDigest ? recoveryDigest(rawDigest) : undefined;
  const pr = digest?.pr_state ?? workspace.pr;
  const base = {
    key: `workspace:${workspace.id}`,
    title: digest?.title?.trim() || workspace.title,
    context: repoName,
    reference: workspace.branch_name
      ? { kind: "branch" as const, name: workspace.branch_name }
      : null,
    activityAt: digest?.trigger_target_at ?? workspace.created_at,
    target: { kind: "workspace" as const, workspaceId: workspace.id },
  };
  const rank = workspaceStatusRank(workspace, digest);

  // The conversation is waiting on the reader: an approval, a question, a
  // failed turn, or a turn that went quiet and stopped.
  if (rank === "needs_you" && digest) {
    return {
      ...base,
      section: "needs_you",
      status: needStatus(digest),
      glyph: { kind: "session", digest },
    };
  }

  // A watch task stopped on something only the reader can settle.
  const stuckWatch = watches.find(
    (watch) =>
      watch.attention.state.type === "needs_you" ||
      watch.watch_state === "blocked",
  );
  if (stuckWatch) {
    const need = stuckWatch.attention.state;
    const status: CodeHomeStatus =
      need.type === "needs_you"
        ? { label: sentence(need.prompt || "Needs you"), tone: "critical" }
        : { label: "Watch is blocked", tone: "warning" };
    return {
      ...base,
      section: "needs_you",
      status,
      glyph: { kind: "alert", tone: status.tone },
      target: { ...base.target, task: stuckWatch.session },
    };
  }

  if (rank === "running" && digest) {
    return {
      ...base,
      section: "running",
      status: runningStatus(digest, sessionActivityLineLabel(digest, pr)),
      glyph: { kind: "session", digest },
    };
  }

  // A watch fixing the pull request is an agent at work, even when the
  // conversation beside it is parked.
  const fixingWatch = watches.find(
    (watch) =>
      watch.watch_state === "fixing" ||
      (watch.watch_state === undefined && watch.lifecycle === "running"),
  );
  if (fixingWatch) {
    return {
      ...base,
      section: "running",
      status: {
        label: `Watch: ${watchRowLabel(fixingWatch)}`,
        tone: "running",
        live: true,
      },
      glyph: { kind: "live" },
      target: { ...base.target, task: fixingWatch.session },
    };
  }

  if (pr) {
    const status = prStatus(pr);
    const glyph: CodeHomeGlyph = {
      kind: "pull_request",
      lifecycle: status.lifecycle,
      tone: status.headline.tone,
    };
    const reference = { kind: "pull_request" as const, number: pr.number };
    // Failed checks, requested changes, conflicts, and stale branches: the
    // same group Delivery files under "Needs your attention".
    if (status.group === "attention") {
      return {
        ...base,
        reference,
        section: "needs_you",
        status: status.headline,
        glyph,
      };
    }
    // The classifier decides readiness. A watch's "ready to merge" notice
    // only fills in while the host has not yet said whether it can merge.
    if (
      status.gate === "ready" ||
      (status.gate === "checking" &&
        readyToMergeNotice(digest?.attention, pr) === "ready")
    ) {
      return {
        ...base,
        key: `pull_request:${workspace.id}:${pr.number}`,
        section: "ready_to_merge",
        title: pr.title?.trim() || base.title,
        reference,
        status: readyStatus(pr),
        glyph: { ...glyph, tone: "ready" },
        target: deliveryPullRequestTarget(pr) ?? base.target,
      };
    }
    return {
      ...base,
      reference,
      section: "recent",
      status:
        workspace.status === "setup_failed"
          ? SETUP_FAILED_STATUS
          : status.headline,
      glyph,
    };
  }

  if (workspace.status === "setup_failed") {
    return {
      ...base,
      section: "recent",
      status: SETUP_FAILED_STATUS,
      glyph: { kind: "alert", tone: "critical" },
    };
  }
  const worthy = digest && isSessionRowWorthy(digest) ? digest : undefined;
  return {
    ...base,
    section: "recent",
    status: worthy
      ? { label: sessionActivityLineLabel(worthy, pr), tone: "neutral" }
      : null,
    glyph: worthy ? { kind: "session", digest: worthy } : { kind: "idle" },
  };
}

function conversationItem(rawDigest: CodeSessionDigest): CodeHomeItem {
  const digest = recoveryDigest(rawDigest);
  const base = {
    key: `session:${digest.session}`,
    title: conversationTitle(digest),
    context: conversationSourceLabel(digest) ?? null,
    reference: null,
    activityAt: digest.trigger_target_at ?? null,
    target: { kind: "session" as const, sessionId: digest.session },
    glyph: { kind: "session" as const, digest },
  };
  const rank = sessionStatusRank(digest);
  if (rank === "needs_you" && !isReadyToMergeAttention(digest.attention)) {
    return { ...base, section: "needs_you", status: needStatus(digest) };
  }
  if (rank === "running") {
    return {
      ...base,
      section: "running",
      status: runningStatus(
        digest,
        sessionTreeWaitLabel(digest.wait) ?? sessionActivityLineLabel(digest),
      ),
    };
  }
  return {
    ...base,
    section: "recent",
    status: isSessionRowWorthy(digest)
      ? { label: sessionActivityLineLabel(digest), tone: "neutral" }
      : null,
  };
}

const SETUP_FAILED_STATUS = {
  label: "Setup failed",
  tone: "critical",
} as const satisfies CodeHomeStatus;

function needStatus(digest: CodeSessionDigest): CodeHomeStatus {
  const attention = digest.attention.state;
  if (attention.type === "needs_you") {
    return {
      label: sentence(attention.prompt || "Needs you"),
      tone: "critical",
    };
  }
  return { label: "Stalled", tone: "warning" };
}

/**
 * The section heading already says ready to merge, so a ready row says what
 * got it there: the approval, and the checks that passed.
 */
function readyStatus(pr: PullRequestDigest): CodeHomeStatus {
  const passing = checkCounts(pr).passing;
  const parts = [
    pullRequestReviewSummary(pr).tone === "ready" ? "Approved" : null,
    passing > 0
      ? `${passing} ${passing === 1 ? "check" : "checks"} passed`
      : null,
  ].filter((part): part is string => part !== null);
  return { label: parts.join(", ") || "Ready to merge", tone: "ready" };
}

/**
 * Live work keeps muted ink and the shimmer while a turn is actually moving
 * (DESIGN.md, live labels). A turn that went quiet or is reconnecting says so
 * in its own tone instead.
 */
function runningStatus(
  digest: CodeSessionDigest,
  label: string,
): CodeHomeStatus {
  return {
    label,
    tone: digestStatusTone(digest),
    live:
      digest.lifecycle === "running" &&
      digest.attention.state.type === "working",
  };
}

/**
 * Where Delivery finds a pull request, read from the host URL the digest
 * carries — the digest has no separate owner and name. Null when the URL is
 * missing or is not a pull request page; the row then opens the workspace,
 * whose header shows the same pull request.
 */
export function deliveryPullRequestTarget(
  pr: Pick<PullRequestDigest, "number" | "url">,
): ({ kind: "pull_request" } & CodeHomePullRequestTarget) | null {
  if (!pr.url) return null;
  let url: URL;
  try {
    url = new URL(pr.url);
  } catch {
    return null;
  }
  const [owner, name, kind, number] = url.pathname.split("/").filter(Boolean);
  if (!owner || !name || kind !== "pull" || Number(number) !== pr.number) {
    return null;
  }
  return {
    kind: "pull_request",
    repository: { host: url.host, owner, name },
    number: pr.number,
  };
}

/** Server prompts are sentence fragments; a row reads them as sentences. */
function sentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

const TONE_URGENCY: Partial<Record<StatusTone, number>> = {
  critical: 0,
  warning: 1,
};

/** Failures before soft blocks, then the newest activity first. */
function byUrgency(left: CodeHomeItem, right: CodeHomeItem): number {
  const urgency =
    (TONE_URGENCY[left.status?.tone ?? "neutral"] ?? 2) -
    (TONE_URGENCY[right.status?.tone ?? "neutral"] ?? 2);
  return urgency || byRecency(left, right);
}

function byRecency(left: CodeHomeItem, right: CodeHomeItem): number {
  return (
    (right.activityAt ?? "").localeCompare(left.activityAt ?? "") ||
    left.key.localeCompare(right.key)
  );
}
