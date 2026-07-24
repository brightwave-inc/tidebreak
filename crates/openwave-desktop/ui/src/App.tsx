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
  hasNativeHost,
  resolveFolderAccessRequest,
  type FolderAccessDecision,
} from "./host";
import { Logomark } from "./Logomark";
import { useTheme } from "./theme";
import { SettingsView } from "./SettingsView";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { ChatSessionController } from "./ChatSessionController";
import { useChatSessionStore } from "./ChatSessionStore";
import { useChatListStore } from "./ChatListStore";
import { useUiStore } from "./UiStore";
import { DocumentsView } from "./DocumentsView";
import { DeliverablesView } from "./DeliverablesView";
import {
  importLibraryDocument,
  type ImportedDocument,
} from "./documents";
import { reconcilePendingApprovalCards } from "./ApprovalHistory";
import { loadChatApprovalHydration } from "./ChatApprovalHydration";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import {
  applyTerminalHydration,
  type ChatSessionEffect,
  type ChatSessionState,
} from "./ChatSessionReducer";
import {
  ActiveTurnSteerFence,
  canBeginActiveTurnSteer,
  shouldClearAcceptedSteerDraft,
} from "./ActiveTurnSteer";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { agentRunsForChat } from "./AgentActivityPanel";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
  sandboxAgentStopKey,
} from "./SandboxAgentStop";
import { prependReplacementChat } from "./ChatDeletion";
import { useConfirm } from "./components/ConfirmDialog";
import { useDesktopUpdates } from "./updates";
import { ChatView } from "./ChatView";
import { FoldersView } from "./FoldersView";
import { Sidebar } from "./Sidebar";

let msgSeq = 0;

function nextId(): string {
  msgSeq += 1;
  return `m${msgSeq}`;
}

const sessionDeps = {
  nextId,
  now: () => new Date().toISOString(),
};

// Store actions are stable for the store's lifetime; these handles are for
// calling actions only — never read state fields from them.
const chatListActions = useChatListStore.getState();
const uiActions = useUiStore.getState();

