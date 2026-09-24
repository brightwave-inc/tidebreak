import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api";
import type { MessageSearchHit } from "../generated/wire";
import {
  clearHighlight,
  FIND_HIGHLIGHT,
  FOCUSED_HIGHLIGHT,
  paintHighlight,
  termRanges,
} from "./highlightTerms";
import { queryTerms } from "./messageSearch";
import { useTranscriptFindStore } from "./transcriptFind";
import { type LoadedFind, useTranscriptFind } from "./useTranscriptFind";

/** How often the on-screen marks catch up with a transcript that changes. */
const REPAINT_MS = 250;

/**
 * Everything a transcript pane needs to host the find bar: registering for
 * Cmd+F, opening and closing, the search itself, and the marks on every
 * match already on screen.
 *
 * `onReveal` puts one match on screen, loading its page first when the
 * transcript does not hold it; the pane owns that, because only it knows how
 * its transcript pages. `loaded` is how the pane finds in what it has loaded
 * when the index cannot search the conversation for you.
 */
export function useFindBar({
  hostId,
  client,
  sessionId,
  scrollElement,
  onReveal,
  loaded = null,
}: {
  hostId: string;
  client: Pick<ApiClient, "searchMessages">;
  sessionId: string;
  scrollElement: HTMLElement | null;
  onReveal: (hit: MessageSearchHit, terms: readonly string[]) => void;
  loaded?: LoadedFind | null;
}) {
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const returnFocus = useRef<HTMLElement | null>(null);

  useEffect(() => useTranscriptFindStore.getState().register(hostId), [hostId]);
  const activate = useCallback(
    () => useTranscriptFindStore.getState().activate(hostId),
    [hostId],
  );

  const request = useTranscriptFindStore((state) =>
    state.openRequest?.hostId === hostId ? state.openRequest.nonce : null,
  );
  useEffect(() => {
    if (request === null) return;
    const active = document.activeElement;
    // Pressed again while open, the chord goes back to the field.
    if (!inputRef.current || active !== inputRef.current) {
      returnFocus.current = active instanceof HTMLElement ? active : null;
    }
    setOpen(true);
    const frame = window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [request]);

  const find = useTranscriptFind({
    client,
    sessionId,
    open,
    onReveal,
    loaded,
  });

  const words = find.state.status === "ready" ? find.state.query : "";
  useEffect(() => {
    if (!open || !scrollElement || !words) {
      clearHighlight(FIND_HIGHLIGHT);
      return;
    }
    const terms = queryTerms(words);
    const paint = () =>
      paintHighlight(FIND_HIGHLIGHT, termRanges(scrollElement, terms));
    paint();
    // Streamed text, a page that loads, and a turn that expands all change
    // what is on screen; the marks follow, a few times a second at most.
    let timer: ReturnType<typeof setTimeout> | null = null;
    const observer = new MutationObserver(() => {
      if (timer !== null) return;
      timer = setTimeout(() => {
        timer = null;
        paint();
      }, REPAINT_MS);
    });
    observer.observe(scrollElement, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    return () => {
      observer.disconnect();
      if (timer !== null) clearTimeout(timer);
      clearHighlight(FIND_HIGHLIGHT);
    };
  }, [open, scrollElement, words]);

  const { setQuery } = find;
  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
    clearHighlight(FIND_HIGHLIGHT);
    clearHighlight(FOCUSED_HIGHLIGHT);
    const target = returnFocus.current;
    returnFocus.current = null;
    if (target?.isConnected) target.focus();
  }, [setQuery]);

  return { open, close, activate, inputRef, ...find };
}
