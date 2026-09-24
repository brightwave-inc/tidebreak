import { useCallback, useEffect, useId, useRef, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import type { CodeTurnSnapshot, SequencedCodeEventFrame } from "../api/types";
import {
  type CodeSessionDeps,
  type CodeTranscriptItem,
  hydrateCodeTurns,
  initialCodeSessionState,
  itemForEvent,
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
      const stretch = shown ? itemForEvent(shown.items, target) : null;
      if (stretch) {
        setPending({ itemId: stretch.id, terms, nonce: token });
        return;
      }
      if (target.eventSeq === undefined) {
        toast.message("That message is no longer in this session.");
        return;
      }
      try {
        const [frames, turns] = await Promise.all([
          client.listCodeJournal(sessionId, {
            before: target.eventSeq + EVENTS_AFTER_FOUND,
            limit: JOURNAL_WINDOW_EVENTS,
          }),
          client.listCodeSessionTurns(sessionId),
        ]);
        if (token !== latest.current) return;
        const view = historyFromJournal(frames, turns);
        const found = itemForEvent(view.items, target);
        if (!found) {
          toast.message("That message is no longer in this session.");
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
