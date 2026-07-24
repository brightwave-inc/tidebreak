import type { Chat } from "./api";

/** Retain every refreshed chat when a new loose replacement is required. */
export function prependReplacementChat(chats: Chat[], replacement: Chat): Chat[] {
  return [replacement, ...chats];
}
