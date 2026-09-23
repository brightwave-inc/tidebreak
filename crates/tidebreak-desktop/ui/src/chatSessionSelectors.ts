import type { ChatMessage } from "./MessageList";

type UserMessage = Extract<ChatMessage, { role: "user" }>;
type ToolMessage = Extract<ChatMessage, { role: "tool" }>;
/** What the transcript's outline is made of: questions and tool calls. */
export type OutlineMessage = UserMessage | ToolMessage;

/**
 * A selector over a list that keeps returning the same array while the items
 * it keeps are the same objects.
 *
 * A streamed token replaces the transcript array, but it touches one message.
 * A subscriber that only reads the questions asked, or the tool calls made,
 * should neither re-render nor recompute for it. Each call site makes its own
 * selector, so two subscribers never share, or thrash, one cache.
 */
export function stableSubset<T extends U, U>(
  keep: (item: U) => item is T,
): (items: readonly U[]) => readonly T[] {
  let lastItems: readonly U[] | null = null;
  let lastKept: readonly T[] = [];
  return (items) => {
    if (items === lastItems) return lastKept;
    lastItems = items;
    // Compare in place first: on a token, nothing changes and nothing is
    // allocated.
    let count = 0;
    let same = true;
    for (const item of items) {
      if (!keep(item)) continue;
      if (lastKept[count] !== item) {
        same = false;
        break;
      }
      count += 1;
    }
    if (same && count === lastKept.length) return lastKept;
    lastKept = items.filter(keep);
    return lastKept;
  };
}

export function isOutlineMessage(
  message: ChatMessage,
): message is OutlineMessage {
  return message.role === "user" || message.role === "tool";
}

export function isUserMessage(message: ChatMessage): message is UserMessage {
  return message.role === "user";
}

export function isToolMessage(message: ChatMessage): message is ToolMessage {
  return message.role === "tool";
}
