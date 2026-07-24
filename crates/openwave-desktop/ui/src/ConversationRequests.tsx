import {
  createContext,
  useContext,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type ReactNode,
  type Ref,
  type RefObject,
} from "react";
import type {
  AgentRun,
  ApiClient,
  Chat,
  PendingFolderAccessRequest,
  PendingUserQuestions,
  UserQuestionAnswer,
} from "./api";
import {
  ActiveTurnSteerFence,
  canBeginActiveTurnSteer,
  shouldClearAcceptedSteerDraft,
} from "./ActiveTurnSteer";
import { agentRunsForChat } from "./AgentActivityPanel";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
  sandboxAgentStopKey,
} from "./SandboxAgentStop";
import {
  hasNativeHost,
  requestUserAttention,
  resolveFolderAccessRequest,
  type FolderAccessDecision,
} from "./host";

/**
 * What the root knows about chat selection, read by requests that are already
 * in flight to decide whether their response still applies. The provider is
 * mounted per conversation, so a response that outlives its chat usually lands
 * on an unmounted instance — but a chat is deleted while its own conversation
 * is still on screen, and that case needs the counters.
 */
export type ChatSelectionFence = {
  /** Bumped whenever the selected chat changes, including on deletion. */
  selection: RefObject<number>;
  chatId: RefObject<string | null>;
  deleting: RefObject<boolean>;
};

/** Everything one conversation is currently waiting on, and how to answer it. */
export type ConversationRequests = {
  agentRuns: AgentRun[];
  agentRunsLoading: boolean;
  agentRunsError: string | null;
  stoppingRunIds: Set<string>;
  stopErrorRunIds: Set<string>;
  refreshAgentRuns: () => void;
  stopSandboxRun: (runId: string) => void;

  folderAccessRequests: PendingFolderAccessRequest[];
  resolvingFolderCalls: Set<string>;
  folderAccessErrors: Record<string, string>;
  decideFolderAccess: (callId: string, decision: FolderAccessDecision) => void;
  cancelFolderAccess: (callId: string, turnId: string) => void;

  userQuestionRequests: PendingUserQuestions[];
  answeringQuestionCalls: Set<string>;
  userQuestionErrors: Record<string, string>;
  answerUserQuestions: (callId: string, answers: UserQuestionAnswer[]) => void;
  cancelUserQuestions: (turnId: string) => void;

  decidingApprovalCalls: Set<string>;
  approvalErrors: Record<string, string>;
  decideApproval: (
    callId: string,
    decision: "approve" | "reject",
    remember?: boolean,
  ) => void;

  cancelPendingTurnId: string | null;
  cancelError: string | null;
  cancelActiveTurn: () => Promise<void>;

  steerPendingTurnId: string | null;
  steerError: string | null;
  steerStatus: string | null;
  /** Settles when the guidance has been accepted or rejected, not on submit;
   *  the composer waits on it to decide where focus belongs afterwards. */
  steerActiveTurn: () => Promise<void>;
  /** Typing withdraws the last steer's outcome; the draft it described is gone. */
  clearSteerFeedback: () => void;
};

/**
 * How the root drives this state from the parts of the chat it still owns: the
 * event socket, which reports what the turn is doing, and chat deletion.
 */
export type ConversationRequestsHandle = {
  refreshAgentRuns: () => void;
  refreshFolderAccess: () => void;
  refreshUserQuestions: () => void;
  /** A turn started. A steer aimed at a turn that has been replaced is stale. */
  turnBegan: (startsDifferentTurn: boolean) => void;
  /** A turn ended, here or on the server. Nothing about it is still pending. */
  turnResolved: () => void;
  /** A turn was just submitted from this client; only cancel feedback is stale. */
  turnSubmitted: () => void;
  /** This conversation's chat is going away. Nothing in flight may land. */
  abandonForChatDeletion: () => void;
};

export type ConversationRequestsProviderProps = {
  client: ApiClient;
  chat: Chat;
  selection: ChatSelectionFence;
  /** The live composer draft, which is what a steer sends. */
  draftRef: RefObject<string>;
  /** Called when an accepted steer consumed the draft it was sent from. */
  onDraftAccepted: () => void;
  ref?: Ref<ConversationRequestsHandle>;
  children: ReactNode;
};

const ConversationRequestsContext = createContext<ConversationRequests | null>(
  null,
);

/**
 * Publishes a conversation's requests to the surfaces that render them, for a
 * caller that already holds the value. `ConversationRequestsProvider` is what
 * produces one from a live chat.
 */
