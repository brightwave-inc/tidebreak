import type { MessageSearchHit, MessageSearchRange } from "../generated/wire";

/**
 * The pure half of message search: how a hit's snippet splits into plain
 * and matched runs, what a hit is called, where it leads, and which words a
 * query highlights once the transcript is on screen.
 */

/** One run of a snippet, matched or not. */
export type SnippetSegment = { text: string; match: boolean };

/**
 * A snippet cut into runs at its matched ranges.
 *
 * The ranges count UTF-16 code units, which is what JavaScript string
 * indices count, so they slice the snippet directly. They arrive in order and
 * apart; this still sorts, clamps, and merges them, so a range from a newer
 * or older server can never throw or print a character twice.
 */
export function snippetSegments(
  snippet: string,
  ranges: readonly MessageSearchRange[],
): SnippetSegment[] {
  const clamped = ranges
    .map((range) => ({
      start: Math.max(0, Math.min(range.start, snippet.length)),
      end: Math.max(0, Math.min(range.end, snippet.length)),
    }))
    .filter((range) => range.end > range.start)
    .sort((left, right) => left.start - right.start);
  const segments: SnippetSegment[] = [];
  let cursor = 0;
  for (const range of clamped) {
    const start = Math.max(range.start, cursor);
    if (range.end <= start) continue;
    if (start > cursor) {
      segments.push({ text: snippet.slice(cursor, start), match: false });
    }
    segments.push({ text: snippet.slice(start, range.end), match: true });
    cursor = range.end;
  }
  if (cursor < snippet.length) {
    segments.push({ text: snippet.slice(cursor), match: false });
  }
  return segments;
}

/** What a hit is called when its conversation has no title yet. */
export function hitTitle(hit: Pick<MessageSearchHit, "title" | "kind">) {
  const title = hit.title?.trim();
  if (title) return title;
  return hit.kind === "chat" ? "New conversation" : "Untitled conversation";
}

/** Who wrote the matched text, as a reader would say it. */
export function hitSourceLabel(hit: Pick<MessageSearchHit, "source">) {
  switch (hit.source) {
    case "user":
      return "You";
    case "assistant":
      return "Assistant";
    case "tool":
      return "Tool call";
  }
}

/** A stable identity for a hit, for list keys and the palette's selection. */
export function hitKey(hit: MessageSearchHit): string {
  const piece =
    hit.message_id ??
    (hit.event_seq !== undefined
      ? `event:${hit.event_seq}`
      : `input:${hit.turn_id ?? hit.created_at}`);
  return `message:${hit.session_id}:${piece}`;
}

/** Where opening a hit goes, and what to put on screen there. */
export type RevealTarget =
  | {
      kind: "chat";
      sessionId: string;
      messageId: string;
      turnId?: string;
    }
  | {
      kind: "code";
      sessionId: string;
      workspaceId?: string;
      /** The journal event to show; absent for a turn's own prompt. */
      eventSeq?: number;
      turnId?: string;
    };

/** The place in its conversation a hit points at, or `null` when it names none. */
export function hitTarget(hit: MessageSearchHit): RevealTarget | null {
  if (hit.kind === "chat") {
    if (!hit.message_id) return null;
    return {
      kind: "chat",
      sessionId: hit.session_id,
      messageId: hit.message_id,
      turnId: hit.turn_id,
    };
  }
  if (hit.event_seq === undefined && !hit.turn_id) return null;
  return {
    kind: "code",
    sessionId: hit.session_id,
    workspaceId: hit.workspace_id,
    eventSeq: hit.event_seq,
    turnId: hit.turn_id,
  };
}

/** The route that opens a hit's conversation. */
export function hitRoute(target: RevealTarget): string {
  if (target.kind === "chat") return `/c/${target.sessionId}`;
  if (target.workspaceId) {
    return `/code/w/${target.workspaceId}?task=${encodeURIComponent(target.sessionId)}`;
  }
  return `/code/s/${target.sessionId}`;
}

/**
 * Fold text the way the index folds it, closely enough to find a query's
 * words again on screen: compatibility-decomposed, without combining marks,
 * lowercased.
 */
export function foldText(text: string): string {
  return text
    .normalize("NFKD")
    .replace(/\p{M}+/gu, "")
    .toLowerCase();
}

/**
 * The words of a query, folded, in the order typed, without repeats.
 *
 * Only letters and digits make a word; everything else separates words,
 * which is how the search reads a query too.
 */
export function queryTerms(query: string): string[] {
  const words = foldText(query).match(/[\p{L}\p{N}]+/gu) ?? [];
  return [...new Set(words)];
}
