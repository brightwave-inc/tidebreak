import { useEffect, useRef, useState } from "react";
import {
  ApiClient,
  type AgentRun,
  type Chat,
  type ModelInfo,
  type PendingFolderAccessRequest,
  type ProviderInfo,
  type ProviderKind,
  type SequencedEvent,
  type ServerInfo,
  type WebSearchConfigInfo,
  type WebSearchCredentialReadiness,
  type WebSearchProviderKind,
} from "./api";
import { resolveServerInfo } from "./boot";
import {
  connectFolder,
  disconnectFolder,
  hasNativeHost,
  listConnectedFolders,
  resolveFolderAccessRequest,
  type ConnectedFolder,
  type FolderAccessDecision,
} from "./host";
import { Logomark } from "./Logomark";
import { Composer } from "./Composer";
import { MessageList, type ChatMessage } from "./MessageList";
import {
  toolApprovalPresentation,
  type ToolCallStatus,
} from "./ToolCallCard";
import { useChatEventStream } from "./ChatEventStream";
import { isNearBottom, scrollToLatest } from "./ChatScroll";
import { DocumentsView } from "./DocumentsView";
import {
  reconcilePendingApprovalCards,
  upsertPendingApprovalCard,
} from "./ApprovalHistory";
import { loadChatApprovalHydration } from "./ChatApprovalHydration";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import {
  ActiveTurnSteerFence,
  canBeginActiveTurnSteer,
  shouldClearAcceptedSteerDraft,
} from "./ActiveTurnSteer";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { AgentActivityPanel, agentRunsForChat } from "./AgentActivityPanel";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
  sandboxAgentStopKey,
} from "./SandboxAgentStop";

type Msg = ChatMessage;

let msgSeq = 0;

function nextId(): string {
  msgSeq += 1;
  return `m${msgSeq}`;
}

