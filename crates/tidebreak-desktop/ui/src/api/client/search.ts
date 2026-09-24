import type {
  MessageSearchHit,
  MessageSearchIndexing,
  MessageSearchPage,
  MessageSearchRange,
} from "../../generated/wire";
import { type Constructor, HttpCore, requireParsed } from "./http";

/** What `GET /search/messages` is asked for. */
export type MessageSearchQuery = {
  /** The words to find. */
  q: string;
  /** Hits per page, 1 to 50. The server answers 20 when absent. */
  limit?: number;
  /** The `next_cursor` of the page before. */
  cursor?: string;
  /** Search only this conversation: a Work chat or a code session. */
  sessionId?: string;
};

/** Full-text search over the caller's own chats and code sessions. */
export function withSearchApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async searchMessages(
      query: MessageSearchQuery,
      signal?: AbortSignal,
    ): Promise<MessageSearchPage> {
      const params = new URLSearchParams({ q: query.q });
      if (query.limit !== undefined) params.set("limit", String(query.limit));
      if (query.cursor) params.set("cursor", query.cursor);
      if (query.sessionId) params.set("session_id", query.sessionId);
      return requireParsed(
        parseMessageSearchPage(
          await this.json(`/search/messages?${params}`, {
            headers: this.headers(),
            signal,
          }),
        ),
        "message search",
      );
    }
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

function parseRange(value: unknown): MessageSearchRange | null {
  if (!isRecord(value)) return null;
  const { start, end } = value;
  if (!Number.isInteger(start) || !Number.isInteger(end)) return null;
  if ((start as number) < 0 || (end as number) < (start as number)) {
    return null;
  }
  return { start: start as number, end: end as number };
}

function parseHit(value: unknown): MessageSearchHit | null {
  if (!isRecord(value)) return null;
  const kind = value.kind;
  const source = value.source;
  if (kind !== "chat" && kind !== "code") return null;
  if (source !== "user" && source !== "assistant" && source !== "tool") {
    return null;
  }
  if (typeof value.session_id !== "string") return null;
  if (typeof value.snippet !== "string") return null;
  if (typeof value.created_at !== "string") return null;
  if (typeof value.archived !== "boolean") return null;
  if (
    !optionalString(value.workspace_id) ||
    !optionalString(value.title) ||
    !optionalString(value.turn_id) ||
    !optionalString(value.message_id)
  ) {
    return null;
  }
  if (value.event_seq !== undefined && !Number.isInteger(value.event_seq)) {
    return null;
  }
  if (!Array.isArray(value.ranges)) return null;
  const ranges: MessageSearchRange[] = [];
  for (const entry of value.ranges) {
    const range = parseRange(entry);
    if (!range) return null;
    ranges.push(range);
  }
  return {
    kind,
    session_id: value.session_id,
    workspace_id: value.workspace_id as string | undefined,
    title: value.title as string | undefined,
    turn_id: value.turn_id as string | undefined,
    message_id: value.message_id as string | undefined,
    event_seq: value.event_seq as number | undefined,
    source,
    snippet: value.snippet,
    ranges,
    created_at: value.created_at,
    archived: value.archived,
  };
}

function parseIndexing(value: unknown): MessageSearchIndexing | null {
  if (!isRecord(value)) return null;
  const { complete, pending_conversations, failed_conversations } = value;
  if (typeof complete !== "boolean") return null;
  if (!Number.isInteger(pending_conversations)) return null;
  // A server from before give-ups were reported sends no count.
  const failed = failed_conversations ?? 0;
  if (!Number.isInteger(failed)) return null;
  return {
    complete,
    pending_conversations: pending_conversations as number,
    failed_conversations: failed as number,
  };
}

/** A search page, or `null` when the body is not one. */
export function parseMessageSearchPage(
  value: unknown,
): MessageSearchPage | null {
  if (!isRecord(value) || !Array.isArray(value.hits)) return null;
  const hits: MessageSearchHit[] = [];
  for (const entry of value.hits) {
    const hit = parseHit(entry);
    if (!hit) return null;
    hits.push(hit);
  }
  const indexing = parseIndexing(value.indexing);
  if (!indexing) return null;
  if (!optionalString(value.next_cursor)) return null;
  return {
    hits,
    next_cursor: value.next_cursor as string | undefined,
    indexing,
  };
}
