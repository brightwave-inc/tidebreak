import { File, FolderOpen, FolderPlus, Paperclip } from "lucide-react";

import { activeTokenQuery } from "./ComposerSlash";
import { documentIcon } from "./documentIcon";
import type { OptionRow } from "@/components/OptionListbox";
import type { ConnectedFolder } from "./host";
import type { ImportedDocument } from "./documents";
import type { TranscriptFileAttachment } from "./TranscriptFileAttachments";

export const MENTION_LIST_LABEL = "Attach context";

/**
 * How many candidates one list offers.
 *
 * The popover sits over the draft, so it is a shortlist rather than a browser:
 * past this many rows the reader is scrolling a panel instead of typing a name,
 * and the query is the faster way to the row they want.
 */
export const MAX_MENTION_ROWS = 20;

/** Something already within reach that `@` can put on the next message. */
export type MentionCandidate =
  | {
      kind: "file";
      /** The library document id — the identity the message carries. */
      id: string;
      label: string;
      mediaType: string;
    }
  | { kind: "folder"; /** The broker's root id. */ id: string; label: string }
  | {
      kind: "path";
      /** Workspace-relative path the engine sees as plain text. */
      path: string;
      label: string;
    };

/** A row that hands off to the picker the tools menu opens. */
export type MentionAction = "browse-files" | "connect-folder";

export type MentionRow =
  | { kind: "candidate"; candidate: MentionCandidate }
  | { kind: "action"; action: MentionAction };

/** The `@` token under the caret: the way into attaching context. */
export function activeMentionQuery(
  draft: string,
  caret: number,
): { start: number; query: string } | null {
  return activeTokenQuery(draft, caret, "@");
}

/**
 * The files this conversation already carries, newest first and each named
 * once.
 *
 * Read off the transcript the renderer is already holding rather than fetched:
 * these are the attachments of messages on screen, so the list can never name a
 * document this chat does not own. A file the composer is already carrying is
 * left out — attaching it twice would put one document on the message twice.
 */
export function recentChatFiles(
  messages: readonly { files?: readonly TranscriptFileAttachment[] }[],
  attached: readonly ImportedDocument[],
): TranscriptFileAttachment[] {
  const seen = new Set(attached.map((file) => file.documentId));
  const recent: TranscriptFileAttachment[] = [];
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    for (const file of messages[index]?.files ?? []) {
      if (seen.has(file.documentId)) continue;
      seen.add(file.documentId);
      recent.push(file);
      if (recent.length >= MAX_MENTION_ROWS) return recent;
    }
  }
  return recent;
}

/**
 * What a query names, best match first, with the pickers last.
 *
 * Matched on the name alone: every candidate is something the reader has
 * already seen by name — a file they attached, a folder they approved — so a
 * name is what they are completing. A prefix sorts above a mid-word hit.
 *
 * The picker rows are ranked the same way but always sort after the candidates,
 * because they are the slower path: reaching one of these names without leaving
 * the keyboard is the whole point of the list. They are also what keeps `@` from
 * being a dead end in a conversation that has nothing to offer yet.
 */
export function mentionRows(
  candidates: readonly MentionCandidate[],
  actions: readonly MentionAction[],
  query: string,
): MentionRow[] {
  const matched = rank(candidates, (candidate) => candidate.label, query)
    .slice(0, MAX_MENTION_ROWS)
    .map((candidate): MentionRow => ({ kind: "candidate", candidate }));
  const offered = rank(actions, actionLabel, query).map(
    (action): MentionRow => ({ kind: "action", action }),
  );
  return [...matched, ...offered];
}

function rank<T>(
  items: readonly T[],
  nameOf: (item: T) => string,
  query: string,
): T[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...items];
  const ranked: { item: T; rank: number }[] = [];
  for (const item of items) {
    const name = nameOf(item).toLowerCase();
    const rank = name.startsWith(needle) ? 0 : name.includes(needle) ? 1 : -1;
    if (rank >= 0) ranked.push({ item, rank });
  }
  return ranked
    .sort((left, right) => left.rank - right.rank)
    .map((entry) => entry.item);
}

function actionLabel(action: MentionAction): string {
  return action === "browse-files" ? "Browse files…" : "Connect a folder…";
}

/** The listbox's view of the rows: an icon, a name, and what a pick will do. */
export function mentionOptionRows(rows: readonly MentionRow[]): OptionRow[] {
  return rows.map((row) => {
    if (row.kind === "action") {
      return {
        key: `action:${row.action}`,
        label: actionLabel(row.action),
        icon: row.action === "browse-files" ? Paperclip : FolderPlus,
        hint: "Browse",
      };
    }
    const { candidate } = row;
    if (candidate.kind === "file") {
      return {
        key: `file:${candidate.id}`,
        label: candidate.label,
        icon: documentIcon(candidate.mediaType),
        hint: "File",
      };
    }
    if (candidate.kind === "folder") {
      return {
        key: `folder:${candidate.id}`,
        label: candidate.label,
        icon: FolderOpen,
        hint: "Folder",
      };
    }
    return {
      key: `path:${candidate.path}`,
      label: candidate.label,
      icon: File,
      hint: "Path",
    };
  });
}

/**
 * The folders `@` can attach: everything approved on this device that is not
 * already on this conversation.
 *
 * Approval is what makes a folder reachable without a picker, and it outlives
 * the chat it was granted in — so the second conversation to want a folder can
 * name it instead of finding it on disk again.
 */
export function attachableFolders(
  approved: readonly ConnectedFolder[],
  attached: readonly { rootId: string }[],
): MentionCandidate[] {
  const already = new Set(attached.map((folder) => folder.rootId));
  return approved
    .filter((folder) => !already.has(folder.rootId))
    .map((folder) => ({
      kind: "folder",
      id: folder.rootId,
      label: folder.displayName,
    }));
}

/** The files `@` can attach, as candidates. */
export function attachableFiles(
  recent: readonly TranscriptFileAttachment[],
): MentionCandidate[] {
  return recent.map((file) => ({
    kind: "file",
    id: file.documentId,
    label: file.name,
    mediaType: file.mediaType,
  }));
}

/** Workspace paths `@` can insert as plain relative text. */
export function workspacePathCandidates(
  paths: readonly string[],
): MentionCandidate[] {
  return paths.map((path) => ({
    kind: "path",
    path,
    label: path,
  }));
}
