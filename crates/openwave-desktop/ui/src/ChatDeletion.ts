import type { Chat } from "./api";

/** Choose an existing post-delete selection without collapsing loose scope. */
export function existingChatAfterDeletion(
  chats: Chat[],
  deletedProjectId: string | null,
): Chat | undefined {
  const sameScope = chats.find((chat) => chat.project_id === deletedProjectId);
  if (sameScope) return sameScope;
  return deletedProjectId === null ? undefined : chats[0];
}

/** Retain every refreshed chat when a new loose replacement is required. */
export function prependReplacementChat(chats: Chat[], replacement: Chat): Chat[] {
  return [replacement, ...chats];
}
