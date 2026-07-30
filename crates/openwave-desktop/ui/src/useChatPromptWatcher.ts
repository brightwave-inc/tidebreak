import { useEffect, useRef } from "react";

import type { ApiClient } from "./api";
import { useChatAttention } from "./ChatAttention";
import { requestUserAttention } from "./host";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";

const POLL_INTERVAL_MS = 10_000;

const promptActions = usePendingPrompts.getState();
const attentionActions = useChatAttention.getState();

/**
 * Watches every conversation for a parked prompt and keeps detailed prompt
 * cards current for the open one.
 *
 * The summary poll is deliberately one server-side read, rather than a browser
 * loop over chats. Detail still comes from the selected chat's established
 * recovery routes, which keeps prompt content scoped to the conversation that
 * can render it.
 */
export function useChatPromptWatcher(client: ApiClient | null, chatId: string | null): void {
  // Which questions the shell has already announced. It spans chat switches
  // and screens, then is pruned to the current server summary after each read.
  const announcedCallIdsRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const detailsRefreshRef = useRef<(() => void) | null>(null);
  const questionsSignal = useRefreshSignals((state) => state.userQuestions);
  const folderSignal = useRefreshSignals((state) => state.folderAccess);
  const writebackSignal = useRefreshSignals((state) => state.outputWritebacks);

  useEffect(() => {
    if (!client) {
      attentionActions.clear();
      announcedCallIdsRef.current = new Set();
      refreshRef.current = null;
      return;
    }

    // A new client can point at a different local profile. Do not leave its
    // sidebar carrying the previous server's attention state until the first
    // summary response arrives.
    attentionActions.clear();
    announcedCallIdsRef.current = new Set();
    let cancelled = false;
    let summarySeq = 0;

    const readSummary = async () => {
      const seq = ++summarySeq;
      try {
        const pending = await client.listPendingChatPrompts();
        if (cancelled || seq !== summarySeq) return;

        attentionActions.setChatIdsWithPendingPrompts(
          pending.map((prompt) => prompt.chatId),
        );
        const pendingPromptIds = new Set(
          pending.flatMap((prompt) => [
            ...prompt.questionCallIds,
            ...prompt.planCallIds,
            ...prompt.folderAccessCallIds,
            ...prompt.outputWritebackCallIds,
          ]),
        );
        const unannounced = [...pendingPromptIds].filter(
          (callId) => !announcedCallIdsRef.current.has(callId),
        );
        announcedCallIdsRef.current = pendingPromptIds;
        if (unannounced.length > 0) {
          void requestUserAttention().catch(() => {
            // Attention is a best-effort hint. Durable polling is truth.
          });
        }
      } catch (err) {
        if (!cancelled && seq === summarySeq) {
          // Keep the last successful state visible. Clearing it on a transient
          // failure would make a waiting chat look resolved.
          console.error("failed to refresh pending chat prompts", err);
        }
      }
    };

    const readAll = () => {
      void readSummary();
      detailsRefreshRef.current?.();
    };

    refreshRef.current = readAll;
    readAll();
    const interval = window.setInterval(readAll, POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      summarySeq += 1;
      window.clearInterval(interval);
      refreshRef.current = null;
    };
  }, [client]);

  useEffect(() => {
    promptActions.reset(chatId);
    if (!client || !chatId) {
      promptActions.setRefresh(() => {});
      detailsRefreshRef.current = null;
      return;
    }

    let cancelled = false;
    let questionsSeq = 0;
    let folderSeq = 0;
    let writebackSeq = 0;

    const readQuestions = async () => {
      const seq = ++questionsSeq;
      try {
        const pending = await client.listPendingUserQuestions(chatId);
        if (!cancelled && seq === questionsSeq) {
          promptActions.setUserQuestions(chatId, pending);
        }
      } catch (err) {
        if (!cancelled && seq === questionsSeq) {
          console.error("failed to refresh pending user questions", err);
        }
      }
    };

    const readFolderAccess = async () => {
      const seq = ++folderSeq;
      try {
        const pending = await client.listPendingFolderAccessRequests(chatId);
        if (!cancelled && seq === folderSeq) promptActions.setFolderAccess(chatId, pending);
      } catch (err) {
        if (!cancelled && seq === folderSeq) {
          console.error("failed to refresh pending folder access", err);
          promptActions.setFolderAccess(chatId, []);
        }
      }
    };

    const readOutputWritebacks = async () => {
      const seq = ++writebackSeq;
      try {
        const pending = await client.listPendingOutputWritebackRequests(chatId);
        if (!cancelled && seq === writebackSeq) {
          promptActions.setOutputWritebacks(chatId, pending);
        }
      } catch (err) {
        if (!cancelled && seq === writebackSeq) {
          console.error("failed to refresh pending output write-backs", err);
          promptActions.setOutputWritebacks(chatId, []);
        }
      }
    };

    const readDetails = () => {
      void readQuestions();
      void readFolderAccess();
      void readOutputWritebacks();
    };

    detailsRefreshRef.current = readDetails;
    promptActions.setRefresh(readDetails);
    readDetails();

    return () => {
      cancelled = true;
      questionsSeq += 1;
      folderSeq += 1;
      writebackSeq += 1;
      detailsRefreshRef.current = null;
      promptActions.setRefresh(() => {});
    };
  }, [client, chatId]);

  // Only a signal raised after this mounted means anything; the counters are
  // app-wide and may already be well past zero on arrival.
  const lastSignalsRef = useRef({
    questions: questionsSignal,
    folder: folderSignal,
    writeback: writebackSignal,
  });
  useEffect(() => {
    const last = lastSignalsRef.current;
    if (
      last.questions === questionsSignal &&
      last.folder === folderSignal &&
      last.writeback === writebackSignal
    ) return;
    lastSignalsRef.current = {
      questions: questionsSignal,
      folder: folderSignal,
      writeback: writebackSignal,
    };
    refreshRef.current?.();
  }, [questionsSignal, folderSignal, writebackSignal]);
}
