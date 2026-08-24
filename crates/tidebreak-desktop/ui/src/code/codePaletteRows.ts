import {
  Archive,
  Bell,
  FileText,
  GitPullRequest,
  MessageSquare,
  Play,
  Plus,
  Terminal,
} from "lucide-react";

import type {
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
} from "../api/types";
import type { PaletteRow } from "@/CommandPalette";
import { SHELL_SHORTCUTS, type ShellShortcutAction } from "@/ShellShortcuts";
import { digestStatusTone } from "./statusTone";
import type { WorkflowSuggestion } from "./CodeUiStore";
import { formatCompactAge } from "./workspaceCards";
import type { WorkflowShortcut } from "./workspaceWorkflow";
import {
  workspaceCommands,
  type WorkspaceCommand,
  type WorkspaceCommandId,
} from "./workspaceActions";

/**
 * What code mode offers the palette.
 *
 * Rows are built here rather than in the palette so the palette never has to
 * know what a workspace is. Everything is a plain function over snapshots the
 * caller already holds, which is also what lets the ordering be tested without
 * a store.
 */

/**
 * The Ship chords, as palette rows.
 *
 * Both the label and the keycaps come from the shortcut table, so a command
 * that moves keys moves in the palette too and the row can never advertise a
 * chord that does something else.
 */
const SHIP_ROWS: readonly {
  action: ShellShortcutAction;
  shortcut: WorkflowShortcut;
}[] = [
  { action: "code-workflow-next", shortcut: "next" },
  { action: "code-create-pr", shortcut: "pull_request" },
  { action: "code-source-control", shortcut: "source_control" },
  { action: "code-update-branch", shortcut: "update_branch" },
  { action: "code-watch-pr", shortcut: "watch" },
  { action: "code-merge-pr", shortcut: "merge" },
  { action: "code-view-pr", shortcut: "view_pr" },
];

function shortcutDescription(action: ShellShortcutAction): string {
  return (
    SHELL_SHORTCUTS.find((def) => def.id === action && def.scope === "code")
      ?.description ?? action
  );
}

/**
 * The single row that leads the list: whatever this workspace's next step is.
 *
 * The workflow control already computes it for the header's primary button and
 * republishes it, so the palette shows the same answer rather than a second
 * opinion. Opening the palette on a branch that is ready to push and being
 * offered "Push" first is the whole reason to reach for it mid-flow.
 */
export function suggestedShipRow(input: {
  suggestion: WorkflowSuggestion | null;
  workspaceId?: string;
  onRun: (shortcut: WorkflowShortcut) => void;
}): PaletteRow[] {
  const { suggestion } = input;
  if (!suggestion || suggestion.workspaceId !== input.workspaceId) return [];
  return [
    {
      id: `suggested:${suggestion.label}`,
      section: "suggested",
      label: suggestion.label,
      hint: suggestion.summary,
      tone: suggestion.tone,
      shortcut: "code-workflow-next",
      // What is next changes with the branch, so remembering this row would
      // float a step the workspace has already moved past.
      transient: true,
      onSelect: () => input.onRun("next"),
    },
  ];
}

/** The Ship chords for the open workspace. */
export function shipPaletteRows(input: {
  onRun: (shortcut: WorkflowShortcut) => void;
}): PaletteRow[] {
  return SHIP_ROWS.map(({ action, shortcut }) => ({
    id: `ship:${shortcut}`,
    section: "ship",
    label: shortcutDescription(action),
    icon: GitPullRequest,
    shortcut: action,
    onSelect: () => input.onRun(shortcut),
  }));
}

/**
 * Every workspace, as a row that jumps to it.
 *
 * Ordered by the caller — the rail's own arrangement — and left that way,
 * because with nothing typed the palette should open on the same list the
 * reader already has a mental picture of. The digest supplies the accent, so a
 * workspace waiting on an answer is as visible here as it is on the rail.
 */
export function workspacePaletteRows(input: {
  workspaces: readonly CodeWorkspaceSnapshot[];
  repos: readonly CodeRepoSnapshot[];
  digests: Readonly<Record<string, CodeSessionDigest | undefined>>;
  activeWorkspaceId?: string;
  nowMs?: number;
  onOpen: (workspaceId: string) => void;
}): PaletteRow[] {
  const repoNames = new Map(
    input.repos.map((repo) => [repo.id, repo.display_name]),
  );
  return (
    input.workspaces
      // The one already on screen is not somewhere to go.
      .filter((workspace) => workspace.id !== input.activeWorkspaceId)
      .map((workspace) => {
        const digest = input.digests[workspace.id];
        const age = formatCompactAge(workspace.created_at, input.nowMs);
        const repo = repoNames.get(workspace.repo_id);
        return {
          id: `workspace:${workspace.id}`,
          section: "workspaces" as const,
          label: workspace.title,
          keywords: [workspace.branch_name, repo].filter(Boolean).join(" "),
          hint: [repo, age].filter(Boolean).join(" · "),
          tone: digestStatusTone(digest),
          onSelect: () => input.onOpen(workspace.id),
        };
      })
  );
}