export default function App() {
  const [bootError, setBootError] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [hydratedChatId, setHydratedChatId] = useState<string | null>(null);
  const chat = useChatListStore((state) => state.selected);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const primaryView = useUiStore((state) => state.primaryView);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
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
  const [addingSourceChatId, setAddingSourceChatId] = useState<string | null>(
    null,
  );
  const [recentSource, setRecentSource] = useState<{
    chatId: string;
    source: ImportedDocument;
  } | null>(null);
  const [sourceAttachmentError, setSourceAttachmentError] = useState<{
    chatId: string;
    message: string;
  } | null>(null);
  const [cancelPendingTurnId, setCancelPendingTurnId] = useState<string | null>(
    null,
  );
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [steerPendingTurnId, setSteerPendingTurnId] = useState<string | null>(
    null,
  );
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const skipRenameCommitRef = useRef(false);
  const [status, setStatus] = useState("starting…");
  // Owns the selected chat's event socket; chat switches dispose it eagerly
  // and the connection effect below constructs a fresh one.
  const controllerRef = useRef<ChatSessionController | null>(null);
  const handleEventRef = useRef<(event: SequencedEvent) => void>(() => {});
  const chatSelectionRef = useRef(0);
  const terminalHydrationGenerationRef = useRef(0);
  const refreshFolderAccessRef = useRef<(() => void) | null>(null);
  const refreshAgentRunsRef = useRef<(() => void) | null>(null);
  const resolvingFolderCallsRef = useRef<Set<string>>(new Set());
  const decidingApprovalCallsRef = useRef<Set<string>>(new Set());
  const cancelRequestTurnRef = useRef<string | null>(null);
  const draftRef = useRef("");
  const selectedChatIdRef = useRef<string | null>(null);
  const steerFenceRef = useRef(new ActiveTurnSteerFence());
  const sandboxStopFenceRef = useRef(new SandboxAgentStopFence());
  const creationInFlightRef = useRef(false);
  const deletionInFlightRef = useRef(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const { mode: themeMode, cycle: cycleTheme, setMode: setThemeMode } = useTheme();
  const desktopUpdates = useDesktopUpdates();

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
        chatListActions.setChats(existingChats);
        const created =
          existingChats[0] ??
          (await client.createChat(catalog.models[0]?.id));
        if (cancelled) return;
        if (existingChats.length === 0) chatListActions.setChats([created]);
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
    updateSession((session) => ({
      ...session,
      messages: [],
      hydratedMessageIds: new Set(),
    }));
    (async () => {
      try {
        const hydration = await loadChatApprovalHydration(
          client,
          chat.id,
          () => !cancelled && selection === chatSelectionRef.current,
        );
        if (!hydration) return;
        const { transcript, pendingApprovals } = hydration;
        const presented = presentChatTranscript(transcript);
        const pendingTurnId = pendingApprovals[0]?.turnId ?? null;
        updateSession((session) => ({
          ...session,
          lastSeq: transcript.last_event_seq,
          hydratedMessageIds: presented.messageIds,
          messages: reconcilePendingApprovalCards(
            presented.messages,
            pendingApprovals,
          ),
          activeTurnId: pendingTurnId,
          busy: pendingTurnId !== null,
        }));
        setHydratedChatId(chat.id);
      } catch (err) {
        if (!cancelled && selection === chatSelectionRef.current) {
          updateSession((session) => ({
            ...session,
            busy: true,
            messages: [
              {
                id: nextId(),
                role: "error",
                text: `Could not load this chat: ${String(err)}`,
              },
            ],
          }));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, chat?.id]);

  useEffect(() => {
    if (!client || !chat || hydratedChatId !== chat.id) return;
    const chatId = chat.id;
    const controller = new ChatSessionController({
      openSocket: (after, onEvent) => client.openEvents(chatId, after, onEvent),
      getAfter: () => useChatSessionStore.getState().lastSeq,
      onEvent: (event) => handleEventRef.current(event),
      onConnectionState: (connectionState) =>
        setStatus(
          (current) => `${withoutConnectionState(current)} · ${connectionState}`,
        ),
    });
    controllerRef.current = controller;
    controller.start();
    return () => {
      controller.dispose();
      if (controllerRef.current === controller) controllerRef.current = null;
    };
  }, [client, chat?.id, hydratedChatId]);

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

  function updateSession(update: (state: ChatSessionState) => ChatSessionState) {
    useChatSessionStore.getState().update(update);
  }

  function handleEvent(framed: SequencedEvent) {
    const effects = useChatSessionStore
      .getState()
      .applyEvent(framed, sessionDeps);
    for (const effect of effects) applySessionEffect(effect);
  }
  handleEventRef.current = handleEvent;

  function applySessionEffect(effect: ChatSessionEffect) {
    switch (effect.type) {
      case "refresh_agent_runs":
        refreshAgentRunsRef.current?.();
        return;
      case "refresh_folder_access":
        refreshFolderAccessRef.current?.();
        return;
      case "turn_began":
        setCancelPendingTurnId(null);
        setCancelError(null);
        cancelRequestTurnRef.current = null;
        if (effect.startsDifferentTurn) clearSteerRequestState();
        return;
      case "turn_resolved":
        setCancelPendingTurnId(null);
        setCancelError(null);
        cancelRequestTurnRef.current = null;
        clearSteerRequestState();
        return;
      case "invalidate_terminal_hydration":
        terminalHydrationGenerationRef.current += 1;
        return;
      case "hydrate_terminal_transcript": {
        const selection = chatSelectionRef.current;
        const generation = ++terminalHydrationGenerationRef.current;
        if (chat) {
          void refreshTerminalTranscript(chat.id, selection, generation);
        }
        return;
      }
    }
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
      updateSession((session) => applyTerminalHydration(session, presented));
    } catch {
      // The scrubbed optimistic response remains safe and visible. Reopening
      // the conversation will load a fresh authoritative snapshot.
    }
  }

  function resolveActiveTurn() {
    updateSession((session) => ({
      ...session,
      busy: false,
      activeTurnId: null,
    }));
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    clearSteerRequestState();
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
    updateSession((session) => ({
      ...session,
      busy: true,
      activeTurnId: turnId,
      messages: [
        ...session.messages,
        {
          id: nextId(),
          role: "user",
          text: content,
          createdAt: new Date().toISOString(),
        },
      ],
    }));
    setCancelPendingTurnId(null);
    setCancelError(null);
    try {
      await client.postMessage(chatId, turnId, content);
      if (chatSelectionRef.current !== selection) return;
      setRecentSource((current) =>
        current?.chatId === chatId ? null : current,
      );
      refreshAgentRunsRef.current?.();
    } catch (err) {
      if (chatSelectionRef.current !== selection) return;
      resolveActiveTurn();
      updateSession((session) => ({
        ...session,
        messages: [
          ...session.messages,
          { id: nextId(), role: "error", text: String(err) },
        ],
      }));
    }
  }

  async function onAddSource() {
    if (!chat || addingSourceChatId !== null || deletionInFlightRef.current) return;
    const chatId = chat.id;
    setAddingSourceChatId(chatId);
    setSourceAttachmentError(null);
    try {
      const source = await importLibraryDocument(chatId);
      if (!source || selectedChatIdRef.current !== chatId) return;
      setRecentSource({ chatId, source });
    } catch (err) {
      if (selectedChatIdRef.current === chatId) {
        setSourceAttachmentError({
          chatId,
          message: friendlySourceAttachmentError(err),
        });
      }
    } finally {
      setAddingSourceChatId(null);
    }
  }

  async function onSteerActiveTurn() {
    const admission = {
      busy,
      turnId: useChatSessionStore.getState().activeTurnId,
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
          turnId: useChatSessionStore.getState().activeTurnId ?? "",
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
          turnId: useChatSessionStore.getState().activeTurnId ?? "",
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
    chatListActions.setCreatingChat(true);
    uiActions.showChat();
    try {
      const created = await client.createChat(
        chat?.model ?? models[0]?.id,
        null,
      );
      openCreatedChat(created);
    } catch (err) {
      updateSession((session) => ({
        ...session,
        messages: [
          ...session.messages,
          {
            id: nextId(),
            role: "error",
            text: `Could not create a chat: ${String(err)}`,
          },
        ],
      }));
    } finally {
      creationInFlightRef.current = false;
      chatListActions.setCreatingChat(false);
    }
  }

  function openCreatedChat(created: Chat) {
    uiActions.showChat();
    controllerRef.current?.dispose();
    controllerRef.current = null;
    useChatSessionStore.getState().reset();
    setAgentRuns([]);
    setAgentRunsError(null);
    setFolderAccessRequests([]);
    setFolderAccessErrors({});
    decidingApprovalCallsRef.current = new Set();
    setDecidingApprovalCalls(new Set());
    setApprovalErrors({});
    setComposerDraft("");
    setRecentSource(null);
    setSourceAttachmentError(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    clearSteerRequestState();
    activateChat(created);
    chatListActions.prependChat(created);
    chatListActions.setChatsError(null);
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
    chatListActions.setDeletingChatId(target.id);
    chatListActions.setChatsError(null);
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
        chatListActions.setChats(refreshed);
        return;
      }

      let next: Chat | undefined = refreshed[0];
      if (!next) {
        next = await client.createChat(models[0]?.id, null);
        refreshed = prependReplacementChat(refreshed, next);
      }
      chatListActions.setChats(refreshed);
      selectChat(next, true);
    } catch (err) {
      chatListActions.setChatsError(`Could not delete chat: ${String(err)}`);
    } finally {
      deletionInFlightRef.current = false;
      chatListActions.setDeletingChatId(null);
    }
  }

  function activateChat(next: Chat) {
    chatSelectionRef.current += 1;
    terminalHydrationGenerationRef.current += 1;
    selectedChatIdRef.current = next.id;
    sandboxStopFenceRef.current.invalidate();
    setStoppingSandboxRunKeys(new Set());
    setSandboxStopErrorKeys(new Set());
    updateSession((session) => ({
      ...session,
      markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    }));
    chatListActions.setSelected(next);
  }

  function selectChat(next: Chat, force = false) {
    uiActions.showChat();
    if (
      next.id === chat?.id ||
      creatingChat ||
      (!force && (creationInFlightRef.current || deletionInFlightRef.current))
    ) {
      return;
    }
    controllerRef.current?.dispose();
    controllerRef.current = null;
    useChatSessionStore.getState().reset();
    setAgentRuns([]);
    setAgentRunsError(null);
    setFolderAccessRequests([]);
    setFolderAccessErrors({});
    decidingApprovalCallsRef.current = new Set();
    setDecidingApprovalCalls(new Set());
    setApprovalErrors({});
    setComposerDraft("");
    setRecentSource(null);
    setSourceAttachmentError(null);
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
    chatListActions.beginRename(target);
  }

  function cancelChatRename() {
    skipRenameCommitRef.current = true;
    chatListActions.endRename();
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
      chatListActions.endRename();
      return;
    }
    const selection = chatSelectionRef.current;
    chatListActions.setSavingTitle(true);
    try {
      const updated = await client.patchChatTitle(target.id, trimmed || null);
      chatListActions.replaceChat(updated);
      chatListActions.endRename();
    } catch (err) {
      // If the user has since switched conversations, abandon this stale edit
      // silently. Otherwise keep the editor open with the typed draft so the
      // rename can be retried instead of being discarded.
      if (chatSelectionRef.current === selection) {
        chatListActions.setChatsError(`Could not rename chat: ${String(err)}`);
      } else {
        chatListActions.endRename();
      }
    } finally {
      chatListActions.setSavingTitle(false);
    }
  }

  async function onModelChange(modelId: string | null) {
    if (!client || !chat || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const updated = await client.patchChatModel(chatId, modelId || null);
    // replaceChat updates the list and, when ids match, the selection too;
    // after a selection change the ids differ, so this stays fence-safe.
    chatListActions.replaceChat(updated);
    void selection;
  }

  async function onReasoningEffortChange(effort: ReasoningEffort | null) {
    if (!client || !chat || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const updated = await client.patchChatReasoningEffort(chatId, effort);
    // replaceChat updates the list and, when ids match, the selection too;
    // after a selection change the ids differ, so this stays fence-safe.
    chatListActions.replaceChat(updated);
    void selection;
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
      updateSession((session) => ({
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

  async function onRestartForUpdate() {
    const version = desktopUpdates.state.version;
    const confirmed = await confirm({
      title: "Restart OpenWave to update?",
      description: `${version ? `Version ${version}` : "The update"} is ready. OpenWave will close and reopen. Wait for active work to finish before restarting.`,
      confirmLabel: "Restart and update",
    });
    if (confirmed) await desktopUpdates.restart();
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
      {confirmDialog}
      <Sidebar
        themeMode={themeMode}
        updateReady={desktopUpdates.state.status === "ready"}
        updateVersion={desktopUpdates.state.version ?? null}
        onCycleTheme={cycleTheme}
        onNewChat={() => void onNewChat()}
        onSelectChat={selectChat}
        onStartRename={startChatRename}
        onCommitRename={(target) => void commitChatRename(target)}
        onCancelRename={cancelChatRename}
        onDeleteChat={(target) => void onDeleteChat(target)}
        onRestartForUpdate={() => void onRestartForUpdate()}
      />

      <div className="main">
        {primaryView === "documents" ? (
          <DocumentsView chatId={chat.id} />
        ) : primaryView === "deliverables" ? (
          <DeliverablesView chatId={chat.id} />
        ) : primaryView === "folders" ? (
          <FoldersView chat={chat} />
        ) : primaryView === "settings" ? (
          <SettingsView
            client={client}
            models={models}
            providers={providers}
            onProvidersChanged={() => void refreshCatalog()}
            onBack={() => uiActions.showChat()}
            themeMode={themeMode}
            onThemeChange={setThemeMode}
            updateState={desktopUpdates.state}
            onCheckForUpdate={desktopUpdates.check}
            onRestartForUpdate={onRestartForUpdate}
          />
        ) : (
          <ChatView
          key={chat.id}
          chat={chat}
          status={status}
          hydrated={hydratedChatId === chat.id}
          nativeHost={hasNativeHost()}
          deletingChat={deletingChatId !== null}
          agentRuns={visibleAgentRuns}
          agentRunsLoading={
            agentRunsChatId === chat.id ? agentRunsLoading : true
          }
          agentRunsError={agentRunsChatId === chat.id ? agentRunsError : null}
          stoppingRunIds={visibleStoppingSandboxRunIds}
          stopErrorRunIds={visibleSandboxStopErrorRunIds}
          onRetryAgentRuns={() => refreshAgentRunsRef.current?.()}
          onStopSandboxRun={(runId) => void onStopSandboxAgentRun(runId)}
          folderAccessRequests={folderAccessRequests}
          resolvingFolderCalls={resolvingFolderCalls}
          folderAccessErrors={folderAccessErrors}
          decidingApprovalCalls={decidingApprovalCalls}
          approvalErrors={approvalErrors}
          onApproval={(callId, decision, remember) =>
            void onApproval(callId, decision, remember)
          }
          onFolderAccessDecision={(callId, decision) =>
            void onFolderAccessDecision(callId, decision)
          }
          onFolderAccessCancel={(callId, turnId) =>
            void onFolderAccessCancel(callId, turnId)
          }
          draft={draft}
          attachingSource={addingSourceChatId !== null}
          attachedSourceName={
            recentSource && recentSource.chatId === chat.id
              ? recentSource.source.displayName
              : null
          }
          sourceAttachmentError={
            sourceAttachmentError && sourceAttachmentError.chatId === chat.id
              ? sourceAttachmentError.message
              : null
          }
          composerModelMenu={
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
          cancelError={cancelError}
          cancelPendingTurnId={cancelPendingTurnId}
          steerError={steerError}
          steerStatus={steerStatus}
          steerPendingTurnId={steerPendingTurnId}
          onDraftChange={onComposerDraftChange}
          onAddSource={onAddSource}
          onDismissAttachedSource={() => setRecentSource(null)}
          onSelectPrompt={setComposerDraft}
          onSend={onSend}
          onSteer={onSteerActiveTurn}
          onStop={onCancelActiveTurn}
        />
        )}
      </div>
    </div>
  );
}

function withoutConnectionState(status: string): string {
  return status.replace(/ · (?:live|reconnecting)$/, "");
}

function friendlySourceAttachmentError(error: unknown): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : "Could not add that file.";
}
