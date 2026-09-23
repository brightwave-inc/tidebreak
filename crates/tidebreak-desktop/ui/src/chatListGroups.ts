import type { Chat } from "./api";

/**
 * How the list of work is ordered and grouped.
 *
 * The server sends the list pinned first, then by latest activity; the store
 * keeps that order when it changes a row locally, and the rail cuts the
 * unpinned rows into date groups. Everything here is a pure function of the
 * rows and the clock, so the rail, the palette, and the tests agree.
 */

export type ChatListGroupKey =
  | "pinned"
  | "today"
  | "yesterday"
  | "previous-7-days"
  | "older";

export type ChatListGroup = {
  key: ChatListGroupKey;
  label: string;
  chats: Chat[];
};

const GROUP_LABELS: Record<ChatListGroupKey, string> = {
  pinned: "Pinned",
  today: "Today",
  yesterday: "Yesterday",
  "previous-7-days": "Previous 7 days",
  older: "Older",
};

function timestamp(value: string | null | undefined): number {
  if (!value) return 0;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

/**
 * The list's order: pinned first, most recently pinned on top, then the rest
 * by latest activity. The same order the server sends.
 */
export function compareChats(left: Chat, right: Chat): number {
  const pinned = timestamp(right.pinned_at) - timestamp(left.pinned_at);
  if (pinned !== 0) return pinned;
  const activity =
    timestamp(right.last_activity_at) - timestamp(left.last_activity_at);
  if (activity !== 0) return activity;
  const created = timestamp(right.created_at) - timestamp(left.created_at);
  if (created !== 0) return created;
  return right.id.localeCompare(left.id);
}

export function sortChats(chats: readonly Chat[]): Chat[] {
  return [...chats].sort(compareChats);
}

/**
 * Whether a conversation earns a row in the list.
 *
 * One nothing has happened in — no turn, no name, no pin — is a start the
 * reader has not made yet. It stays out of the list unless it is the one on
 * screen, so abandoning a new chat leaves nothing behind.
 */
export function isListableChat(chat: Chat, activeChatId?: string | null) {
  return (
    chat.turn_count > 0 ||
    Boolean(chat.title?.trim()) ||
    Boolean(chat.pinned_at) ||
    chat.id === activeChatId
  );
}

function startOfDay(date: Date): number {
  return new Date(
    date.getFullYear(),
    date.getMonth(),
    date.getDate(),
  ).getTime();
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** Which date group a moment of activity falls in, by the local calendar. */
export function activityGroup(
  lastActivityAt: string,
  now: Date,
): Exclude<ChatListGroupKey, "pinned"> {
  const today = startOfDay(now);
  const at = timestamp(lastActivityAt);
  if (at >= today) return "today";
  // Calendar days rather than 24-hour windows, so "Yesterday" means the date
  // before today even across a daylight-saving change.
  const yesterday = startOfDay(new Date(today - DAY_MS / 2));
  if (at >= yesterday) return "yesterday";
  const weekAgo = startOfDay(new Date(today - 7 * DAY_MS + DAY_MS / 2));
  if (at >= weekAgo) return "previous-7-days";
  return "older";
}

/**
 * Cut the list into Pinned, Today, Yesterday, Previous 7 days, and Older.
 *
 * Pinned rows keep their pin order and leave the date groups. Empty groups
 * are left out, so a short list shows only the headings it needs.
 */
export function groupChats(chats: readonly Chat[], now: Date): ChatListGroup[] {
  const buckets = new Map<ChatListGroupKey, Chat[]>();
  for (const chat of sortChats(chats)) {
    const key: ChatListGroupKey = chat.pinned_at
      ? "pinned"
      : activityGroup(chat.last_activity_at, now);
    const bucket = buckets.get(key);
    if (bucket) bucket.push(chat);
    else buckets.set(key, [chat]);
  }
  return (Object.keys(GROUP_LABELS) as ChatListGroupKey[]).flatMap((key) => {
    const bucket = buckets.get(key);
    return bucket ? [{ key, label: GROUP_LABELS[key], chats: bucket }] : [];
  });
}
