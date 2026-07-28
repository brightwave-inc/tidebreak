import type { Chat } from "./api";
import { disconnectFolder, hasNativeHost } from "./host";

/** Retain every refreshed chat when a new loose replacement is required. */
export function prependReplacementChat(chats: Chat[], replacement: Chat): Chat[] {
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

/** How the delete confirmation describes what else goes with the chat. */
export function deletionDescription(folderCount: number): string {
  if (folderCount < 1) return "This cannot be undone.";
  const folders =
    folderCount === 1 ? "1 connected folder" : `${folderCount} connected folders`;
  return `Disconnects ${folders} first. This cannot be undone.`;
}
