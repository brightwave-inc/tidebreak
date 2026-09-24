import { useCallback, useEffect, useId, useRef, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import type {
  CodeEvent,
  CodeTurnSnapshot,
  SequencedCodeEventFrame,
} from "../api/types";
import {
  type CodeSessionDeps,
  type CodeTranscriptItem,
  hydrateCodeTurns,
  initialCodeSessionState,
  itemForEvent,
  mainAgentTranscriptItems,
  reduceCodeSessionEvent,
} from "../code/CodeSessionReducer";
import { friendlyErrorMessage } from "../lib/utils";
import {
  FOCUSED_HIGHLIGHT,
  paintHighlight,
  ringElement,
  scrollToElement,
  termRanges,
} from "./highlightTerms";
import type { RevealTarget } from "./messageSearch";
import { useTranscriptRevealStore, useRevealRequest } from "./transcriptReveal";
import { useFindBar } from "./useFindBar";

/** How far past a found event its window of the journal reads. */
export const EVENTS_AFTER_FOUND = 120;
/** How many events a window of the journal reads. */
export const JOURNAL_WINDOW_EVENTS = 400;
/**
 * How many events a window reads back past its start to find where a found
 * tool call started, a page at a time.
 */
export const CALL_START_LOOKBACK_EVENTS = 4_000;
/** One page of that look back; the journal route's own ceiling. */
const LOOKBACK_PAGE_EVENTS = 2_000;
/** How long a reveal waits for its row to render before giving up. */
const REVEAL_DEADLINE_MS = 5_000;

type CodeTarget = Pick<
  Extract<RevealTarget, { kind: "code" }>,
  "eventSeq" | "turnId"
>;

/** A stretch of a code session's journal, shown apart from its tail. */
export type CodeHistoryView = { items: CodeTranscriptItem[] };

let historyItem = 0;
const historyDeps: CodeSessionDeps = {
  nextId: () => {
    historyItem += 1;
    return `history-${historyItem}`;
  },
  now: () => new Date().toISOString(),
};

/**
 * The transcript a window of the journal draws, on its own.
 *
 * The turns' prompts come from the durable turn rows, the rest from the
 * events, the same two sources the live transcript is built from. Only the
 * turns the window saw start or resume get their prompt, so the stretch reads
 * as the part of the session it is and not as every turn at once.
 */
export function historyFromJournal(
  frames: readonly SequencedCodeEventFrame[],
  turns: readonly CodeTurnSnapshot[],
): CodeHistoryView {
  const seen = new Set<string>();
  for (const frame of frames) {
    const event = frame.event;
    if (event.type === "turn_started" || event.type === "turn_resumed") {
      seen.add(event.turn_id);
    }
  }
  let state = hydrateCodeTurns(
    initialCodeSessionState(),
    turns.filter((turn) => seen.has(turn.id)),
  );
  for (const frame of frames) {
    state = reduceCodeSessionEvent(state, frame, historyDeps).state;
  }
  return { items: state.items };
}

type JournalClient = Pick<ApiClient, "listCodeJournal">;

/** The tool call a journal event reports on, when it reports on one. */
function callOf(event: CodeEvent): string | null {
  if (event.type === "background_activity") return callOf(event.event);
  return event.type === "tool_started" || event.type === "tool_completed"
    ? event.call_id
    : null;
}

/** Whether `frames` hold the event that started `callId`. */
function startsCall(
  frames: readonly SequencedCodeEventFrame[],
  callId: string,
): boolean {
  return frames.some((frame) => {
    const event =
      frame.event.type === "background_activity"
        ? frame.event.event
        : frame.event;
    return event.type === "tool_started" && event.call_id === callId;
  });
}

/**
 * The window of the journal that opens the event at `eventSeq`: the events
 * around it, oldest first.
 *
 * A tool call's row is drawn from the event that started it, while a match
 * on the call names the last event that restated it. When the call started
 * before the window, the window reads back to its start, up to
 * {@link CALL_START_LOOKBACK_EVENTS} further. `callStartMissing` says the
 * start was still not found.
 */
export async function journalWindow(
  client: JournalClient,
  sessionId: string,
  eventSeq: number,
): Promise<{
  frames: SequencedCodeEventFrame[];
  callStartMissing: boolean;
}> {
  let frames = await client.listCodeJournal(sessionId, {
    before: eventSeq + EVENTS_AFTER_FOUND,
    limit: JOURNAL_WINDOW_EVENTS,
  });
  const found = frames.find((frame) => frame.seq === eventSeq);
  const callId = found ? callOf(found.event) : null;
  if (!callId) return { frames, callStartMissing: false };
  let read = 0;
  while (!startsCall(frames, callId) && read < CALL_START_LOOKBACK_EVENTS) {
    const first = frames[0];
    if (!first?.truncated) break;
    const older = await client.listCodeJournal(sessionId, {
      before: first.seq,
      limit: Math.min(LOOKBACK_PAGE_EVENTS, CALL_START_LOOKBACK_EVENTS - read),
    });
    if (older.length === 0) break;
    read += older.length;
    // Only the window's first frame says older events were left out.
    const { truncated: _joined, ...joined } = first;
    frames = [...older, joined, ...frames.slice(1)];
  }
  return { frames, callStartMissing: !startsCall(frames, callId) };
}

/** The element that draws a code row, found by the id the reducer gave it. */
export function findCodeRow(
  root: ParentNode,
  itemId: string,
): HTMLElement | null {
  if (!/^[A-Za-z0-9_:-]{1,128}$/.test(itemId)) return null;
  const marked = root.querySelector<HTMLElement>(
    `[data-code-item-id="${itemId}"], [data-transcript-anchor="${itemId}"]`,
  );
  if (!marked) return null;
  // A box-less wrapper names the row; the box to scroll to is inside it.
  if (getComputedStyle(marked).display === "contents") {
    return (marked.firstElementChild as HTMLElement | null) ?? null;
  }
  return marked;
}

type Reveal = { itemId: string; terms: readonly string[]; nonce: number };

/**
 * Search inside one open code session: the find bar, and the palette's
 * requests to show an event.
 *
 * An event the transcript holds is scrolled to where it is. The transcript
 * holds only the newest part of a long session's journal; an older event
 * opens the window of the journal around it on its own, with the way back to
 * the latest, and nothing between the two is read.
 */
export function useCodeTranscriptSearch({
  client,
  sessionId,
  hydrated,
  items,
  scrollElement,
  pauseFollow,
}: {
  client: Pick<
    ApiClient,
    "searchMessages" | "listCodeJournal" | "listCodeSessionTurns"
  >;
  sessionId: string;
  hydrated: boolean;
  items: readonly CodeTranscriptItem[];
  scrollElement: HTMLElement | null;
  pauseFollow: () => void;
}) {
  const [history, setHistory] = useState<CodeHistoryView | null>(null);
  const [pending, setPending] = useState<Reveal | null>(null);
  const itemsRef = useRef(items);
  itemsRef.current = items;
  const historyRef = useRef(history);
  historyRef.current = history;
  const latest = useRef(0);

  const reveal = useCallback(
    async (target: CodeTarget, terms: readonly string[]) => {
      latest.current += 1;
      const token = latest.current;
      const live = itemForEvent(itemsRef.current, target);
      if (live) {
        setHistory(null);
        setPending({ itemId: live.id, terms, nonce: token });
        return;
      }
      const shown = historyRef.current;
      const stretch = shown
        ? itemForEvent(mainAgentTranscriptItems(shown.items), target)
        : null;
      if (stretch) {
        setPending({ itemId: stretch.id, terms, nonce: token });
        return;
      }
      if (target.eventSeq === undefined) {
        toast.message("That message is no longer in this session.");
        return;
      }
      try {
        const [opened, turns] = await Promise.all([
          journalWindow(client, sessionId, target.eventSeq),
          client.listCodeSessionTurns(sessionId),
        ]);
        if (token !== latest.current) return;
        const view = historyFromJournal(opened.frames, turns);
        // The pane draws the main agent's rows; a subagent's are read from
        // its own view.
        const found = itemForEvent(
          mainAgentTranscriptItems(view.items),
          target,
        );
        if (!found) {
          toast.message(
            itemForEvent(view.items, target)
              ? "That message is in a subagent's work. Open the subagent to read it."
              : opened.callStartMissing
                ? "That tool call started too far back in this session to open here."
                : "That message is no longer in this session.",
          );
          return;
        }
        pauseFollow();
        setHistory(view);
        setPending({ itemId: found.id, terms, nonce: token });
      } catch (error) {
        if (token !== latest.current) return;
        toast.error(
          friendlyErrorMessage(error, "Could not open that message."),
        );
      }
    },
    [client, sessionId, pauseFollow],
  );

  const request = useRevealRequest("code", sessionId);
  useEffect(() => {
    if (!request || !hydrated || request.target.kind !== "code") return;
    useTranscriptRevealStore.getState().settle(request.nonce);
    void reveal(
      { eventSeq: request.target.eventSeq, turnId: request.target.turnId },
      request.terms,
    );
  }, [request, hydrated, reveal]);

  useEffect(() => {
    if (!pending || !scrollElement) return;
    let settled = false;
    const finish = () => {
      settled = true;
      window.clearInterval(timer);
      window.clearTimeout(deadline);
      setPending((current) =>
        current?.nonce === pending.nonce ? null : current,
      );
    };
    const attempt = () => {
      if (settled) return;
      const row = findCodeRow(scrollElement, pending.itemId);
      if (!row) return;
      pauseFollow();
      scrollToElement(scrollElement, row);
      ringElement(row);
      paintHighlight(FOCUSED_HIGHLIGHT, termRanges(row, pending.terms));
      finish();
    };
    const timer = window.setInterval(attempt, 100);
    const deadline = window.setTimeout(finish, REVEAL_DEADLINE_MS);
    const frame = window.requestAnimationFrame(attempt);
    return () => {
      window.clearInterval(timer);
      window.clearTimeout(deadline);
      window.cancelAnimationFrame(frame);
    };
  }, [pending, scrollElement, pauseFollow]);

  const instance = useId();
  const find = useFindBar({
    hostId: `code:${sessionId}:${instance}`,
    client,
    sessionId,
    scrollElement,
    onReveal: (hit, terms) =>
      void reveal({ eventSeq: hit.event_seq, turnId: hit.turn_id }, terms),
  });

  const leaveHistory = useCallback(() => {
    latest.current += 1;
    setPending(null);
    setHistory(null);
  }, []);

  return {
    history,
    leaveHistory,
    revealItemId: pending?.itemId ?? null,
    find,
  };
}
