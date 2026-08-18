import type {
  CodeRepoSnapshot,
  CodeSessionLifecycle,
  CodeWorkspaceSnapshot,
  CodeWorkspaceStatus,
} from "../api/types";
import { LIFECYCLE_LABELS, WORKSPACE_STATUS_LABELS } from "./labels";

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
    groups.push({ repo, workspaces: listed });
  }
  const orphans = [...byRepo.values()].flat();
  if (orphans.length > 0) groups.push({ repo: null, workspaces: orphans });
  return groups;
}

/**
 * The one state word a card shows: workspace setup states win, then the
 * session lifecycle. Null when the workspace is active with no session yet —
 * an empty label reads better than "Created" on every fresh card.
 */
export function workspaceStateLabel(
  status: CodeWorkspaceStatus,
  lifecycle: CodeSessionLifecycle | undefined,
): string | null {
  if (status === "creating" || status === "setup_failed") {
    return WORKSPACE_STATUS_LABELS[status];
  }
  if (!lifecycle) return null;
  return LIFECYCLE_LABELS[lifecycle];
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