export const ConversationRequestsScope = ConversationRequestsContext.Provider;

export function useConversationRequests(): ConversationRequests {
  const requests = useContext(ConversationRequestsContext);
  if (!requests) {
    throw new Error(
      "useConversationRequests must be used inside a ConversationRequestsProvider",
    );
  }
  return requests;
}

/**
 * Owns what one conversation has in flight: its agent runs, the approvals and
 * folder-access and question prompts it is parked on, and the steer and cancel
 * controls for its active turn.
 *
 * Mount with `key={chat.id}`. The state here is the conversation's, so a new
 * conversation gets a new instance rather than a reset — which is also what
 * makes a response that arrives after the switch land on nothing.
 */
export function ConversationRequestsProvider({
  client,
  chat,
  selection,
  draftRef,
  onDraftAccepted,
  ref,
  children,
}: ConversationRequestsProviderProps) {
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);

  const [agentRuns, setAgentRuns] = useState<AgentRun[]>([]);
  const [agentRunsChatId, setAgentRunsChatId] = useState<string | null>(null);
  const [agentRunsLoading, setAgentRunsLoading] = useState(false);
  const [agentRunsError, setAgentRunsError] = useState<string | null>(null);
  const [stoppingSandboxRunKeys, setStoppingSandboxRunKeys] = useState<
    Set<string>
  >(new Set());
  const [sandboxStopErrorKeys, setSandboxStopErrorKeys] = useState<Set<string>>(
    new Set(),
  );
  const [folderAccessRequests, setFolderAccessRequests] = useState<
    PendingFolderAccessRequest[]
  >([]);
  const [resolvingFolderCalls, setResolvingFolderCalls] = useState<Set<string>>(
    new Set(),
  );
  const [folderAccessErrors, setFolderAccessErrors] = useState<
    Record<string, string>
  >({});
  const [userQuestionRequests, setUserQuestionRequests] = useState<
    PendingUserQuestions[]
  >([]);
  const [answeringQuestionCalls, setAnsweringQuestionCalls] = useState<
    Set<string>
  >(new Set());
  const [userQuestionErrors, setUserQuestionErrors] = useState<
    Record<string, string>
  >({});
  const [decidingApprovalCalls, setDecidingApprovalCalls] = useState<
    Set<string>
  >(new Set());
  const [approvalErrors, setApprovalErrors] = useState<Record<string, string>>(
    {},
  );
  const [cancelPendingTurnId, setCancelPendingTurnId] = useState<string | null>(
    null,
  );
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [steerPendingTurnId, setSteerPendingTurnId] = useState<string | null>(
    null,
  );
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);

  const refreshFolderAccessRef = useRef<(() => void) | null>(null);
  const refreshUserQuestionsRef = useRef<(() => void) | null>(null);
  const refreshAgentRunsRef = useRef<(() => void) | null>(null);
  const resolvingFolderCallsRef = useRef<Set<string>>(new Set());
  const answeringQuestionCallsRef = useRef<Set<string>>(new Set());
  const seenQuestionCallIdsRef = useRef<Set<string>>(new Set());
  const decidingApprovalCallsRef = useRef<Set<string>>(new Set());
  const cancelRequestTurnRef = useRef<string | null>(null);
  const steerFenceRef = useRef(new ActiveTurnSteerFence());
  const sandboxStopFenceRef = useRef(new SandboxAgentStopFence());

  useEffect(() => {
    const steerFence = steerFenceRef.current;
    const sandboxStopFence = sandboxStopFenceRef.current;
    return () => {
      steerFence.invalidate();
      sandboxStopFence.invalidate();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const requests = await client.listPendingUserQuestions(chat.id);
        if (!cancelled && seq === requestSeq) {
          const hasNewRequest = requests.some(
            (request) => !seenQuestionCallIdsRef.current.has(request.callId),
          );
          seenQuestionCallIdsRef.current = new Set(
            requests.map((request) => request.callId),
          );
          setUserQuestionRequests(requests);
          if (hasNewRequest) {
            void requestUserAttention().catch(() => {
              // Attention is a best-effort hint. Durable polling is truth.
            });
          }
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to refresh pending user questions", err);
        }
      }
    };

    refreshUserQuestionsRef.current = () => void refresh();
    void refresh();
    const interval = window.setInterval(() => void refresh(), 10_000);
    return () => {
      cancelled = true;
      requestSeq += 1;
      window.clearInterval(interval);
      if (refreshUserQuestionsRef.current) {
        refreshUserQuestionsRef.current = null;
      }
      setUserQuestionRequests([]);
      seenQuestionCallIdsRef.current = new Set();
    };
  }, [client, chat.id]);

  useEffect(() => {
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const runs = await client.listAgentRuns(chat.id);
        if (!cancelled && seq === requestSeq) {
          setAgentRuns(runs);
          setAgentRunsError(null);
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          setAgentRunsError(String(err));
        }
      } finally {
        if (!cancelled && seq === requestSeq) {
          setAgentRunsLoading(false);
        }
      }
    };

    setAgentRuns([]);
    setAgentRunsChatId(chat.id);
    setAgentRunsError(null);
    setAgentRunsLoading(true);
    refreshAgentRunsRef.current = () => void refresh();
    void refresh();
    return () => {
      cancelled = true;
      requestSeq += 1;
      if (refreshAgentRunsRef.current) {
        refreshAgentRunsRef.current = null;
      }
    };
  }, [client, chat.id]);

  useEffect(() => {
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const requests = await client.listPendingFolderAccessRequests(chat.id);
        if (!cancelled && seq === requestSeq) {
          setFolderAccessRequests(requests);
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to refresh pending folder access", err);
          setFolderAccessRequests([]);
        }
      }
    };

    refreshFolderAccessRef.current = () => void refresh();
    void refresh();
    const interval = window.setInterval(() => void refresh(), 10_000);
    return () => {
      cancelled = true;
      requestSeq += 1;
      window.clearInterval(interval);
      if (refreshFolderAccessRef.current) {
        refreshFolderAccessRef.current = null;
      }
      setFolderAccessRequests([]);
    };
  }, [client, chat.id]);

  const visibleAgentRuns = agentRunsForChat(
    agentRunsChatId,
    chat.id,
    agentRuns,
  );
  const stoppingRunIds = new Set(
    visibleAgentRuns
      .filter((run) =>
        stoppingSandboxRunKeys.has(sandboxAgentStopKey(chat.id, run.id)),
      )
      .map((run) => run.id),
  );
  const stopErrorRunIds = new Set(
    visibleAgentRuns
      .filter((run) =>
        sandboxStopErrorKeys.has(sandboxAgentStopKey(chat.id, run.id)),
      )
      .map((run) => run.id),
  );
  const hasActiveSandboxRun = visibleAgentRuns.some(
    (run) =>
      run.execution === "sandbox" &&
      ["queued", "running", "cancelling", "waiting", "retry_wait"].includes(
        run.status,
      ),
  );

  useEffect(() => {
    if (!hasActiveSandboxRun) return;
    const interval = window.setInterval(
      () => refreshAgentRunsRef.current?.(),
      5_000,
    );
    return () => window.clearInterval(interval);
  }, [hasActiveSandboxRun]);

  function clearSteerRequestState() {
    steerFenceRef.current.invalidate();
    setSteerPendingTurnId(null);
    setSteerError(null);
    setSteerStatus(null);
  }

  function clearCancelRequestState() {
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
  }

  async function stopSandboxRun(runId: string) {
    if (selection.deleting.current) return;
    const target = visibleAgentRuns.find((run) => run.id === runId);
    if (!target || !canStopSandboxAgentRun(target)) return;

    const chatId = chat.id;
    const request = sandboxStopFenceRef.current.begin(chatId, runId);
    if (!request) return;
    const key = sandboxAgentStopKey(chatId, runId);
    setStoppingSandboxRunKeys((current) => new Set(current).add(key));
    setSandboxStopErrorKeys((current) => {
      const next = new Set(current);
      next.delete(key);
      return next;
    });

    try {
      const cancellation = await client.cancelAgentRun(chatId, runId);
      if (!sandboxStopFenceRef.current.isCurrent(request, selection.chatId.current)) {
        return;
      }
      setAgentRuns((current) =>
        reconcileSandboxAgentCancellation(current, cancellation),
      );
      refreshAgentRunsRef.current?.();
    } catch {
      if (!sandboxStopFenceRef.current.isCurrent(request, selection.chatId.current)) {
        return;
      }
      setSandboxStopErrorKeys((current) => new Set(current).add(key));
    } finally {
      if (sandboxStopFenceRef.current.finish(request, selection.chatId.current)) {
        setStoppingSandboxRunKeys((current) => {
          const next = new Set(current);
          next.delete(key);
          return next;
        });
      }
    }
  }

  async function decideApproval(
    callId: string,
    decision: "approve" | "reject",
    remember = false,
  ) {
    if (decidingApprovalCallsRef.current.has(callId)) return;
    decidingApprovalCallsRef.current.add(callId);
    setDecidingApprovalCalls((calls) => new Set(calls).add(callId));
    setApprovalErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await client.decideApproval(chat.id, callId, decision, remember);
      useChatSessionStore.getState().update((session) => ({
        ...session,
        messages: session.messages.map((m) =>
          m.role === "approval" && m.callId === callId
            ? { ...m, resolved: true }
            : m,
        ),
      }));
    } catch (err) {
      setApprovalErrors((errors) => ({
        ...errors,
        [callId]: `Could not send your decision: ${String(err)}`,
      }));
    } finally {
      decidingApprovalCallsRef.current.delete(callId);
      setDecidingApprovalCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
    }
  }

  async function decideFolderAccess(
    callId: string,
    decision: FolderAccessDecision,
  ) {
    if (!hasNativeHost()) return;
    if (resolvingFolderCallsRef.current.size > 0) return;
    resolvingFolderCallsRef.current.add(callId);
    setResolvingFolderCalls((calls) => new Set(calls).add(callId));
    setFolderAccessErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await resolveFolderAccessRequest(chat.id, callId, decision);
    } catch (err) {
      setFolderAccessErrors((errors) => ({
        ...errors,
        [callId]: String(err),
      }));
    } finally {
      resolvingFolderCallsRef.current.delete(callId);
      setResolvingFolderCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      refreshFolderAccessRef.current?.();
    }
  }

  async function cancelFolderAccess(callId: string, turnId: string) {
    if (resolvingFolderCallsRef.current.size > 0) return;
    resolvingFolderCallsRef.current.add(callId);
    setResolvingFolderCalls((calls) => new Set(calls).add(callId));
    setFolderAccessErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await client.cancel(chat.id, turnId);
    } catch (err) {
      setFolderAccessErrors((errors) => ({
        ...errors,
        [callId]: String(err),
      }));
    } finally {
      resolvingFolderCallsRef.current.delete(callId);
      setResolvingFolderCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      refreshFolderAccessRef.current?.();
    }
  }

  async function answerUserQuestions(
    callId: string,
    answers: UserQuestionAnswer[],
  ) {
    if (answeringQuestionCallsRef.current.has(callId)) return;
    const chatId = chat.id;
    const selected = selection.selection.current;
    answeringQuestionCallsRef.current.add(callId);
    setAnsweringQuestionCalls((calls) => new Set(calls).add(callId));
    setUserQuestionErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await client.answerUserQuestions(chatId, callId, answers);
    } catch (err) {
      if (selection.selection.current === selected) {
        setUserQuestionErrors((errors) => ({
          ...errors,
          [callId]: `Could not send your answer: ${String(err)}`,
        }));
      }
    } finally {
      answeringQuestionCallsRef.current.delete(callId);
      setAnsweringQuestionCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      if (selection.selection.current === selected) {
        refreshUserQuestionsRef.current?.();
      }
    }
  }

  async function cancelUserQuestions(turnId: string) {
    const request = userQuestionRequests.find(
      (candidate) => candidate.turnId === turnId,
    );
    if (!request || answeringQuestionCallsRef.current.has(request.callId)) {
      return;
    }
    const chatId = chat.id;
    const selected = selection.selection.current;
    answeringQuestionCallsRef.current.add(request.callId);
    setAnsweringQuestionCalls((calls) => new Set(calls).add(request.callId));
    setUserQuestionErrors((errors) => {
      const next = { ...errors };
      delete next[request.callId];
      return next;
    });
    try {
      await client.cancel(chatId, turnId);
    } catch (err) {
      if (selection.selection.current === selected) {
        setUserQuestionErrors((errors) => ({
          ...errors,
          [request.callId]: `Could not cancel the turn: ${String(err)}`,
        }));
      }
    } finally {
      answeringQuestionCallsRef.current.delete(request.callId);
      setAnsweringQuestionCalls((calls) => {
        const next = new Set(calls);
        next.delete(request.callId);
        return next;
      });
      if (selection.selection.current === selected) {
        refreshUserQuestionsRef.current?.();
      }
    }
  }

  async function steerActiveTurn() {
    const admission = {
      busy,
      turnId: useChatSessionStore.getState().activeTurnId,
      cancelRequestTurnId: cancelRequestTurnRef.current,
      deletionInFlight: selection.deleting.current,
    };
    if (!canBeginActiveTurnSteer(admission)) return;
    const turnId = admission.turnId;

    const request = steerFenceRef.current.begin(
      {
        chatId: chat.id,
        turnId,
        selection: selection.selection.current,
      },
      draftRef.current,
      () => crypto.randomUUID(),
    );
    if (!request) return;

    setSteerPendingTurnId(turnId);
    setSteerError(null);
    setSteerStatus("Sending guidance…");
    setCancelError(null);
    try {
      await client.steer(
        request.chatId,
        request.turnId,
        request.steerId,
        request.content,
        true,
      );
      if (!steerFenceRef.current.canApplyResponse(request, currentSteerTarget())) {
        return;
      }

      steerFenceRef.current.finish(request);
      setSteerPendingTurnId(null);
      if (shouldClearAcceptedSteerDraft(request, draftRef.current)) {
        onDraftAccepted();
      }
      setSteerStatus("Guidance sent");
    } catch (err) {
      if (!steerFenceRef.current.canApplyResponse(request, currentSteerTarget())) {
        return;
      }

      steerFenceRef.current.fail(request);
      setSteerPendingTurnId(null);
      setSteerStatus(null);
      setSteerError(String(err));
    }
  }

  function currentSteerTarget() {
    return {
      chatId: selection.chatId.current ?? "",
      turnId: useChatSessionStore.getState().activeTurnId ?? "",
      selection: selection.selection.current,
    };
  }

  async function cancelActiveTurn() {
    const turnId = activeTurnId;
    if (!busy || !turnId || cancelRequestTurnRef.current === turnId) return;
    const selected = selection.selection.current;
    const chatId = chat.id;

    cancelRequestTurnRef.current = turnId;
    setCancelPendingTurnId(turnId);
    setCancelError(null);
    try {
      await client.cancel(chatId, turnId);
    } catch (err) {
      if (
        selection.selection.current === selected &&
        cancelRequestTurnRef.current === turnId
      ) {
        cancelRequestTurnRef.current = null;
        setCancelPendingTurnId(null);
        setCancelError(String(err));
      }
    }
  }

  useImperativeHandle(
    ref,
    (): ConversationRequestsHandle => ({
      refreshAgentRuns: () => refreshAgentRunsRef.current?.(),
      refreshFolderAccess: () => refreshFolderAccessRef.current?.(),
      refreshUserQuestions: () => refreshUserQuestionsRef.current?.(),
      turnBegan: (startsDifferentTurn) => {
        clearCancelRequestState();
        if (startsDifferentTurn) clearSteerRequestState();
      },
      turnResolved: () => {
        clearCancelRequestState();
        clearSteerRequestState();
      },
      turnSubmitted: () => {
        setCancelPendingTurnId(null);
        setCancelError(null);
      },
      abandonForChatDeletion: () => {
        clearSteerRequestState();
        sandboxStopFenceRef.current.invalidate();
        setStoppingSandboxRunKeys(new Set());
        setSandboxStopErrorKeys(new Set());
      },
    }),
    [],
  );

  const requests: ConversationRequests = {
    agentRuns: visibleAgentRuns,
    agentRunsLoading: agentRunsChatId === chat.id ? agentRunsLoading : true,
    agentRunsError: agentRunsChatId === chat.id ? agentRunsError : null,
    stoppingRunIds,
    stopErrorRunIds,
    refreshAgentRuns: () => refreshAgentRunsRef.current?.(),
    stopSandboxRun: (runId) => void stopSandboxRun(runId),

    folderAccessRequests,
    resolvingFolderCalls,
    folderAccessErrors,
    decideFolderAccess: (callId, decision) =>
      void decideFolderAccess(callId, decision),
    cancelFolderAccess: (callId, turnId) =>
      void cancelFolderAccess(callId, turnId),

    userQuestionRequests,
    answeringQuestionCalls,
    userQuestionErrors,
    answerUserQuestions: (callId, answers) =>
      void answerUserQuestions(callId, answers),
    cancelUserQuestions: (turnId) => void cancelUserQuestions(turnId),

    decidingApprovalCalls,
    approvalErrors,
    decideApproval: (callId, decision, remember) =>
      void decideApproval(callId, decision, remember),

    cancelPendingTurnId,
    cancelError,
    cancelActiveTurn,

    steerPendingTurnId,
    steerError,
    steerStatus,
    steerActiveTurn,
    clearSteerFeedback: () => {
      setSteerError(null);
      setSteerStatus(null);
    },
  };

  return (
    <ConversationRequestsContext.Provider value={requests}>
      {children}
    </ConversationRequestsContext.Provider>
  );
}
