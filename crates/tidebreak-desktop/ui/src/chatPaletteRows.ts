import {
  Blocks,
  FolderOpen,
  Inbox,
  MessageSquare,
  Plus,
  SquareTerminal,
} from "lucide-react";

import type { Chat, Project } from "./api";
import type { PaletteRow } from "./CommandPalette";

/**
 * What chat mode offers the palette.
 *
 * The mirror of `codePaletteRows`, and deliberately smaller: chat mode has one
 * kind of thing in it. A conversation is reached by name, and everything else
 * is a place to go.
 */

/** The untitled fallback the rails already use, so the palette agrees with them. */
const UNTITLED = "New chat";

/**
 * Recent conversations, as rows that open them.
 *
 * Left in the order the caller passed — the chat list is already sorted by
 * what happened last — so with nothing typed the palette opens on the same
 * conversations the sidebar is showing.
 */
export function chatPaletteRows(input: {
  chats: readonly Chat[];
  projects: readonly Project[];
  activeChatId?: string | null;
  onOpen: (chat: Chat) => void;
}): PaletteRow[] {
  const projectNames = new Map(
    input.projects.map((project) => [project.id, project.title ?? "Project"]),
  );
  return (
    input.chats
      // The conversation already on screen is not somewhere to go.
      .filter((chat) => chat.id !== input.activeChatId)
      .map((chat) => {
        const project = chat.project_id
          ? projectNames.get(chat.project_id)
          : undefined;
        return {
          id: `chat:${chat.id}`,
          section: "chats" as const,
          label: chat.title ?? UNTITLED,
          hint: project,
          keywords: project,
          icon: MessageSquare,
          onSelect: () => input.onOpen(chat),
        };
      })
  );
}

/** Projects, as rows that open them. */
export function projectPaletteRows(input: {
  projects: readonly Project[];
  onOpen: (project: Project) => void;
}): PaletteRow[] {
  return input.projects.map((project) => ({
    id: `project:${project.id}`,
    section: "chats",
    label: project.title ?? "Project",
    hint: "Project",
    keywords: "project folder",
    icon: FolderOpen,
    onSelect: () => input.onOpen(project),
  }));
}

/** What chat mode does, and where else it goes. */
export function chatNavigationPaletteRows(input: {
  navigate: (path: string) => void;
  onNewChat: () => void;
}): PaletteRow[] {
  return [
    {
      id: "navigate:new-chat",
      section: "actions",
      label: "Start new work",
      keywords: "new chat conversation",
      icon: Plus,
      shortcut: "new-chat",
      onSelect: input.onNewChat,
    },
    {
      id: "navigate:inbox",
      section: "navigate",
      label: "Inbox",
      icon: Inbox,
      onSelect: () => input.navigate("/inbox"),
    },
    {
      id: "navigate:apps",
      section: "navigate",
      label: "Apps",
      icon: Blocks,
      onSelect: () => input.navigate("/apps"),
    },
    {
      id: "navigate:plugins",
      section: "navigate",
      label: "Plugins",
      keywords: "skills prompts library",
      icon: Blocks,
      onSelect: () => input.navigate("/plugins"),
    },
    {
      id: "navigate:code",
      section: "navigate",
      label: "Go to code",
      keywords: "workspaces worktrees",
      icon: SquareTerminal,
      onSelect: () => input.navigate("/code"),
    },
  ];
}
