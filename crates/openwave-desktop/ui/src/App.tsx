import { useEffect, useRef, useState } from "react";
import {
  ApiClient,
  type AgentRun,
  type Chat,
  type ModelInfo,
  type PendingFolderAccessRequest,
  type ProviderInfo,
  type ReasoningEffort,
  type SequencedEvent,
  type ServerInfo,
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
  ArrowDown,
  Ellipsis,
  FolderOpen,
  LibraryBig,
  Monitor,
  Moon,
  Pencil,
  Settings,
  SquarePen,
  Sun,
  Trash2,
} from "lucide-react";
import { useTheme } from "./theme";
import { SettingsView } from "./SettingsView";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { SettingsError, SettingsPanel } from "./settings/primitives";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
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
import { shouldRefreshAgentRunsAfterToolEvent } from "./AgentRunRefresh";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
  sandboxAgentStopKey,
} from "./SandboxAgentStop";
import { prependReplacementChat } from "./ChatDeletion";
import { useConfirm } from "./components/ConfirmDialog";
import { Button } from "@/components/ui/button";

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
  const [decidingApprovalCalls, setDecidingApprovalCalls] = useState<
    Set<string>
  >(new Set());
  const [approvalErrors, setApprovalErrors] = useState<Record<string, string>>(
    {},
  );
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
  const [savingTitle, setSavingTitle] = useState(false);
  const [renamingChatId, setRenamingChatId] = useState<string | null>(null);
  const [renameChatDraft, setRenameChatDraft] = useState("");
  const skipRenameCommitRef = useRef(false);
  const [settingsPanel, setSettingsPanel] = useState<"folders" | null>(null);
  const [primaryView, setPrimaryView] = useState<
    "chat" | "documents" | "settings"
  >("chat");
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
  const decidingApprovalCallsRef = useRef<Set<string>>(new Set());
  const visibleFolderCallIdsRef = useRef<Set<string>>(new Set());
  const cancelRequestTurnRef = useRef<string | null>(null);
  const activeTurnIdRef = useRef<string | null>(null);
  const draftRef = useRef("");
  const selectedChatIdRef = useRef<string | null>(null);
  const steerFenceRef = useRef(new ActiveTurnSteerFence());
  const sandboxStopFenceRef = useRef(new SandboxAgentStopFence());
  const provisionalToolCallIdsRef = useRef<Set<string>>(new Set());
  const creationInFlightRef = useRef(false);
  const deletionInFlightRef = useRef(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const { mode: themeMode, cycle: cycleTheme, setMode: setThemeMode } = useTheme();

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
              text: `Could not load this chat: ${String(err)}`,
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

    if (shouldRefreshAgentRunsAfterToolEvent(event)) {
      refreshAgentRunsRef.current?.();
    }

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
    if (!client || creationInFlightRef.current || deletionInFlightRef.current) return;
    creationInFlightRef.current = true;
    setCreatingChat(true);
    setPrimaryView("chat");
    try {
      const created = await client.createChat(
        chat?.model ?? models[0]?.id,
        null,
      );
      openCreatedChat(created);
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
      creationInFlightRef.current = false;
      setCreatingChat(false);
    }
  }

  function openCreatedChat(created: Chat) {
    setPrimaryView("chat");
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
    decidingApprovalCallsRef.current = new Set();
    setDecidingApprovalCalls(new Set());
    setApprovalErrors({});
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
  }

  async function onDeleteChat(target: Chat) {
    if (!client || deletionInFlightRef.current || creationInFlightRef.current) return;
    const label = target.title?.trim() || "this chat";
    const confirmed = await confirm({
      title: `Delete ${label}?`,
      description: "This cannot be undone.",
      confirmLabel: "Delete chat",
      destructive: true,
    });
    if (!confirmed) return;

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

      let next: Chat | undefined = refreshed[0];
      if (!next) {
        next = await client.createChat(models[0]?.id, null);
        refreshed = prependReplacementChat(refreshed, next);
      }
      setChats(refreshed);
      selectChat(next, true);
    } catch (err) {
      setChatsError(`Could not delete chat: ${String(err)}`);
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
    if (
      next.id === chat?.id ||
      creatingChat ||
      (!force && (creationInFlightRef.current || deletionInFlightRef.current))
    ) {
      return;
    }
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
    decidingApprovalCallsRef.current = new Set();
    setDecidingApprovalCalls(new Set());
    setApprovalErrors({});
    setComposerDraft("");
    setBusy(false);
    setCurrentActiveTurnId(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    clearSteerRequestState();
    cancelChatRename();
    activateChat(next);
    setStatus(`chat ${next.id.slice(0, 8)}…`);
  }

  function startChatRename(target: Chat) {
    skipRenameCommitRef.current = false;
    setRenameChatDraft(target.title ?? "");
    setRenamingChatId(target.id);
  }

  function cancelChatRename() {
    skipRenameCommitRef.current = true;
    setRenamingChatId(null);
    setRenameChatDraft("");
  }

  async function commitChatRename(target: Chat) {
    // A single rename resolves through the input's blur; Enter blurs the field
    // and Escape sets the skip flag before blurring, so the blur that follows
    // must not also patch.
    if (skipRenameCommitRef.current) {
      skipRenameCommitRef.current = false;
      return;
    }
    if (!client || savingTitle || deletionInFlightRef.current) return;
    const trimmed = renameChatDraft.trim();
    if (trimmed === (target.title?.trim() ?? "")) {
      setRenamingChatId(null);
      setRenameChatDraft("");
      return;
    }
    const selection = chatSelectionRef.current;
    setSavingTitle(true);
    try {
      const updated = await client.patchChatTitle(target.id, trimmed || null);
      setChats((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      if (chat?.id === updated.id && chatSelectionRef.current === selection) {
        setChat(updated);
      }
      setRenamingChatId(null);
      setRenameChatDraft("");
    } catch (err) {
      // If the user has since switched conversations, abandon this stale edit
      // silently. Otherwise keep the editor open with the typed draft so the
      // rename can be retried instead of being discarded.
      if (chatSelectionRef.current === selection) {
        setChatsError(`Could not rename chat: ${String(err)}`);
      } else {
        setRenamingChatId(null);
        setRenameChatDraft("");
      }
    } finally {
      setSavingTitle(false);
    }
  }

  async function onModelChange(modelId: string | null) {
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

  async function onReasoningEffortChange(effort: ReasoningEffort | null) {
    if (!client || !chat || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const updated = await client.patchChatReasoningEffort(chatId, effort);
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

  async function onApproval(
    callId: string,
    decision: "approve" | "reject",
    remember = false,
  ) {
    if (!client || !chat) return;
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
      setMessages((prev) =>
        prev.map((m) =>
          m.role === "approval" && m.callId === callId
            ? { ...m, resolved: true }
            : m,
        ),
      );
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

  const visibleChats = chats;

  return (
    <div className="app-shell">
      {confirmDialog}
      <aside className="sidebar">
        <div className="sidebar-brand">
          <Logomark />
          <span>OpenWave</span>
          <WithTooltip
            label={`Theme: ${themeMode} — click to change`}
            side="bottom"
          >
            <button
              type="button"
              className="theme-toggle"
              aria-label={`Theme: ${themeMode}. Click to change.`}
              onClick={cycleTheme}
            >
              {themeMode === "light" ? (
                <Sun size={15} />
              ) : themeMode === "dark" ? (
                <Moon size={15} />
              ) : (
                <Monitor size={15} />
              )}
            </button>
          </WithTooltip>
        </div>

        <button
          type="button"
          className="new-chat"
          onClick={() => void onNewChat()}
          disabled={creatingChat || deletingChatId !== null}
        >
          <SquarePen size={15} />
          {creatingChat
            ? "Starting…"
            : deletingChatId
              ? "Deleting…"
              : "New chat"}
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
            <LibraryBig size={16} />
            Documents
          </button>
        )}

        <div className="sidebar-section">
          <span className="sidebar-label">Chats</span>
          <div className="conversation-list" aria-label="Chats">
            {visibleChats.map((item) => {
              const chatTitle = item.title?.trim() || "New chat";
              const isActive = primaryView === "chat" && item.id === chat.id;
              const mutating = deletingChatId !== null || creatingChat;

              if (renamingChatId === item.id) {
                return (
                  <div
                    key={item.id}
                    className="conversation-row is-renaming"
                  >
                    <input
                      className="conversation-rename-input"
                      autoFocus
                      aria-label="Chat title"
                      value={renameChatDraft}
                      disabled={savingTitle}
                      onChange={(event) =>
                        setRenameChatDraft(event.target.value)
                      }
                      onBlur={() => void commitChatRename(item)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          event.currentTarget.blur();
                        }
                        if (event.key === "Escape") {
                          event.preventDefault();
                          cancelChatRename();
                        }
                      }}
                    />
                  </div>
                );
              }

              return (
                <div
                  key={item.id}
                  className={`conversation-row${isActive ? " is-active" : ""}`}
                >
                  <button
                    type="button"
                    className="conversation-item"
                    aria-current={isActive ? "page" : undefined}
                    disabled={mutating}
                    onClick={() => selectChat(item)}
                  >
                    <span className="conversation-title">{chatTitle}</span>
                  </button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <button
                        type="button"
                        className="conversation-menu"
                        aria-label={`Actions for ${chatTitle}`}
                        title="Chat actions"
                        disabled={mutating}
                      >
                        <Ellipsis size={15} />
                      </button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end" side="right">
                      <DropdownMenuItem
                        onSelect={() => startChatRename(item)}
                      >
                        <Pencil />
                        Rename
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        variant="destructive"
                        onSelect={() => void onDeleteChat(item)}
                      >
                        <Trash2 />
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              );
            })}
          </div>
          {chatsError && <p className="sidebar-error">{chatsError}</p>}
        </div>

        <div className="sidebar-footer">
          {hasNativeHost() && (
            <button
              type="button"
              className={`sidebar-action${primaryView === "chat" && settingsPanel === "folders" ? " is-active" : ""}`}
              onClick={() => {
                setPrimaryView("chat");
                setSettingsPanel((panel) =>
                  panel === "folders" ? null : "folders",
                );
              }}
            >
              <FolderOpen size={16} />
              Folders
            </button>
          )}
          <button
            type="button"
            className={`sidebar-action${primaryView === "settings" ? " is-active" : ""}`}
            onClick={() => {
              setSettingsPanel(null);
              setPrimaryView("settings");
            }}
          >
            <Settings size={16} />
            Settings
          </button>
        </div>
      </aside>

      <div
        className={`main${primaryView === "chat" && settingsPanel ? " with-settings" : ""}`}
      >
        {primaryView === "documents" ? (
          <DocumentsView chatId={chat.id} onBack={() => setPrimaryView("chat")} />
        ) : primaryView === "settings" ? (
          <SettingsView
            client={client}
            models={models}
            providers={providers}
            onProvidersChanged={() => void refreshCatalog()}
            onBack={() => setPrimaryView("chat")}
            themeMode={themeMode}
            onThemeChange={setThemeMode}
          />
        ) : (
          <>
        <section className="chat-pane">
          <header className="conversation-header">
            <div className="conversation-title-row">
              <h1>{chat.title?.trim() || "New chat"}</h1>
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
                    <LibraryBig size={14} />
                  </button>
                )}
                {hasNativeHost() && (
                  <button
                    type="button"
                    className={`btn${settingsPanel === "folders" ? " is-active" : ""}`}
                    aria-label="Folders"
                    onClick={() =>
                      setSettingsPanel((panel) =>
                        panel === "folders" ? null : "folders",
                      )
                    }
                  >
                    <FolderOpen size={14} />
                  </button>
                )}
                <button
                  type="button"
                  className="btn"
                  aria-label="Settings"
                  onClick={() => {
                    setSettingsPanel(null);
                    setPrimaryView("settings");
                  }}
                >
                  <Settings size={14} />
                </button>
              </div>
              <span className="status" title={status}>
                {status}
              </span>
            </div>
          </header>

          <AgentActivityPanel
            runs={visibleAgentRuns}
            loading={agentRunsChatId === chat.id ? agentRunsLoading : true}
            error={agentRunsChatId === chat.id ? agentRunsError : null}
            onRetry={() => refreshAgentRunsRef.current?.()}
            stoppingRunIds={visibleStoppingSandboxRunIds}
            stopErrorRunIds={visibleSandboxStopErrorRunIds}
            onStop={(runId) => void onStopSandboxAgentRun(runId)}
          />

          <div className="message-view">
            <MessageList
              key={chat.id}
              messages={messages}
              folderAccessRequests={folderAccessRequests}
              nativeHost={hasNativeHost()}
              nativeBusy={resolvingFolderCalls.size > 0}
              resolvingFolderCalls={resolvingFolderCalls}
              folderAccessErrors={folderAccessErrors}
              decidingApprovalCalls={decidingApprovalCalls}
              approvalErrors={approvalErrors}
              busy={busy}
              scrollRef={scrollRef}
              onScroll={(event) => {
                const followsLatest = isNearBottom(event.currentTarget);
                followsLatestRef.current = followsLatest;
                if (followsLatest) setHasUnreadActivity(false);
              }}
              onApproval={(callId, decision, remember) =>
                void onApproval(callId, decision, remember)
              }
              onFolderAccessDecision={(callId, decision) =>
                void onFolderAccessDecision(callId, decision)
              }
              onFolderAccessCancel={(callId, turnId) =>
                void onFolderAccessCancel(callId, turnId)
              }
              onSelectPrompt={setComposerDraft}
              hydrated={hydratedChatId === chat.id}
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
                New activity
                <ArrowDown size={13} />
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
            modelMenu={
              <>
                <ModelMenu
                  models={models}
                  value={chat.model}
                  disabled={deletingChatId !== null}
                  onChange={onModelChange}
                />
                {models.find((model) => model.id === chat.model)
                  ?.supports_reasoning_effort && (
                  <ReasoningEffortMenu
                    value={chat.reasoning_effort}
                    disabled={deletingChatId !== null}
                    onChange={onReasoningEffortChange}
                  />
                )}
              </>
            }
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

        {settingsPanel === "folders" && (
          <aside className="settings">
            <FoldersPanel chat={chat} />
          </aside>
        )}
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
  const scopeLabel = "chat";

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
    <SettingsPanel
      title="Connected folders"
      description={`OpenWave can read only folders you choose for this ${scopeLabel}. Folder locations stay with the native host.`}
    >
      <Button
        type="button"
        variant="outline"
        className="self-start"
        disabled={working}
        onClick={() => void addFolder()}
      >
        Choose folder…
      </Button>
      <div className="flex flex-col gap-2">
        {folders.length === 0 && !error && (
          <p className="text-sm text-muted-foreground">
            No folders connected to this {scopeLabel}.
          </p>
        )}
        {folders.map((folder) => (
          <div
            className="flex items-center justify-between gap-3 rounded-lg border border-border p-3"
            key={folder.rootId}
          >
            <div className="min-w-0">
              <strong className="block truncate text-sm font-medium">
                {folder.displayName}
              </strong>
              <span className="text-xs text-muted-foreground">read access</span>
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => void removeFolder(folder.rootId)}
            >
              Disconnect
            </Button>
          </div>
        ))}
      </div>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
