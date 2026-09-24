import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api";
import type {
  MessageSearchHit,
  MessageSearchIndexing,
} from "../generated/wire";
import { friendlyErrorMessage } from "../lib/utils";
import { queryTerms } from "./messageSearch";

/** Most matches the find bar steps through, and counts exactly below. */
export const MAX_FIND_MATCHES = 500;
/** Hits one search page reads. The route's own ceiling. */
const FIND_PAGE = 50;
/** How long typing has to pause before the find bar searches. */
export const FIND_DELAY_MS = 150;

export type TranscriptFindState = {
  status: "idle" | "loading" | "ready" | "error";
  /** The words the matches answer. */
  query: string;
  /** Every match in the conversation, newest first. */
  matches: readonly MessageSearchHit[];
  /** More matched than the find bar reads. */
  capped: boolean;
  /** The match on screen, counted from the newest; -1 for none. */
  position: number;
  indexing: MessageSearchIndexing | null;
  error: string | null;
  /**
   * Why the matches come only from what the transcript has loaded, when the
   * index cannot search this conversation; null when they come from the
   * index.
   */
  loadedOnly: string | null;
};

const IDLE: TranscriptFindState = {
  status: "idle",
  query: "",
  matches: [],
  capped: false,
  position: -1,
  indexing: null,
  error: null,
  loadedOnly: null,
};

/**
 * How to find in a conversation the index cannot search for you: in what the
 * transcript has loaded, and why that is all.
 */
export type LoadedFind = {
  /** Why only what is loaded is searched, said in the find bar. */
  reason: string;
  /** The loaded rows that hold every term, newest first. */
  matches: (terms: readonly string[]) => MessageSearchHit[];
};

type SearchClient = Pick<ApiClient, "searchMessages">;

/**
 * Every match of `query` in one conversation, newest first, up to
 * {@link MAX_FIND_MATCHES}. The search reads the whole conversation from the
 * index, so a match on a page the transcript has not loaded is found too.
 */
export async function findInConversation(
  client: SearchClient,
  sessionId: string,
  query: string,
  signal: AbortSignal,
): Promise<{
  matches: MessageSearchHit[];
  capped: boolean;
  indexing: MessageSearchIndexing;
}> {
  const matches: MessageSearchHit[] = [];
  let cursor: string | undefined;
  let indexing: MessageSearchIndexing | null = null;
  for (;;) {
    const page = await client.searchMessages(
      { q: query, sessionId, limit: FIND_PAGE, cursor },
      signal,
    );
    indexing ??= page.indexing;
    matches.push(...page.hits);
    cursor = page.next_cursor;
    if (!cursor) return { matches, capped: false, indexing };
    if (matches.length >= MAX_FIND_MATCHES) {
      return {
        matches: matches.slice(0, MAX_FIND_MATCHES),
        capped: true,
        indexing,
      };
    }
  }
}

/**
 * Find in the open conversation: the query, its matches, and which one is
 * on screen.
 *
 * Matches run newest first, because a transcript is read from its newest
 * end: the first match shown is the one nearest the reader, `older` steps up
 * the conversation and `newer` back down, each wrapping at the end. Each step
 * hands the match to `onReveal`, which loads its page if it has to and
 * scrolls to it.
 *
 * With `loaded`, the index cannot search this conversation for you, so the
 * find reads what the transcript has loaded instead and says why.
 */
export function useTranscriptFind({
  client,
  sessionId,
  open,
  onReveal,
  loaded = null,
  delayMs = FIND_DELAY_MS,
}: {
  client: SearchClient;
  sessionId: string;
  open: boolean;
  onReveal: (hit: MessageSearchHit, terms: readonly string[]) => void;
  loaded?: LoadedFind | null;
  delayMs?: number;
}) {
  const [query, setQuery] = useState("");
  const [state, setState] = useState<TranscriptFindState>(IDLE);
  const revealRef = useRef(onReveal);
  revealRef.current = onReveal;
  // Read when a search goes out, so a fresh client object per render does
  // not restart the search.
  const clientRef = useRef(client);
  clientRef.current = client;
  const loadedRef = useRef(loaded);
  loadedRef.current = loaded;
  const loadedOnly = loaded?.reason ?? null;
  const words = query.trim();

  useEffect(() => {
    if (!open || words.length === 0) {
      setState(IDLE);
      return;
    }
    setState((current) => ({ ...current, status: "loading", error: null }));
    const controller = new AbortController();
    const timer = globalThis.setTimeout(() => {
      const local = loadedRef.current;
      if (local) {
        const terms = queryTerms(words);
        const found = local.matches(terms);
        const matches = found.slice(0, MAX_FIND_MATCHES);
        setState({
          status: "ready",
          query: words,
          matches,
          capped: found.length > matches.length,
          position: matches.length > 0 ? 0 : -1,
          indexing: null,
          error: null,
          loadedOnly: local.reason,
        });
        const first = matches[0];
        if (first) revealRef.current(first, terms);
        return;
      }
      findInConversation(
        clientRef.current,
        sessionId,
        words,
        controller.signal,
      ).then(
        (found) => {
          if (controller.signal.aborted) return;
          const position = found.matches.length > 0 ? 0 : -1;
          setState({
            status: "ready",
            query: words,
            matches: found.matches,
            capped: found.capped,
            position,
            indexing: found.indexing,
            error: null,
            loadedOnly: null,
          });
          const first = found.matches[0];
          if (first) revealRef.current(first, queryTerms(words));
        },
        (error: unknown) => {
          if (controller.signal.aborted) return;
          setState({
            ...IDLE,
            status: "error",
            query: words,
            error: friendlyErrorMessage(error, "Could not search."),
          });
        },
      );
    }, delayMs);
    return () => {
      globalThis.clearTimeout(timer);
      controller.abort();
    };
  }, [sessionId, open, words, delayMs, loadedOnly]);

  // A different conversation starts the find over.
  useEffect(() => {
    setQuery("");
  }, [sessionId]);

  const stateRef = useRef(state);
  stateRef.current = state;
  const step = useCallback((delta: 1 | -1) => {
    const current = stateRef.current;
    const count = current.matches.length;
    if (current.status !== "ready" || count === 0) return;
    const position = (current.position + delta + count) % count;
    const next = { ...current, position };
    stateRef.current = next;
    setState(next);
    const hit = current.matches[position];
    if (hit) revealRef.current(hit, queryTerms(current.query));
  }, []);

  return {
    query,
    setQuery,
    state,
    /** Step to the next match up the conversation. */
    older: useCallback(() => step(1), [step]),
    /** Step to the next match down the conversation. */
    newer: useCallback(() => step(-1), [step]),
  };
}

/** What the find bar's counter says. */
export function findCountLabel(state: TranscriptFindState): string {
  switch (state.status) {
    case "idle":
      return "";
    case "loading":
      return "Searching…";
    case "error":
      return "Search failed";
    case "ready": {
      const count = state.matches.length;
      if (count === 0) return "No matches";
      const total = state.capped ? `${count}+` : String(count);
      return `${state.position + 1} of ${total}`;
    }
  }
}