/**
 * The open workspace's own commands, and the repo's quick actions.
 *
 * `workspaceCommands` is the same list the card's context menu reads, so the
 * palette is a third surface over one definition rather than a third
 * definition. A command that needs a name or a confirmation still gets one —
 * the palette only picks it, and `useWorkspaceCardCommands` runs it.
 *
 * `fork-agent` is left out: it opens into a tab, which only the workspace page
 * knows how to place.
 */
export function workspaceActionPaletteRows(input: {
  workspace: CodeWorkspaceSnapshot;
  hasPr: boolean;
  hasSession: boolean;
  attentionPinned: boolean;
  quickActions: readonly { name: string }[];
  onCommand: (command: WorkspaceCommand) => void;
}): PaletteRow[] {
  const archived = input.workspace.status === "archived";
  const commands = workspaceCommands({
    hasPr: input.hasPr,
    archived,
    hasSession: input.hasSession,
    attentionPinned: input.attentionPinned,
  });
  const quick: WorkspaceCommand[] = archived
    ? []
    : input.quickActions.map((action) => ({
        id: "run-quick-action" as const,
        label: `Run: ${action.name}`,
        actionName: action.name,
      }));
  return [...commands, ...quick]
    .filter(
      // "Open workspace" is where the reader already is, and a fork needs a
      // tab to land in.
      (command) => command.id !== "open" && command.id !== "fork-agent",
    )
    .map((command) => ({
      id: `action:${command.id}${command.actionName ? `:${command.actionName}` : ""}`,
      section: "actions" as const,
      label: command.label.replace(/…$/, ""),
      icon: ACTION_ICONS[command.id],
      shortcut: ACTION_SHORTCUTS[command.id],
      onSelect: () => input.onCommand(command),
    }));
}

const ACTION_ICONS: Partial<Record<WorkspaceCommandId, PaletteRow["icon"]>> = {
  "new-session": Plus,
  "toggle-terminal": Terminal,
  "open-pr": GitPullRequest,
  "run-quick-action": Play,
  archive: Archive,
  "force-archive": Archive,
  restore: Archive,
};

/** The commands a chord already reaches, so the row can teach it. */
const ACTION_SHORTCUTS: Partial<
  Record<WorkspaceCommandId, ShellShortcutAction>
> = {
  "toggle-terminal": "toggle-code-terminal",
  "open-pr": "code-view-pr",
  archive: "code-archive-workspace",
};

/** Where else code mode goes. */
export function codeNavigationPaletteRows(input: {
  navigate: (path: string) => void;
  onNewWorkspace: () => void;
  onQuickOpen: () => void;
}): PaletteRow[] {
  return [
    {
      id: "navigate:code-new-workspace",
      section: "actions",
      label: "New workspace",
      icon: Plus,
      shortcut: "code-new-workspace",
      onSelect: input.onNewWorkspace,
    },
    {
      id: "navigate:pull-requests",
      section: "navigate",
      label: "Pull requests",
      keywords: "delivery prs",
      icon: GitPullRequest,
      onSelect: () => input.navigate("/code/delivery/pull-requests"),
    },
    {
      id: "navigate:runs",
      section: "navigate",
      label: "Runs",
      keywords: "delivery checks ci",
      icon: Play,
      onSelect: () => input.navigate("/code/delivery/runs"),
    },
    {
      id: "navigate:notifications",
      section: "navigate",
      label: "Notifications",
      icon: Bell,
      onSelect: () => input.navigate("/code/notifications"),
    },
    {
      id: "navigate:archive",
      section: "navigate",
      label: "Archived workspaces",
      icon: Archive,
      onSelect: () => input.navigate("/code/archive"),
    },
    {
      id: "navigate:chat",
      section: "navigate",
      label: "Go to chat",
      keywords: "work conversations",
      icon: MessageSquare,
      onSelect: () => input.navigate("/"),
    },
    {
      id: "navigate:quick-open",
      section: "files",
      label: "Open a file by name…",
      keywords: "quick open goto file",
      icon: FileText,
      shortcut: "code-quick-open",
      // The tree is thousands of paths and a palette that loaded it on every
      // open would pay for a search nobody asked for. This row is the same
      // picker Cmd+P opens, for a reader who did not know the chord.
      transient: true,
      onSelect: input.onQuickOpen,
    },
  ];
}
