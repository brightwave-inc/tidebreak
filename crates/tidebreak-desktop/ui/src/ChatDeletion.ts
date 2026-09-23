import type { AgentRun, Chat } from "./api";
import { HttpError } from "./api/client/http";
import {
  disconnectFolder,
  hasNativeHost,
  purgeDeletedConversationSubject,
} from "./host";
import { friendlyErrorMessage } from "./lib/utils";

/** Retain every refreshed chat when a new loose replacement is required. */
export function prependReplacementChat(
  chats: Chat[],
  replacement: Chat,
): Chat[] {
  return [replacement, ...chats];
}

/**
 * Disconnect every folder a chat holds, so the chat can then be deleted.
 *
 * `DELETE /chats/{id}` refuses a conversation with roots still attached: a
 * connected folder is native authority held by the host broker, not a row, so
 * the server deliberately never revokes it as a side effect of deletion. The
 * renderer is the one caller that can drive both halves, so it detaches first
 * rather than handing the reader a conflict to go resolve by hand.
 *
 * Detaching is sequential because each change is a compare-and-set against the
 * chat's attachment revision; concurrent detaches would lose the race and fail.
 */
export async function detachChatFolders(chat: Chat): Promise<void> {
  if (!hasNativeHost()) return;
  for (const attachment of chat.root_attachments) {
    await disconnectFolder(chat, attachment.root_id);
  }
}

/**
 * After the server has deleted the chat, drop any residual host-broker rows
 * still keyed to that conversation subject. Detach handles live attachments
 * first; this is the terminal cleanup so Permissions does not keep a deleted
 * chat's grants.
 */
export async function purgeDeletedChatHostAuthority(
  chatId: string,
): Promise<void> {
  if (!hasNativeHost()) return;
  await purgeDeletedConversationSubject(chatId);
}

/** How the delete confirmation describes what else goes with the chat. */
export function deletionDescription(
  folderCount: number,
  stopping = false,
): string {
  const folders =
    folderCount === 1
      ? "1 connected folder"
      : `${folderCount} connected folders`;
  if (stopping) {
    return folderCount < 1
      ? "This stops the running response and background agents, then deletes the conversation. This cannot be undone."
      : `This stops the running response and background agents, disconnects ${folders}, then deletes the conversation. This cannot be undone.`;
  }
  if (folderCount < 1) return "This cannot be undone.";
  return `Disconnects ${folders} first. This cannot be undone.`;
}

/** In-flight work that makes the server refuse `DELETE /chats/{id}`. */
export type LiveChatWork = {
  activeTurnId: string | null;
  backgroundRunIds: string[];
};

export function liveChatWorkIsBlocking(work: LiveChatWork): boolean {
  return work.activeTurnId !== null || work.backgroundRunIds.length > 0;
}

function isLiveBackgroundRun(run: AgentRun): boolean {
  return (
    run.tier === "background" &&
    run.status !== "completed" &&
    run.status !== "failed" &&
    run.status !== "cancelled"
  );
}

/**
 * What would make the server refuse a delete: a non-terminal turn on the open
 * chat, or a live background agent.
 */
export async function inspectLiveChatWork(args: {
  chatId: string;
  openChatId: string | null;
  session: { busy: boolean; activeTurnId: string | null };
  listAgentRuns: (chatId: string) => Promise<AgentRun[]>;
}): Promise<LiveChatWork> {
  const runs = await args.listAgentRuns(args.chatId);
  const backgroundRunIds = runs
    .filter(isLiveBackgroundRun)
    .map((run) => run.id);
  const activeTurnId =
    args.openChatId === args.chatId && args.session.busy
      ? args.session.activeTurnId
      : null;
  return { activeTurnId, backgroundRunIds };
}

export async function stopLiveChatWork(args: {
  chatId: string;
  work: LiveChatWork;
  cancelTurn: (chatId: string, turnId: string) => Promise<void>;
  cancelAgentRun: (chatId: string, runId: string) => Promise<void>;
}): Promise<void> {
  if (args.work.activeTurnId) {
    await args.cancelTurn(args.chatId, args.work.activeTurnId);
  }
  for (const runId of args.work.backgroundRunIds) {
    await args.cancelAgentRun(args.chatId, runId);
  }
}

/** Why the first delete refused, or that it succeeded. */
export type ChatDeleteAttempt =
  | "deleted"
  | "chat_active"
  | "chat_roots_attached";

/**
 * Ask the server to delete first. Detach folders only after it answers
 * `chat_roots_attached`; a running turn answers `chat_active` instead.
 */
export async function tryDeleteChat(
  deleteChat: () => Promise<void>,
): Promise<ChatDeleteAttempt> {
  try {
    await deleteChat();
    return "deleted";
  } catch (error) {
    if (error instanceof HttpError && error.kind === "chat_active") {
      return "chat_active";
    }
    if (error instanceof HttpError && error.kind === "chat_roots_attached") {
      return "chat_roots_attached";
    }
    throw error;
  }
}

/** Wait until cancel has made the conversation deletable, or give up. */
export async function waitForChatQuiescent(args: {
  inspect: () => Promise<LiveChatWork>;
  wait?: (ms: number) => Promise<void>;
  attempts?: number;
  intervalMs?: number;
}): Promise<boolean> {
  const wait =
    args.wait ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const attempts = args.attempts ?? 40;
  const intervalMs = args.intervalMs ?? 250;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const work = await args.inspect();
    if (!liveChatWorkIsBlocking(work)) return true;
    if (attempt + 1 < attempts) await wait(intervalMs);
  }
  return false;
}

/** Plain copy for a refused or failed chat delete. */
export function chatDeletionErrorMessage(error: unknown): string {
  if (error instanceof HttpError && error.kind === "chat_active") {
    return "Stop the active work before deleting this conversation, or choose Stop and delete.";
  }
  if (error instanceof HttpError && error.kind === "chat_roots_attached") {
    return "Disconnect connected folders before deleting this conversation.";
  }
  return friendlyErrorMessage(error, "Could not delete this work. Try again.");
}