export default function App() {
  const [bootError, setBootError] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [chat, setChat] = useState<Chat | null>(null);
  const [hydratedChatId, setHydratedChatId] = useState<string | null>(null);
  const [chats, setChats] = useState<Chat[]>([]);
  const [chatsError, setChatsError] = useState<string | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [agentRuns, setAgentRuns] = useState<AgentRun[]>([]);
  const [agentRunsChatId, setAgentRunsChatId] = useState<string | null>(null);
  const [agentRunsLoading, setAgentRunsLoading] = useState(false);
  const [agentRunsError, setAgentRunsError] = useState<string | null>(null);
  const [stoppingSandboxRunKeys, setStoppingSandboxRunKeys] = useState<Set<string>>(
    new Set(),
  );
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
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [activeTurnId, setActiveTurnId] = useState<string | null>(null);
  const [cancelPendingTurnId, setCancelPendingTurnId] = useState<string | null>(
    null,
  );
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [steerPendingTurnId, setSteerPendingTurnId] = useState<string | null>(
    null,
  );
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const [creatingChat, setCreatingChat] = useState(false);
  const [deletingChatId, setDeletingChatId] = useState<string | null>(null);
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [savingTitle, setSavingTitle] = useState(false);
  const [settingsPanel, setSettingsPanel] = useState<
    "providers" | "web-search" | "folders" | null
  >(null);
  const [primaryView, setPrimaryView] = useState<"chat" | "documents">("chat");
  const [status, setStatus] = useState("starting…");
  const [hasUnreadActivity, setHasUnreadActivity] = useState(false);
  const socketRef = useRef<WebSocket | null>(null);
  const socketGenerationRef = useRef(0);
  const chatSelectionRef = useRef(0);
  const hydratedMessageIdsRef = useRef<Set<string>>(new Set());
  const lastSeqRef = useRef(0);
  const assistantBufRef = useRef("");
  const assistantMarkerScrubberRef = useRef(
    new AssistantSourceMarkerStreamScrubber(),
  );
  const terminalHydrationGenerationRef = useRef(0);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followsLatestRef = useRef(true);
  const refreshFolderAccessRef = useRef<(() => void) | null>(null);
  const refreshAgentRunsRef = useRef<(() => void) | null>(null);
  const resolvingFolderCallsRef = useRef<Set<string>>(new Set());
  const visibleFolderCallIdsRef = useRef<Set<string>>(new Set());
  const cancelRequestTurnRef = useRef<string | null>(null);
  const activeTurnIdRef = useRef<string | null>(null);
  const draftRef = useRef("");
  const selectedChatIdRef = useRef<string | null>(null);
  const steerFenceRef = useRef(new ActiveTurnSteerFence());
  const sandboxStopFenceRef = useRef(new SandboxAgentStopFence());
  const provisionalToolCallIdsRef = useRef<Set<string>>(new Set());
  const deletionInFlightRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const server = await resolveServerInfo();
        if (cancelled) return;
        setInfo(server);
        setClient(new ApiClient(server.baseUrl, server.token));
        setStatus(`connected ${server.baseUrl}`);
      } catch (err) {
        if (!cancelled) setBootError(String(err));
      }
    })();
    return () => {
      cancelled = true;
      terminalHydrationGenerationRef.current += 1;
      steerFenceRef.current.invalidate();
      sandboxStopFenceRef.current.invalidate();
    };
  }, []);

  useEffect(() => {
    if (!client || !info) return;
    let cancelled = false;
    (async () => {
      try {
        const [catalog, providerList, existingChats] = await Promise.all([
          client.listModels(),
          client.listProviders(),
          client.listChats(),
        ]);
        if (cancelled) return;
        setModels(catalog.models);
        setProviders(providerList.providers);
        setChats(existingChats);
        const created =
          existingChats[0] ??
          (await client.createChat(catalog.models[0]?.id));
        if (cancelled) return;
        if (existingChats.length === 0) setChats([created]);
        activateChat(created);
        setStatus(`chat ${created.id.slice(0, 8)}…`);
      } catch (err) {
        if (!cancelled) setBootError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, info]);

  useEffect(() => {
    if (!client || !chat) return;
    let cancelled = false;
    const selection = chatSelectionRef.current;
    setHydratedChatId(null);
    setMessages([]);
    hydratedMessageIdsRef.current = new Set();
    (async () => {
      try {
        const hydration = await loadChatApprovalHydration(
          client,
          chat.id,
          () => !cancelled && selection === chatSelectionRef.current,
        );
        if (!hydration) return;
        const { transcript, pendingApprovals } = hydration;
        lastSeqRef.current = transcript.last_event_seq;
        const presented = presentChatTranscript(transcript);
        hydratedMessageIdsRef.current = presented.messageIds;
        setMessages(
          reconcilePendingApprovalCards(presented.messages, pendingApprovals),
        );
        const pendingTurnId = pendingApprovals[0]?.turnId ?? null;
        setCurrentActiveTurnId(pendingTurnId);
        setBusy(pendingTurnId !== null);
        setHydratedChatId(chat.id);
      } catch (err) {
        if (!cancelled && selection === chatSelectionRef.current) {
          setBusy(true);
          setMessages([
            {
              id: nextId(),
              role: "error",
              text: `Could not load this conversation: ${String(err)}`,
            },
          ]);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, chat?.id]);

  useChatEventStream({
    client,
    chatId: chat?.id ?? null,
    ready: hydratedChatId === chat?.id,
    afterRef: lastSeqRef,
    socketRef,
    generationRef: socketGenerationRef,
    onEvent: handleEvent,
    onConnectionState: (connectionState) =>
      setStatus((current) =>
        `${withoutConnectionState(current)} · ${connectionState}`,
      ),
  });

  useEffect(() => {
    if (!client || !chat) return;
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
  }, [client, chat?.id]);

  const visibleAgentRuns = agentRunsForChat(agentRunsChatId, chat?.id ?? null, agentRuns);
  const visibleStoppingSandboxRunIds = new Set(
    visibleAgentRuns
      .filter((run) =>
        stoppingSandboxRunKeys.has(sandboxAgentStopKey(chat?.id ?? "", run.id)),
      )
      .map((run) => run.id),
  );
  const visibleSandboxStopErrorRunIds = new Set(
    visibleAgentRuns
      .filter((run) =>
        sandboxStopErrorKeys.has(sandboxAgentStopKey(chat?.id ?? "", run.id)),
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

  async function onStopSandboxAgentRun(runId: string) {
    if (!client || !chat || deletionInFlightRef.current) return;
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
      if (!sandboxStopFenceRef.current.isCurrent(request, selectedChatIdRef.current)) {
        return;
      }
      setAgentRuns((current) =>
        reconcileSandboxAgentCancellation(current, cancellation),
      );
      refreshAgentRunsRef.current?.();
    } catch {
      if (!sandboxStopFenceRef.current.isCurrent(request, selectedChatIdRef.current)) {
        return;
      }
      setSandboxStopErrorKeys((current) => new Set(current).add(key));
    } finally {
      if (sandboxStopFenceRef.current.finish(request, selectedChatIdRef.current)) {
        setStoppingSandboxRunKeys((current) => {
          const next = new Set(current);
          next.delete(key);
          return next;
        });
      }
    }
  }

  useEffect(() => {
    if (!client || !chat) return;
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
  }, [client, chat?.id]);

  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll);
    } else {
      setHasUnreadActivity(true);
    }
  }, [messages]);

  useEffect(() => {
    const next = new Set(folderAccessRequests.map((request) => request.callId));
    const gainedRequest = [...next].some(
      (callId) => !visibleFolderCallIdsRef.current.has(callId),
    );
    visibleFolderCallIdsRef.current = next;
    if (!gainedRequest) return;
    const scroll = scrollRef.current;
    if (!scroll) return;
    if (followsLatestRef.current) {
      scrollToLatest(scroll);
    } else {
      setHasUnreadActivity(true);
    }
  }, [folderAccessRequests]);

  function handleEvent(framed: SequencedEvent) {
    if (framed.seq <= lastSeqRef.current) return;
    lastSeqRef.current = framed.seq;
    const event = framed.event;

    if (event.type === "turn_started") {
      const startsDifferentTurn = activeTurnIdRef.current !== event.turn_id;
      refreshAgentRunsRef.current?.();
      terminalHydrationGenerationRef.current += 1;
      assistantBufRef.current = "";
      assistantMarkerScrubberRef.current =
        new AssistantSourceMarkerStreamScrubber();
      provisionalToolCallIdsRef.current = new Set();
      setBusy(true);
      setCurrentActiveTurnId(event.turn_id);
      setCancelPendingTurnId(null);
      setCancelError(null);
      cancelRequestTurnRef.current = null;
      if (startsDifferentTurn) clearSteerRequestState();
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "assistant",
          text: "",
          sources: [],
          createdAt: new Date().toISOString(),
        },
      ]);
      return;
    }

    if (event.type === "text_delta") {
      assistantBufRef.current += assistantMarkerScrubberRef.current.push(
        event.text,
      );
      const text = assistantBufRef.current;
      setMessages((prev) => {
        const copy = [...prev];
        const last = copy[copy.length - 1];
        if (last?.role === "assistant") {
          copy[copy.length - 1] = { ...last, text };
        } else {
          copy.push({
            id: nextId(),
            role: "assistant",
            text,
            sources: [],
            createdAt: new Date().toISOString(),
          });
        }
        return copy;
      });
      return;
    }

    if (event.type === "stream_interrupted") {
      // The whole optimistic candidate is invalidated at this boundary. Finish
      // clears any withheld marker-like tail before the replacement starts.
      assistantMarkerScrubberRef.current.finish();
      assistantBufRef.current = "";
      const provisionalCallIds = provisionalToolCallIdsRef.current;
      provisionalToolCallIdsRef.current = new Set();
      setMessages((prev) => {
        const copy = discardToolCalls(prev, provisionalCallIds);
        if (copy[copy.length - 1]?.role === "assistant") {
          copy.pop();
        }
        return copy;
      });
      return;
    }

    if (event.type === "tool_call_started") {
      flushAssistantMarkerTail();
      if (provisionalToolCallIdsRef.current.size === 0) {
        assistantBufRef.current = "";
        assistantMarkerScrubberRef.current =
          new AssistantSourceMarkerStreamScrubber();
      }
      refreshAgentRunsRef.current?.();
      if (event.name === "request_folder_access") {
        refreshFolderAccessRef.current?.();
      }
      provisionalToolCallIdsRef.current.add(event.call_id);
      setMessages((prev) =>
        upsertToolCall(prev, event.call_id, event.name, "running"),
      );
      return;
    }

    if (event.type === "tool_call_args_delta") {
      // Arguments are intentionally not retained in renderer state. They can
      // contain paths, file content, credentials, or provider-specific data.
      setMessages((prev) =>
        updateToolCall(prev, event.call_id, (tool) => ({
          ...tool,
          status: tool.status === "waiting_approval" ? tool.status : "running",
        })),
      );
      return;
    }

    if (event.type === "approval_required") {
      const approval = toolApprovalPresentation(event.approval);
      provisionalToolCallIdsRef.current.delete(event.call_id);
      setMessages((prev) =>
        upsertPendingApprovalCard(prev, {
          callId: event.call_id,
          action: event.action,
          approval: event.approval,
          canApprove: approval.canApprove,
        }),
      );
      return;
    }

    if (event.type === "approval_decided") {
      setMessages((prev) =>
        updateApprovalAndToolCall(prev, event.call_id, event.approved),
      );
      return;
    }

    if (event.type === "tool_call_completed") {
      provisionalToolCallIdsRef.current.delete(event.call_id);
      setMessages((prev) =>
        updateToolCall(prev, event.call_id, (tool) => ({
          ...tool,
          status:
            tool.status === "cancelled"
              ? "cancelled"
              : event.status === "failed"
                ? "failed"
                : "completed",
        })),
      );
      return;
    }

    if (event.type === "user_steered") {
      if (hydratedMessageIdsRef.current.has(event.message_id)) return;
      hydratedMessageIdsRef.current.add(event.message_id);
      setMessages((prev) => [
        ...prev,
        {
          id: event.message_id,
          role: "user",
          text: event.text,
          createdAt: new Date().toISOString(),
        },
      ]);
      return;
    }

    if (event.type === "turn_completed") {
      flushAssistantMarkerTail();
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      const selection = chatSelectionRef.current;
      const generation = ++terminalHydrationGenerationRef.current;
      if (chat) {
        void refreshTerminalTranscript(chat.id, selection, generation);
      }
      return;
    }

    if (event.type === "turn_cancelled") {
      flushAssistantMarkerTail();
      terminalHydrationGenerationRef.current += 1;
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      setMessages((prev) => [
        ...settleActiveToolCalls(prev, "cancelled"),
        { id: nextId(), role: "system", text: "turn cancelled" },
      ]);
      return;
    }

    if (event.type === "turn_failed") {
      flushAssistantMarkerTail();
      terminalHydrationGenerationRef.current += 1;
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      setMessages((prev) => [
        ...settleActiveToolCalls(prev, "failed"),
        {
          id: nextId(),
          role: "error",
          text: "The turn could not be completed.",
        },
      ]);
    }
  }

  function flushAssistantMarkerTail() {
    const tail = assistantMarkerScrubberRef.current.finish();
    if (!tail) return;
    assistantBufRef.current += tail;
    const text = assistantBufRef.current;
    setMessages((previous) => {
      const copy = [...previous];
      const last = copy[copy.length - 1];
      if (last?.role === "assistant") {
        copy[copy.length - 1] = { ...last, text };
      } else {
        copy.push({
          id: nextId(),
          role: "assistant",
          text,
          sources: [],
          createdAt: new Date().toISOString(),
        });
      }
      return copy;
    });
  }

  async function refreshTerminalTranscript(
    chatId: string,
    selection: number,
    generation: number,
  ) {
    if (!client) return;
    try {
      const presented = await loadCurrentTerminalTranscript(
        client,
        chatId,
        () =>
          chatSelectionRef.current === selection &&
          terminalHydrationGenerationRef.current === generation,
      );
      if (!presented) return;
      lastSeqRef.current = Math.max(
        lastSeqRef.current,
        presented.lastEventSeq,
      );
      hydratedMessageIdsRef.current = presented.messageIds;
      setMessages(presented.messages);
    } catch {
      // The scrubbed optimistic response remains safe and visible. Reopening
      // the conversation will load a fresh authoritative snapshot.
    }
  }

  function resolveActiveTurn() {
    setBusy(false);
    setCurrentActiveTurnId(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    clearSteerRequestState();
  }

  function setCurrentActiveTurnId(turnId: string | null) {
    activeTurnIdRef.current = turnId;
    setActiveTurnId(turnId);
  }

  function setComposerDraft(nextDraft: string) {
    draftRef.current = nextDraft;
    setDraft(nextDraft);
  }

  function clearSteerRequestState() {
    steerFenceRef.current.invalidate();
    setSteerPendingTurnId(null);
    setSteerError(null);
    setSteerStatus(null);
  }

  function onComposerDraftChange(nextDraft: string) {
    setComposerDraft(nextDraft);
    setSteerError(null);
    setSteerStatus(null);
  }

  async function refreshCatalog() {
    if (!client) return;
    const [catalog, providerList] = await Promise.all([
      client.listModels(),
      client.listProviders(),
    ]);
    setModels(catalog.models);
    setProviders(providerList.providers);
  }

  async function onSend() {
    if (!client || !chat || !draft.trim() || busy || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const content = draft.trim();
    const turnId = crypto.randomUUID();
    terminalHydrationGenerationRef.current += 1;
    setComposerDraft("");
    setMessages((prev) => [
      ...prev,
      {
        id: nextId(),
        role: "user",
        text: content,
        createdAt: new Date().toISOString(),
      },
    ]);
    setBusy(true);
    setCurrentActiveTurnId(turnId);
    setCancelPendingTurnId(null);
    setCancelError(null);
    try {
      await client.postMessage(chatId, turnId, content);
      if (chatSelectionRef.current !== selection) return;
      refreshAgentRunsRef.current?.();
    } catch (err) {
      if (chatSelectionRef.current !== selection) return;
      resolveActiveTurn();
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "error", text: String(err) },
      ]);
    }
  }

  async function onSteerActiveTurn() {
    const admission = {
      busy,
      turnId: activeTurnIdRef.current,
      cancelRequestTurnId: cancelRequestTurnRef.current,
      deletionInFlight: deletionInFlightRef.current,
    };
    if (
      !client ||
      !chat ||
      !canBeginActiveTurnSteer(admission)
    ) {
      return;
    }
    const turnId = admission.turnId;

    const request = steerFenceRef.current.begin(
      {
        chatId: chat.id,
        turnId,
        selection: chatSelectionRef.current,
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
      if (
        !steerFenceRef.current.canApplyResponse(request, {
          chatId: selectedChatIdRef.current ?? "",
          turnId: activeTurnIdRef.current ?? "",
          selection: chatSelectionRef.current,
        })
      ) {
        return;
      }

      steerFenceRef.current.finish(request);
      setSteerPendingTurnId(null);
      if (shouldClearAcceptedSteerDraft(request, draftRef.current)) {
        setComposerDraft("");
      }
      setSteerStatus("Guidance sent");
    } catch (err) {
      if (
        !steerFenceRef.current.canApplyResponse(request, {
          chatId: selectedChatIdRef.current ?? "",
          turnId: activeTurnIdRef.current ?? "",
          selection: chatSelectionRef.current,
        })
      ) {
        return;
      }

      steerFenceRef.current.fail(request);
      setSteerPendingTurnId(null);
      setSteerStatus(null);
      setSteerError(String(err));
    }
  }

  async function onCancelActiveTurn() {
    const turnId = activeTurnId;
    if (
      !client ||
      !chat ||
      !busy ||
      !turnId ||
      cancelRequestTurnRef.current === turnId
    ) {
      return;
    }
    const selection = chatSelectionRef.current;
    const chatId = chat.id;

    cancelRequestTurnRef.current = turnId;
    setCancelPendingTurnId(turnId);
    setCancelError(null);
    try {
      await client.cancel(chatId, turnId);
    } catch (err) {
      if (
        chatSelectionRef.current === selection &&
        cancelRequestTurnRef.current === turnId
      ) {
        cancelRequestTurnRef.current = null;
        setCancelPendingTurnId(null);
        setCancelError(String(err));
      }
    }
  }

  async function onNewChat() {
    if (!client || creatingChat || deletionInFlightRef.current) return;
    setCreatingChat(true);
    setPrimaryView("chat");
    try {
      const created = await client.createChat(chat?.model ?? models[0]?.id);
      socketGenerationRef.current += 1;
      socketRef.current?.close();
      socketRef.current = null;
      assistantBufRef.current = "";
      assistantMarkerScrubberRef.current =
        new AssistantSourceMarkerStreamScrubber();
      provisionalToolCallIdsRef.current = new Set();
      lastSeqRef.current = 0;
      followsLatestRef.current = true;
      setHasUnreadActivity(false);
      setMessages([]);
      hydratedMessageIdsRef.current = new Set();
      setAgentRuns([]);
      setAgentRunsError(null);
      setFolderAccessRequests([]);
      setFolderAccessErrors({});
      setComposerDraft("");
      setBusy(false);
      setCurrentActiveTurnId(null);
      setCancelPendingTurnId(null);
      setCancelError(null);
      cancelRequestTurnRef.current = null;
      clearSteerRequestState();
      activateChat(created);
      setChats((current) => [created, ...current]);
      setChatsError(null);
      setStatus(`chat ${created.id.slice(0, 8)}…`);
    } catch (err) {
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "error",
          text: `Could not create a chat: ${String(err)}`,
        },
      ]);
    } finally {
      setCreatingChat(false);
    }
  }

  async function onDeleteChat(target: Chat) {
    if (!client || deletionInFlightRef.current || creatingChat) return;
    const label = target.title?.trim() || "this conversation";
    if (!window.confirm(`Delete ${label}? This cannot be undone.`)) return;

    deletionInFlightRef.current = true;
    setDeletingChatId(target.id);
    setChatsError(null);
    const deletingSelectedChat = chat?.id === target.id;
    if (deletingSelectedChat) {
      // This invalidates callbacks that captured the deleted selection. The
      // ref update also gates sends before the disabled composer renders.
      chatSelectionRef.current += 1;
      clearSteerRequestState();
      sandboxStopFenceRef.current.invalidate();
      setStoppingSandboxRunKeys(new Set());
      setSandboxStopErrorKeys(new Set());
    }
    try {
      await client.deleteChat(target.id);
      let refreshed = await client.listChats();
      if (!deletingSelectedChat) {
        setChats(refreshed);
        return;
      }

      let next = refreshed[0];
      if (!next) {
        next = await client.createChat(models[0]?.id);
        refreshed = await client.listChats();
      }
      setChats(refreshed);
      selectChat(next, true);
    } catch (err) {
      setChatsError(`Could not delete conversation: ${String(err)}`);
    } finally {
      deletionInFlightRef.current = false;
      setDeletingChatId(null);
    }
  }

  function activateChat(next: Chat) {
    chatSelectionRef.current += 1;
    terminalHydrationGenerationRef.current += 1;
    selectedChatIdRef.current = next.id;
    sandboxStopFenceRef.current.invalidate();
    setStoppingSandboxRunKeys(new Set());
    setSandboxStopErrorKeys(new Set());
    assistantMarkerScrubberRef.current =
      new AssistantSourceMarkerStreamScrubber();
    setChat(next);
  }

  function selectChat(next: Chat, force = false) {
    setPrimaryView("chat");
    setSettingsPanel(null);
    if (next.id === chat?.id || creatingChat || (!force && deletionInFlightRef.current)) return;
    socketGenerationRef.current += 1;
    socketRef.current?.close();
    socketRef.current = null;
    assistantBufRef.current = "";
    assistantMarkerScrubberRef.current =
      new AssistantSourceMarkerStreamScrubber();
    lastSeqRef.current = 0;
    followsLatestRef.current = true;
    setHasUnreadActivity(false);
    setMessages([]);
    hydratedMessageIdsRef.current = new Set();
    setAgentRuns([]);
    setAgentRunsError(null);
    setFolderAccessRequests([]);
    setFolderAccessErrors({});
    setComposerDraft("");
    setBusy(false);
    setCurrentActiveTurnId(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    clearSteerRequestState();
    setEditingTitle(false);
    activateChat(next);
    setStatus(`chat ${next.id.slice(0, 8)}…`);
  }

  async function onRenameChat() {
    if (!client || !chat || savingTitle || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    setSavingTitle(true);
    try {
      const updated = await client.patchChatTitle(chatId, titleDraft.trim() || null);
      setChats((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      if (chatSelectionRef.current !== selection) return;
      setChat(updated);
      setEditingTitle(false);
    } catch (err) {
      if (chatSelectionRef.current !== selection) return;
      setChatsError(`Could not rename conversation: ${String(err)}`);
    } finally {
      setSavingTitle(false);
    }
  }

  async function onModelChange(modelId: string) {
    if (!client || !chat || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const updated = await client.patchChatModel(chatId, modelId || null);
    if (chatSelectionRef.current !== selection) {
      setChats((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      return;
    }
    setChat(updated);
    setChats((current) =>
      current.map((item) => (item.id === updated.id ? updated : item)),
    );
  }

  async function onApproval(callId: string, decision: "approve" | "reject") {
    if (!client || !chat) return;
    await client.decideApproval(chat.id, callId, decision);
    setMessages((prev) =>
      prev.map((m) =>
        m.role === "approval" && m.callId === callId
          ? { ...m, resolved: true }
          : m,
      ),
    );
  }

  async function onFolderAccessDecision(
    callId: string,
    decision: FolderAccessDecision,
  ) {
    if (!chat || !hasNativeHost()) return;
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

  async function onFolderAccessCancel(callId: string, turnId: string) {
    if (!client || !chat || resolvingFolderCallsRef.current.size > 0) return;
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

  if (bootError) {
    return (
      <div className="boot">
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{bootError}</p>
      </div>
    );
  }

  if (!client || !chat) {
    return (
      <div className="boot">
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{status}</p>
      </div>
    );
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="sidebar-brand">
          <Logomark />
          <span>OpenWave</span>
        </div>

        <button
          type="button"
          className="new-chat"
          onClick={() => void onNewChat()}
          disabled={creatingChat || deletingChatId !== null}
        >
          <span aria-hidden="true">+</span>
          {creatingChat ? "Starting…" : deletingChatId ? "Deleting…" : "New chat"}
        </button>

        {hasNativeHost() && (
          <button
            type="button"
            className={`sidebar-action sidebar-library${primaryView === "documents" ? " is-active" : ""}`}
            onClick={() => {
              setSettingsPanel(null);
              setPrimaryView("documents");
            }}
          >
            <span aria-hidden="true">▤</span>
            Documents
          </button>
        )}

        <div className="sidebar-section">
          <span className="sidebar-label">Conversations</span>
          <div className="conversation-list" aria-label="Conversations">
            {chats.map((item) => (
              <div
                key={item.id}
                className={`conversation-row${primaryView === "chat" && item.id === chat.id ? " is-active" : ""}`}
              >
                <button
                  type="button"
                  className="conversation-item"
                  aria-current={primaryView === "chat" && item.id === chat.id ? "page" : undefined}
                  disabled={deletingChatId !== null || creatingChat}
                  onClick={() => selectChat(item)}
                >
                  <span className="conversation-dot" aria-hidden="true" />
                  <span>{item.title?.trim() || "New conversation"}</span>
                </button>
                <button
                  type="button"
                  className="conversation-delete"
                  aria-label={`Delete ${item.title?.trim() || "conversation"}`}
                  title="Delete conversation"
                  disabled={deletingChatId !== null || creatingChat}
                  onClick={() => void onDeleteChat(item)}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
          {chatsError && <p className="sidebar-error">{chatsError}</p>}
        </div>

        <div className="sidebar-footer">
          {hasNativeHost() && (
            <button
              type="button"
              className={`sidebar-action${settingsPanel === "folders" ? " is-active" : ""}`}
              onClick={() => {
                setPrimaryView("chat");
                setSettingsPanel((panel) =>
                  panel === "folders" ? null : "folders",
                );
              }}
            >
              Folders
            </button>
          )}
          <button
            type="button"
            className={`sidebar-action${settingsPanel === "providers" ? " is-active" : ""}`}
            onClick={() => {
              setPrimaryView("chat");
              setSettingsPanel((panel) =>
                panel === "providers" ? null : "providers",
              );
            }}
          >
            Providers
          </button>
          <button
            type="button"
            className={`sidebar-action${settingsPanel === "web-search" ? " is-active" : ""}`}
            onClick={() => {
              setPrimaryView("chat");
              setSettingsPanel((panel) =>
                panel === "web-search" ? null : "web-search",
              );
            }}
          >
            Web search
          </button>
        </div>
      </aside>

      <div
        className={`main${primaryView === "chat" && settingsPanel ? " with-settings" : ""}`}
      >
        {primaryView === "documents" ? (
          <DocumentsView chatId={chat.id} onBack={() => setPrimaryView("chat")} />
        ) : (
          <>
        <section className="chat-pane">
          <header className="conversation-header">
            <div>
              <p className="eyebrow">Conversation</p>
              {editingTitle ? (
                <form
                  className="conversation-title-editor"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void onRenameChat();
                  }}
                >
                  <input
                    autoFocus
                    value={titleDraft}
                    aria-label="Conversation title"
                    onChange={(event) => setTitleDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") setEditingTitle(false);
                    }}
                  />
                  <button
                    type="submit"
                    className="btn"
                    disabled={savingTitle || deletingChatId !== null}
                  >
                    {savingTitle ? "Saving…" : "Save"}
                  </button>
                </form>
              ) : (
                <div className="conversation-title-row">
                  <h1>{chat.title?.trim() || "New conversation"}</h1>
                  <button
                    type="button"
                    className="title-action"
                    disabled={deletingChatId !== null}
                    onClick={() => {
                      setTitleDraft(chat.title ?? "");
                      setEditingTitle(true);
                    }}
                  >
                    Rename
                  </button>
                </div>
              )}
            </div>
            <div className="conversation-header-actions">
              <div className="mobile-settings-actions">
                {hasNativeHost() && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => {
                      setSettingsPanel(null);
                      setPrimaryView("documents");
                    }}
                  >
                    Documents
                  </button>
                )}
                {hasNativeHost() && (
                  <button
                    type="button"
                    className={`btn${settingsPanel === "folders" ? " is-active" : ""}`}
                    onClick={() =>
                      setSettingsPanel((panel) =>
                        panel === "folders" ? null : "folders",
                      )
                    }
                  >
                    Folders
                  </button>
                )}
                <button
                  type="button"
                  className={`btn${settingsPanel === "providers" ? " is-active" : ""}`}
                  onClick={() =>
                    setSettingsPanel((panel) =>
                      panel === "providers" ? null : "providers",
                    )
                  }
                >
                  Providers
                </button>
                <button
                  type="button"
                  className={`btn${settingsPanel === "web-search" ? " is-active" : ""}`}
                  onClick={() =>
                    setSettingsPanel((panel) =>
                      panel === "web-search" ? null : "web-search",
                    )
                  }
                >
                  Web search
                </button>
              </div>
              <span className="status" title={status}>
                {status}
              </span>
            </div>
          </header>

          <div className="chat-meta">
            <label>
              Model{" "}
              <select
                value={chat.model ?? ""}
                disabled={deletingChatId !== null}
                onChange={(e) => void onModelChange(e.target.value)}
              >
                <option value="">default</option>
                {models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id} ({m.provider})
                  </option>
                ))}
                {chat.model && !models.some((m) => m.id === chat.model) && (
                  <option value={chat.model}>{chat.model} (custom)</option>
                )}
              </select>
            </label>
            <input
              className="model-custom"
              type="text"
              placeholder="or type a model id"
              defaultValue={
                chat.model && !models.some((m) => m.id === chat.model)
                  ? chat.model
                  : ""
              }
              key={chat.id}
              onBlur={(e) => {
                const next = e.target.value.trim();
                if (next && next !== (chat.model ?? "")) {
                  void onModelChange(next);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
            <AgentActivityPanel
              runs={visibleAgentRuns}
              loading={agentRunsChatId === chat.id ? agentRunsLoading : true}
              error={agentRunsChatId === chat.id ? agentRunsError : null}
              onRetry={() => refreshAgentRunsRef.current?.()}
              stoppingRunIds={visibleStoppingSandboxRunIds}
              stopErrorRunIds={visibleSandboxStopErrorRunIds}
              onStop={(runId) => void onStopSandboxAgentRun(runId)}
            />
          </div>

          <div className="message-view">
            <MessageList
              key={chat.id}
              messages={messages}
              folderAccessRequests={folderAccessRequests}
              nativeHost={hasNativeHost()}
              nativeBusy={resolvingFolderCalls.size > 0}
              resolvingFolderCalls={resolvingFolderCalls}
              folderAccessErrors={folderAccessErrors}
              busy={busy}
              scrollRef={scrollRef}
              onScroll={(event) => {
                const followsLatest = isNearBottom(event.currentTarget);
                followsLatestRef.current = followsLatest;
                if (followsLatest) setHasUnreadActivity(false);
              }}
              onApproval={(callId, decision) => void onApproval(callId, decision)}
              onFolderAccessDecision={(callId, decision) =>
                void onFolderAccessDecision(callId, decision)
              }
              onFolderAccessCancel={(callId, turnId) =>
                void onFolderAccessCancel(callId, turnId)
              }
            />
            {hasUnreadActivity && (
              <button
                type="button"
                className="new-activity"
                onClick={() => {
                  followsLatestRef.current = true;
                  setHasUnreadActivity(false);
                  if (scrollRef.current) scrollToLatest(scrollRef.current);
                }}
              >
                New activity ↓
              </button>
            )}
          </div>

          <Composer
            activeTurnId={activeTurnId}
            busy={busy}
            cancelError={cancelError}
            cancelPending={
              activeTurnId !== null && cancelPendingTurnId === activeTurnId
            }
            disabled={deletingChatId !== null}
            draft={draft}
            onDraftChange={onComposerDraftChange}
            onSend={onSend}
            onSteer={onSteerActiveTurn}
            onStop={onCancelActiveTurn}
            resetKey={chat?.id ?? "no-chat"}
            steerError={steerError}
            steerPending={
              activeTurnId !== null && steerPendingTurnId === activeTurnId
            }
            steerStatus={steerStatus}
          />
        </section>

        {settingsPanel === "providers" && (
          <ProvidersPanel
            providers={providers}
            client={client}
            onChanged={() => void refreshCatalog()}
          />
        )}
        {settingsPanel === "web-search" && <WebSearchPanel client={client} />}
        {settingsPanel === "folders" && <FoldersPanel chat={chat} />}
          </>
        )}
      </div>
    </div>
  );
}

function upsertToolCall(
  messages: Msg[],
  callId: string,
  name: string,
  status: ToolCallStatus,
): Msg[] {
  const existing = messages.findIndex(
    (message) => message.role === "tool" && message.callId === callId,
  );
  if (existing >= 0) {
    return messages.map((message, index) =>
      index === existing && message.role === "tool" ? { ...message, status } : message,
    );
  }
  return [...messages, { id: nextId(), role: "tool", callId, name, status }];
}

function updateToolCall(
  messages: Msg[],
  callId: string,
  update: (tool: Extract<Msg, { role: "tool" }>) => Extract<Msg, { role: "tool" }>,
): Msg[] {
  return messages.map((message) =>
    message.role === "tool" && message.callId === callId ? update(message) : message,
  );
}

function updateApprovalAndToolCall(
  messages: Msg[],
  callId: string,
  approved: boolean,
): Msg[] {
  return messages.map((message) => {
    if (message.role === "approval" && message.callId === callId) {
      return { ...message, resolved: true };
    }
    if (message.role === "tool" && message.callId === callId) {
      return {
        ...message,
        status: approved ? "running" : "cancelled",
      };
    }
    return message;
  });
}

function settleActiveToolCalls(
  messages: Msg[],
  status: Extract<ToolCallStatus, "failed" | "cancelled">,
): Msg[] {
  const activeCallIds = new Set(
    messages.flatMap((message) =>
      message.role === "tool" &&
      (message.status === "running" || message.status === "waiting_approval")
        ? [message.callId]
        : [],
    ),
  );
  return messages.map((message) =>
    message.role === "tool" &&
    (message.status === "running" || message.status === "waiting_approval")
      ? { ...message, status }
      : message.role === "approval" &&
          !message.resolved &&
          activeCallIds.has(message.callId)
        ? { ...message, resolved: true }
      : message,
  );
}

function discardToolCalls(messages: Msg[], callIds: Set<string>): Msg[] {
  return messages.filter(
    (message) => message.role !== "tool" || !callIds.has(message.callId),
  );
}

function withoutConnectionState(status: string): string {
  return status.replace(/ · (?:live|reconnecting)$/, "");
}

function FoldersPanel({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scopeLabel = chat.project_id ? "project" : "chat";

  async function refresh() {
    setError(null);
    try {
      setFolders(await listConnectedFolders(chat));
    } catch (err) {
      setError(String(err));
    }
  }

  useEffect(() => {
    void refresh();
  }, [chat.id, chat.project_id]);

  async function addFolder() {
    setWorking(true);
    setError(null);
    try {
      const connected = await connectFolder(chat);
      if (connected) await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function removeFolder(rootId: string) {
    setWorking(true);
    setError(null);
    try {
      await disconnectFolder(chat, rootId);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  return (
    <aside className="settings">
      <h2>Connected folders</h2>
      <p>
        OpenWave can read only folders you choose for this {scopeLabel}. Folder
        locations stay with the native host.
      </p>
      <button
        type="button"
        className="btn btn-primary"
        disabled={working}
        onClick={() => void addFolder()}
      >
        Choose folder…
      </button>
      <div className="folder-list">
        {folders.length === 0 && !error && (
          <div className="status">
            No folders connected to this {scopeLabel}.
          </div>
        )}
        {folders.map((folder) => (
          <div className="folder" key={folder.rootId}>
            <div>
              <strong>{folder.displayName}</strong>
              <div className="status">read access</div>
            </div>
            <button
              type="button"
              className="btn"
              disabled={working}
              onClick={() => void removeFolder(folder.rootId)}
            >
              Disconnect from {scopeLabel}
            </button>
          </div>
        ))}
      </div>
      {error && <div className="folder-error">{error}</div>}
    </aside>
  );
}

const MIN_WEB_SEARCH_TIMEOUT_MS = 1_000;
const MAX_WEB_SEARCH_TIMEOUT_MS = 60_000;

function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [savingConfig, setSavingConfig] = useState(false);
  const [savingCredential, setSavingCredential] = useState(false);
  const [removingCredential, setRemovingCredential] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    const [nextConfig, nextCredentials] = await Promise.all([
      client.getWebSearchConfig(),
      client.listWebSearchCredentials(),
    ]);
    setConfig(nextConfig);
    setCredentials(nextCredentials.credentials);
    setProvider(nextConfig.provider ?? "");
    setTimeoutMs(String(nextConfig.timeout_ms));
  }

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const [nextConfig, nextCredentials] = await Promise.all([
          client.getWebSearchConfig(),
          client.listWebSearchCredentials(),
        ]);
        if (cancelled) return;
        setConfig(nextConfig);
        setCredentials(nextCredentials.credentials);
        setProvider(nextConfig.provider ?? "");
        setTimeoutMs(String(nextConfig.timeout_ms));
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  const activeProvider = config?.provider;
  const selectedCredential = activeProvider
    ? credentials.find((credential) => credential.provider === activeProvider)
    : undefined;
  const selectedHasCredential = selectedCredential?.has_credential ?? false;
  const working = savingConfig || savingCredential || removingCredential;
  const state = webSearchState(config);

  async function saveConfig() {
    const parsedTimeout = Number(timeoutMs);
    if (
      !Number.isInteger(parsedTimeout) ||
      parsedTimeout < MIN_WEB_SEARCH_TIMEOUT_MS ||
      parsedTimeout > MAX_WEB_SEARCH_TIMEOUT_MS
    ) {
      setError(
        `Timeout must be a whole number between ${MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and ${MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.`,
      );
      return;
    }

    setSavingConfig(true);
    setError(null);
    try {
      const nextConfig = await client.putWebSearchConfig({
        provider: provider || null,
        timeout_ms: parsedTimeout,
      });
      setConfig(nextConfig);
      setTimeoutMs(String(nextConfig.timeout_ms));
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingConfig(false);
    }
  }

  async function saveCredential() {
    if (!activeProvider || !apiKey.trim()) return;
    setSavingCredential(true);
    setError(null);
    try {
      await client.putWebSearchCredential(activeProvider, apiKey.trim());
      setApiKey("");
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingCredential(false);
    }
  }

  async function removeCredential() {
    if (!activeProvider) return;
    setRemovingCredential(true);
    setError(null);
    try {
      await client.deleteWebSearchCredential(activeProvider);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingCredential(false);
    }
  }

  return (
    <aside className="settings web-search-settings" aria-busy={loading}>
      <h2>Web search</h2>
      <p>
        Choose a local provider and a bounded request timeout. Existing keys are
        never shown here.
      </p>

      {loading ? (
        <div className="status">Loading web-search settings…</div>
      ) : !config ? (
        <div className="status">Web-search settings are unavailable.</div>
      ) : (
        <>
          <div className={`web-search-state is-${state.kind}`} role="status">
            <strong>{state.label}</strong>
            <span>{state.description}</span>
          </div>

          <div className="provider">
            <label className="settings-field">
              <span>Provider</span>
              <select
                value={provider}
                disabled={working}
                onChange={(event) => setProvider(event.target.value as WebSearchProviderKind | "")}
              >
                <option value="">Disabled</option>
                <option value="exa">Exa</option>
                <option value="tavily">Tavily</option>
              </select>
            </label>

            <label className="settings-field">
              <span>Request timeout (ms)</span>
              <input
                type="number"
                inputMode="numeric"
                min={MIN_WEB_SEARCH_TIMEOUT_MS}
                max={MAX_WEB_SEARCH_TIMEOUT_MS}
                step="1000"
                value={timeoutMs}
                disabled={working}
                onChange={(event) => setTimeoutMs(event.target.value)}
              />
            </label>
            <p className="settings-hint">
              Between {MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and {MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.
            </p>
            <div className="row">
              <button
                type="button"
                className="btn btn-primary"
                disabled={working}
                onClick={() => void saveConfig()}
              >
                {savingConfig ? "Saving…" : "Save configuration"}
              </button>
            </div>
          </div>

          {activeProvider && (
            <div className="provider">
              <h3>{activeProvider} credential</h3>
              <span className="status">
                {selectedHasCredential ? "credential saved" : "no credential saved"}
              </span>
              <label className="settings-field">
                <span>{selectedHasCredential ? "Replace API key" : "API key"}</span>
                <input
                  type="password"
                  placeholder="Paste a new API key"
                  value={apiKey}
                  maxLength={8_192}
                  autoComplete="new-password"
                  disabled={working}
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </label>
              <div className="row">
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={working || !apiKey.trim()}
                  onClick={() => void saveCredential()}
                >
                  {savingCredential ? "Saving…" : selectedHasCredential ? "Update key" : "Save key"}
                </button>
                {selectedHasCredential && (
                  <button
                    type="button"
                    className="btn btn-danger"
                    disabled={working}
                    onClick={() => void removeCredential()}
                  >
                    {removingCredential ? "Removing…" : "Remove saved key"}
                  </button>
                )}
              </div>
            </div>
          )}

          {provider !== (activeProvider ?? "") && (
            <p className="settings-hint">
              Save the provider configuration before managing that provider’s key.
            </p>
          )}

          <p className="settings-note">
            This configures host-owned search access only. Search is not yet a
            chat tool in this build.
          </p>
        </>
      )}
      {error && <div className="folder-error" role="alert">{error}</div>}
    </aside>
  );
}

function webSearchState(config: WebSearchConfigInfo | null): {
  kind: "disabled" | "ready" | "not-configured";
  label: string;
  description: string;
} {
  if (!config?.provider) {
    return {
      kind: "disabled",
      label: "Disabled",
      description: "No web-search provider is selected.",
    };
  }
  if (config.has_credential) {
    return {
      kind: "ready",
      label: "Ready",
      description: `${config.provider} is selected and has a saved credential.`,
    };
  }
  return {
    kind: "not-configured",
    label: "Not configured",
    description: `${config.provider} is selected but needs an API key.`,
  };
}

function ProvidersPanel({
  providers,
  client,
  onChanged,
}: {
  providers: ProviderInfo[];
  client: ApiClient;
  onChanged: () => void;
}) {
  return (
    <aside className="settings">
      <h2>Providers</h2>
      <p>Keys stay on this machine. Enable a provider, then save a credential.</p>
      {providers.map((p) => (
        <ProviderRow key={p.kind} info={p} client={client} onChanged={onChanged} />
      ))}
    </aside>
  );
}

function ProviderRow({
  info,
  client,
  onChanged,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
      } = { enabled };
      if (info.kind === "openai_compatible") {
        body.base_url = baseUrl.trim() || null;
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function clearCredential() {
    setSaving(true);
    setError(null);
    try {
      await client.deleteCredential(info.kind as ProviderKind);
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="provider">
      <h3>{info.kind.replaceAll("_", " ")}</h3>
      <div className="row">
        <label>
          <input
            type="checkbox"
            checked={info.enabled}
            disabled={saving}
            onChange={(e) => void save(e.target.checked)}
          />{" "}
          enabled
        </label>
        <span className="status">
          {info.has_credential ? "credential set" : "no credential"}
        </span>
      </div>
      {info.kind === "openai_compatible" && (
        <div className="row">
          <input
            type="text"
            placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
        </div>
      )}
      <div className="row">
        <input
          type="password"
          placeholder="API key"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          autoComplete="off"
        />
        <button
          type="button"
          className="btn btn-primary"
          disabled={saving || !key.trim()}
          onClick={() => void save(true)}
        >
          Save
        </button>
        {info.has_credential && (
          <button
            type="button"
            className="btn"
            disabled={saving}
            onClick={() => void clearCredential()}
          >
            Clear
          </button>
        )}
      </div>
      {error && <div className="status">{error}</div>}
    </div>
  );
}
