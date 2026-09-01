import { useEffect, useRef } from "react";

import type { ApiClient } from "./api";
import { useChatAttention } from "./ChatAttention";
import { requestUserAttention } from "./host";
import { useInbox } from "./Inbox";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";
import { useVisibilityGatedPoll } from "./useVisibilityGatedPoll";

/**
 * Safety-net cadence. The event stream signals the open chat's prompts as
 * they park; the timer covers chats that are not open, whose parked work has
 * no stream to announce it. Hidden, it slows rather than stops: a question
 * parking in a background chat still wants the dock bounce.
 */
const POLL_INTERVAL_MS = 30_000;
const HIDDEN_POLL_INTERVAL_MS = 60_000;

const promptActions = usePendingPrompts.getState();
const attentionActions = useChatAttention.getState();
const inboxActions = useInbox.getState();

/**
 * Watches every conversation for parked work and keeps detailed prompt cards
 * current for the open one.
 *
 * The cross-chat poll is deliberately one server-side read, rather than a
 * browser loop over chats, and it is the read the inbox is built from — the
 * rail's attention markers and the inbox are two views of one set, so they
 * cannot disagree about what is waiting. Detail still comes from the selected
 * chat's established recovery routes, which keeps prompt content scoped to the
 * conversation that can render it.
 */
export function useChatPromptWatcher(
  client: ApiClient | null,
  chatId: string | null,
): void {
  // Which questions the shell has already announced. It spans chat switches
  // and screens, then is pruned to the current server summary after each read.
  const announcedCallIdsRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const detailsRefreshRef = useRef<(() => void) | null>(null);
  const questionsSignal = useRefreshSignals((state) => state.userQuestions);
  const plansSignal = useRefreshSignals((state) => state.planApprovals);
  const folderSignal = useRefreshSignals((state) => state.folderAccess);
  const writebackSignal = useRefreshSignals((state) => state.outputWritebacks);

  useEffect(() => {
    if (!client) {
      attentionActions.clear();
      inboxActions.clear();
      announcedCallIdsRef.current = new Set();
      refreshRef.current = null;
      return;
    }

    // A new client can point at a different local profile. Do not leave its
    // sidebar carrying the previous server's attention state until the first
    // summary response arrives.
    attentionActions.clear();
    inboxActions.clear();
    announcedCallIdsRef.current = new Set();
    let cancelled = false;
    let summarySeq = 0;

    const readSummary = async () => {
      const seq = ++summarySeq;
      try {
        const pending = await client.listInbox();
        if (cancelled || seq !== summarySeq) return;

        inboxActions.setEntries(pending);
        // The rail marks chats, so only chat-surface entries reach it. A code
        // conversation is marked on its own rail by its session digest.
        attentionActions.setChatIdsWithPendingPrompts(
          pending
            .map((entry) => entry.conversation)
            .filter((conversation) => conversation.surface === "chat")
            .map((conversation) => conversation.chatId),
        );
        const pendingPromptIds = new Set(
          pending.flatMap((entry) => entry.items.map((item) => item.callId)),
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
          console.error("failed to refresh the inbox", err);
        }
      }
    };

    const readAll = () => {
      void readSummary();
      detailsRefreshRef.current?.();
    };

    refreshRef.current = readAll;
    readAll();

    return () => {
      cancelled = true;
      summarySeq += 1;
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
    let plansSeq = 0;
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

    const readPlans = async () => {
      const seq = ++plansSeq;
      try {
        const pending = await client.listPendingPlanApprovals(chatId);
        if (!cancelled && seq === plansSeq) {
          promptActions.setPlanApprovals(chatId, pending);
        }
      } catch (err) {
        if (!cancelled && seq === plansSeq) {
          console.error("failed to refresh pending plan approvals", err);
        }
      }
    };

    const readFolderAccess = async () => {
      const seq = ++folderSeq;
      try {
        const pending = await client.listPendingFolderAccessRequests(chatId);
        if (!cancelled && seq === folderSeq)
          promptActions.setFolderAccess(chatId, pending);
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
      void readPlans();
      void readFolderAccess();
      void readOutputWritebacks();
    };

    detailsRefreshRef.current = readDetails;
    promptActions.setRefresh(readDetails);
    readDetails();

    return () => {
      cancelled = true;
      questionsSeq += 1;
      plansSeq += 1;
      folderSeq += 1;
      writebackSeq += 1;
      detailsRefreshRef.current = null;
      promptActions.setRefresh(() => {});
    };
  }, [client, chatId]);

  // Only a signal raised after this mounted means anything; the counters are
  // app-wide and may already be well past zero on arrival. Counters only
  // grow, so their sum moves on any one of them.
  useVisibilityGatedPoll(() => refreshRef.current?.(), POLL_INTERVAL_MS, {
    enabled: client !== null,
    hiddenIntervalMs: HIDDEN_POLL_INTERVAL_MS,
    revision: questionsSignal + plansSignal + folderSignal + writebackSignal,
  });
}
