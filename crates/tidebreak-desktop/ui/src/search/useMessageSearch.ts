import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api";
import type {
  MessageSearchHit,
  MessageSearchIndexing,
} from "../generated/wire";
import { friendlyErrorMessage } from "../lib/utils";

/**
 * The shortest query worth sending. One letter matches only itself on the
 * server, which reads as noise in a list of results; two start matching words
 * as a prefix.
 */
export const MIN_MESSAGE_QUERY_CHARS = 2;

/** How long typing has to pause before a search goes out. */
export const MESSAGE_SEARCH_DELAY_MS = 180;

export type MessageSearchState = {
  status: "idle" | "loading" | "ready" | "error";
  /** The words the hits answer. Trails the typed query while one loads. */
  query: string;
  hits: readonly MessageSearchHit[];
  indexing: MessageSearchIndexing | null;
  error: string | null;
};

export const IDLE_MESSAGE_SEARCH: MessageSearchState = {
  status: "idle",
  query: "",
  hits: [],
  indexing: null,
  error: null,
};

type SearchClient = Pick<ApiClient, "searchMessages">;

/**
 * Search messages as the person types.
 *
 * Each keystroke restarts a short wait, and only a pause sends the query;
 * a newer query aborts the request still out for an older one, so a slow
 * answer can never land over a newer one. While a search runs, the hits of
 * the last one stay up with the status saying a newer one is coming, which
 * keeps the list from blinking empty between words. A search that fails
 * clears them.
 */
export function useMessageSearch(
  client: SearchClient | null,
  query: string,
  options: {
    enabled?: boolean;
    limit?: number;
    sessionId?: string;
    delayMs?: number;
  } = {},
): MessageSearchState {
  const {
    enabled = true,
    limit,
    sessionId,
    delayMs = MESSAGE_SEARCH_DELAY_MS,
  } = options;
  const words = query.trim();
  const active =
    client !== null && enabled && words.length >= MIN_MESSAGE_QUERY_CHARS;
  const [state, setState] = useState<MessageSearchState>(IDLE_MESSAGE_SEARCH);
  // Read when a search goes out, so a caller that hands a fresh client
  // object each render does not restart the search on every render.
  const clientRef = useRef(client);
  clientRef.current = client;

  useEffect(() => {
    const current = clientRef.current;
    if (!active || current === null) {
      setState(IDLE_MESSAGE_SEARCH);
      return;
    }
    setState((previous) => ({ ...previous, status: "loading", error: null }));
    const controller = new AbortController();
    const timer = globalThis.setTimeout(() => {
      current
        .searchMessages({ q: words, limit, sessionId }, controller.signal)
        .then(
          (page) => {
            if (controller.signal.aborted) return;
            setState({
              status: "ready",
              query: words,
              hits: page.hits,
              indexing: page.indexing,
              error: null,
            });
          },
          (error: unknown) => {
            if (controller.signal.aborted) return;
            // The last search's hits answer another query, so they go: a
            // hit left under the error would open a match for words the
            // field no longer holds.
            setState({
              ...IDLE_MESSAGE_SEARCH,
              status: "error",
              query: words,
              error: friendlyErrorMessage(error, "Could not search messages."),
            });
          },
        );
    }, delayMs);
    return () => {
      globalThis.clearTimeout(timer);
      controller.abort();
    };
  }, [active, words, limit, sessionId, delayMs]);

  return active ? state : IDLE_MESSAGE_SEARCH;
}
