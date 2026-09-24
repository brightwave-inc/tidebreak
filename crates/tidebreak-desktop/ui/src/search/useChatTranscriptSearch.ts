import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import { useChatSessionStore } from "../ChatSessionStore";
import { friendlyErrorMessage } from "../lib/utils";
import {
  type ChatHistoryView,
  loadChatMessage,
  loadEarlierHistory,
} from "./chatReveal";
import {
  FOCUSED_HIGHLIGHT,
  paintHighlight,
  ringElement,
  scrollToElement,
  termRanges,
} from "./highlightTerms";
import { useTranscriptRevealStore, useRevealRequest } from "./transcriptReveal";
import { useFindBar } from "./useFindBar";

/** How long a reveal waits for its row to render before giving up. */
const REVEAL_DEADLINE_MS = 5_000;

/** The row that draws a chat message, user or assistant. */
export function findMessageRow(
  root: ParentNode,
  messageId: string,
): HTMLElement | null {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(messageId)) return null;
  return root.querySelector<HTMLElement>(
    `[data-message-id="${messageId}"], [data-transcript-anchor="${messageId}"]`,
  );
}

type Reveal = { messageId: string; terms: readonly string[]; nonce: number };

/**
 * Search inside one open Work chat: the find bar, and answering the
 * palette's requests to show a message.
 *
 * Either one ends in the same reveal. The message is loaded if the
 * transcript does not hold it (see `chatReveal.ts`), the window of turns
 * reaches back to it, and once its row renders the transcript scrolls to it,
 * rings it, and marks the query's words inside it.
 */
export function useChatTranscriptSearch({
  client,
  chatId,
  hydrated,
  scrollElement,
  disarmFollow,
  beginProgrammaticScroll,
  endProgrammaticScroll,
}: {
  client: Pick<ApiClient, "listChatMessages" | "searchMessages">;
  chatId: string;
  hydrated: boolean;
  scrollElement: HTMLElement | null;
  disarmFollow: () => void;
  beginProgrammaticScroll: () => void;
  endProgrammaticScroll: () => void;
}) {
  const [historyView, setHistoryView] = useState<ChatHistoryView | null>(null);
  const historyRef = useRef(historyView);
  historyRef.current = historyView;
  const [pending, setPending] = useState<Reveal | null>(null);
  const latest = useRef(0);
  // The session store holds whichever conversation is open. A page that
  // lands after this one closed belongs to no transcript on screen, so
  // closing it retires every jump still waiting for a page.
  useEffect(
    () => () => {
      latest.current += 1;
    },
    [chatId],
  );

  const reveal = useCallback(
    async (messageId: string, terms: readonly string[]) => {
      latest.current += 1;
      const token = latest.current;
      try {
        const loaded = await loadChatMessage({
          client,
          chatId,
          messageId,
          session: () => useChatSessionStore.getState(),
          update: (change) => useChatSessionStore.getState().update(change),
          view: historyRef.current,
          isCurrent: () => token === latest.current,
        });
        if (!loaded || token !== latest.current) return;
        if (loaded.kind === "missing") {
          toast.message("That message is no longer in this conversation.");
          return;
        }
        if (loaded.kind === "history") disarmFollow();
        setHistoryView(loaded.kind === "history" ? loaded.view : null);
        setPending({ messageId, terms, nonce: token });
      } catch (error) {
        if (token !== latest.current) return;
        toast.error(
          friendlyErrorMessage(error, "Could not open that message."),
        );
      }
    },
    [client, chatId, disarmFollow],
  );

  // The palette raises its request before the conversation opens; it is
  // answered once the transcript has loaded.
  const request = useRevealRequest("chat", chatId);
  useEffect(() => {
    if (!request || !hydrated || request.target.kind !== "chat") return;
    useTranscriptRevealStore.getState().settle(request.nonce);
    void reveal(request.target.messageId, request.terms);
  }, [request, hydrated, reveal]);

  // The row renders a frame or a page load after the reveal asks for it.
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
      const row = findMessageRow(scrollElement, pending.messageId);
      if (!row) return;
      disarmFollow();
      beginProgrammaticScroll();
      scrollToElement(scrollElement, row);
      ringElement(row);
      paintHighlight(FOCUSED_HIGHLIGHT, termRanges(row, pending.terms));
      window.setTimeout(endProgrammaticScroll, 800);
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
  }, [
    pending,
    scrollElement,
    disarmFollow,
    beginProgrammaticScroll,
    endProgrammaticScroll,
  ]);

  const find = useFindBar({
    hostId: `chat:${chatId}`,
    client,
    sessionId: chatId,
    scrollElement,
    onReveal: (hit, terms) => {
      if (hit.message_id) void reveal(hit.message_id, terms);
    },
  });

  const [earlierLoading, setEarlierLoading] = useState(false);
  const showEarlierHistory = useCallback(async () => {
    const view = historyRef.current;
    if (!view || earlierLoading) return;
    setEarlierLoading(true);
    try {
      const next = await loadEarlierHistory(client, chatId, view);
      if (historyRef.current === view) setHistoryView(next);
    } finally {
      setEarlierLoading(false);
    }
  }, [client, chatId, earlierLoading]);

  const leaveHistory = useCallback(() => {
    latest.current += 1;
    setPending(null);
    setHistoryView(null);
  }, []);

  return {
    historyView,
    showEarlierHistory,
    leaveHistory,
    revealMessageId: pending?.messageId ?? null,
    find,
  };
}
