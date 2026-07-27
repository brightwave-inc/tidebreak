import { useEffect, useRef } from "react";

import type { ApiClient } from "./api";
import { requestUserAttention } from "./host";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";

const POLL_INTERVAL_MS = 10_000;

const promptActions = usePendingPrompts.getState();

/**
 * Watches the open conversation for anything the agent is parked waiting on.
 *
 * Mounted by the shell rather than by a view, because the answer to "has the
 * agent asked me something?" cannot depend on which screen is rendered. Reading
 * the settings screen does not mean the agent stopped asking.
 *
 * Polling is the durable truth and the event stream only says when to look
 * again, so this stays correct with no socket at all — which is what makes it
 * safe for the conversation itself to unmount.
 */
export function useChatPromptWatcher(client: ApiClient | null, chatId: string | null): void {
  // Which questions the reader has already been alerted to. This lives for as
  // long as the shell does, so it spans chat switches and screen changes: a
  // question announced once is not announced again when the reader comes back
  // to it. Pruned to what is still pending on every read, so a long session
  // does not accumulate the id of every question ever asked.
  const announcedCallIdsRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const questionsSignal = useRefreshSignals((state) => state.userQuestions);
  const folderSignal = useRefreshSignals((state) => state.folderAccess);

  useEffect(() => {
    promptActions.reset(chatId);
    if (!client || !chatId) {
      promptActions.setRefresh(() => {});
      return;
    }
    let cancelled = false;
    let questionsSeq = 0;
    let folderSeq = 0;

    const readQuestions = async () => {
      const seq = ++questionsSeq;
      try {
        const pending = await client.listPendingUserQuestions(chatId);
        if (cancelled || seq !== questionsSeq) return;
        const unannounced = pending.filter(
          (request) => !announcedCallIdsRef.current.has(request.callId),
        );
        announcedCallIdsRef.current = new Set(pending.map((request) => request.callId));
        promptActions.setUserQuestions(chatId, pending);
        if (unannounced.length > 0) {
          void requestUserAttention().catch(() => {
            // Attention is a best-effort hint. Durable polling is truth.
          });
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

    const readAll = () => {
      void readQuestions();
      void readFolderAccess();
    };

    refreshRef.current = readAll;
    promptActions.setRefresh(readAll);
    readAll();
    const interval = window.setInterval(readAll, POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      questionsSeq += 1;
      folderSeq += 1;
      window.clearInterval(interval);
      refreshRef.current = null;
      promptActions.setRefresh(() => {});
    };
  }, [client, chatId]);

  // Only a signal raised after this mounted means anything; the counters are
  // app-wide and may already be well past zero on arrival.
  const lastSignalsRef = useRef({ questions: questionsSignal, folder: folderSignal });
  useEffect(() => {
    const last = lastSignalsRef.current;
    if (last.questions === questionsSignal && last.folder === folderSignal) return;
    lastSignalsRef.current = { questions: questionsSignal, folder: folderSignal };
    refreshRef.current?.();
  }, [questionsSignal, folderSignal]);
}
