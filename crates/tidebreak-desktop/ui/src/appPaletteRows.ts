import {
  Bell,
  Download,
  FolderInput,
  FolderMinus,
  Keyboard,
  Monitor,
  Moon,
  Pencil,
  RefreshCw,
  Sun,
  Trash2,
  ZoomIn,
  ZoomOut,
  Search,
} from "lucide-react";

import type { Chat, Project } from "./api";
import type { PaletteRow } from "./CommandPalette";
import type { ThemeMode } from "./theme";

/**
 * Commands the palette offers wherever the reader is: the app's own panels
 * and preferences, and what can be done to the conversation on screen.
 *
 * Each row runs an action the app already has — the rename box in the
 * header, the delete confirmation, the move the rail's menu makes, the
 * export on the Data page, the native menu's update check — so the palette
 * learns no second way to do any of them.
 */

/** Find in the conversation on screen, from either mode. */
export function findPaletteRow(onFind: () => void, label: string): PaletteRow {
  return {
    id: "conversation:find",
    section: "actions",
    label,
    keywords: "search messages text find",
    icon: Search,
    shortcut: "find-in-transcript",
    transient: true,
    movesFocus: true,
    onSelect: onFind,
  };
}

/** What the palette can do to the Work chat on screen. */
export function currentChatPaletteRows(input: {
  chat: Chat;
  projects: readonly Project[];
  onRename: () => void;
  onDelete: () => void;
  onMove: (projectId: string | null) => void;
  onExport: () => void;
  onFind: () => void;
}): PaletteRow[] {
  const title = input.chat.title?.trim() || "this conversation";
  const rows: PaletteRow[] = [
    findPaletteRow(input.onFind, "Find in this conversation"),
    {
      id: "chat:rename",
      section: "actions",
      label: "Rename this conversation",
      hint: title,
      keywords: "title name chat conversation",
      icon: Pencil,
      transient: true,
      movesFocus: true,
      onSelect: input.onRename,
    },
    {
      id: "chat:export",
      section: "actions",
      label: "Export this conversation",
      hint: "Markdown",
      keywords: "download save conversation transcript markdown",
      icon: Download,
      transient: true,
      onSelect: input.onExport,
    },
  ];
  const current = input.chat.project_id ?? null;
  for (const project of input.projects) {
    if (project.id === current) continue;
    rows.push({
      id: `chat:move:${project.id}`,
      section: "actions",
      label: `Move to ${project.title ?? "Project"}`,
      hint: "Project",
      keywords: "move project folder file",
      icon: FolderInput,
      transient: true,
      onSelect: () => input.onMove(project.id),
    });
  }
  if (current !== null) {
    rows.push({
      id: "chat:move:none",
      section: "actions",
      label: "Remove from project",
      keywords: "move project folder out",
      icon: FolderMinus,
      transient: true,
      onSelect: () => input.onMove(null),
    });
  }
  rows.push({
    id: "chat:delete",
    section: "actions",
    label: "Delete this conversation",
    hint: title,
    keywords: "remove chat conversation trash",
    icon: Trash2,
    transient: true,
    onSelect: input.onDelete,
  });
  return rows;
}

const THEME_ROWS: {
  mode: ThemeMode;
  label: string;
  icon: PaletteRow["icon"];
}[] = [
  { mode: "light", label: "Use the light theme", icon: Sun },
  { mode: "dark", label: "Use the dark theme", icon: Moon },
  { mode: "system", label: "Match the system theme", icon: Monitor },
];

/** The app's panels and view preferences, in both modes. */
export function appPaletteRows(input: {
  theme: ThemeMode;
  onTheme: (mode: ThemeMode) => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
  onNotifications: () => void;
  onShortcuts?: () => void;
  /** Absent where the app cannot update itself, such as a browser. */
  onCheckForUpdates?: () => void;
}): PaletteRow[] {
  const rows: PaletteRow[] = [
    {
      id: "app:notifications",
      section: "navigate",
      label: "Notifications",
      keywords: "alerts inbox bell unread",
      icon: Bell,
      onSelect: input.onNotifications,
    },
  ];
  if (input.onShortcuts) {
    rows.push({
      id: "app:shortcuts",
      section: "navigate",
      label: "Keyboard shortcuts",
      keywords: "keys chords help hotkeys",
      icon: Keyboard,
      shortcut: "show-shortcuts",
      onSelect: input.onShortcuts,
    });
  }
  if (input.onCheckForUpdates) {
    rows.push({
      id: "app:check-for-updates",
      section: "actions",
      label: "Check for updates",
      keywords: "update upgrade version release",
      icon: RefreshCw,
      onSelect: input.onCheckForUpdates,
    });
  }
  for (const theme of THEME_ROWS) {
    rows.push({
      id: `app:theme:${theme.mode}`,
      section: "settings",
      label: theme.label,
      hint: input.theme === theme.mode ? "Current" : undefined,
      keywords: "theme appearance color mode dark light system",
      icon: theme.icon,
      onSelect: () => input.onTheme(theme.mode),
    });
  }
  rows.push(
    {
      id: "app:zoom-in",
      section: "settings",
      label: "Zoom in",
      keywords: "larger bigger interface scale size",
      icon: ZoomIn,
      shortcut: "zoom-in",
      transient: true,
      onSelect: input.onZoomIn,
    },
    {
      id: "app:zoom-out",
      section: "settings",
      label: "Zoom out",
      keywords: "smaller interface scale size",
      icon: ZoomOut,
      shortcut: "zoom-out",
      transient: true,
      onSelect: input.onZoomOut,
    },
    {
      id: "app:zoom-reset",
      section: "settings",
      label: "Reset zoom",
      keywords: "actual size interface scale default",
      icon: ZoomIn,
      shortcut: "zoom-reset",
      transient: true,
      onSelect: input.onZoomReset,
    },
  );
  return rows;
}
