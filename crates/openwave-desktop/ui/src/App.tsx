import { useEffect, useRef, useState } from "react";
import {
  ApiClient,
  type Chat,
  type ModelInfo,
  type ModelSelectionKey,
  type ProviderInfo,
  type ReasoningEffort,
  type SequencedEvent,
  type ServerInfo,
} from "./api";
import { modelForSelection } from "./ModelSelection";
import { resolveServerInfo } from "./boot";
import { hasMacOverlayTitlebar, hasNativeHost } from "./host";
import { Logomark } from "./Logomark";
import { useTheme } from "./theme";
import { SettingsView } from "./SettingsView";
import { ModelMenu, ReasoningEffortMenu } from "./ModelMenu";
import { ChatSessionController } from "./ChatSessionController";
import { useChatSessionStore } from "./ChatSessionStore";
import { useRefreshSignals } from "./RefreshSignals";
import { useTurnLifecycle } from "./TurnLifecycleSignals";
import { useChatListStore } from "./ChatListStore";
import { useUiStore } from "./UiStore";
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
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { prependReplacementChat } from "./ChatDeletion";
import { useConfirm } from "./components/ConfirmDialog";
import { PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { useDesktopUpdates } from "./updates";
import { ChatView } from "./ChatView";
import { ChatWorkspace } from "./ChatWorkspace";
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
const { signal: signalRefresh } = useRefreshSignals.getState();
const { signal: signalTurnLifecycle } = useTurnLifecycle.getState();

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
  const surface = useUiStore((state) => state.surface);
  const sidebarCollapsed = useUiStore((state) => state.sidebarCollapsed);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const busy = useChatSessionStore((session) => session.busy);
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
  const skipRenameCommitRef = useRef(false);
  const [status, setStatus] = useState("starting…");
  // Owns the selected chat's event socket; chat switches dispose it eagerly
  // and the connection effect below constructs a fresh one.
  const controllerRef = useRef<ChatSessionController | null>(null);
  const handleEventRef = useRef<(event: SequencedEvent) => void>(() => {});
  const chatSelectionRef = useRef(0);
  const terminalHydrationGenerationRef = useRef(0);
  const draftRef = useRef("");
  const selectedChatIdRef = useRef<string | null>(null);
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
          (await client.createChat());
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
      case "refresh_folder_access":
        signalRefresh("folderAccess");
        return;
      case "refresh_user_questions":
        signalRefresh("userQuestions");
        return;
      case "turn_began":
        signalTurnLifecycle(
          effect.startsDifferentTurn ? "began" : "began_same_turn",
        );
        return;
      case "turn_resolved":
        signalTurnLifecycle("resolved");
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
    signalTurnLifecycle("resolved");
  }

  function setComposerDraft(nextDraft: string) {
    draftRef.current = nextDraft;
    setDraft(nextDraft);
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
    signalTurnLifecycle("submitted");
    try {
      await client.postMessage(chatId, turnId, content);
      if (chatSelectionRef.current !== selection) return;
      setRecentSource((current) =>
        current?.chatId === chatId ? null : current,
      );
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

  async function onNewChat() {
    if (!client || creationInFlightRef.current || deletionInFlightRef.current) return;
    creationInFlightRef.current = true;
    chatListActions.setCreatingChat(true);
    try {
      const created = await client.createChat();
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
    controllerRef.current?.dispose();
    controllerRef.current = null;
    useChatSessionStore.getState().reset();
    setComposerDraft("");
    setRecentSource(null);
    setSourceAttachmentError(null);
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
      // ref update also gates sends before the disabled composer renders. The
      // chat pane retires its own in-flight work off the deleting chat id.
      chatSelectionRef.current += 1;
    }
    try {
      await client.deleteChat(target.id);
      uiActions.forgetChatWorkspace(target.id);
      let refreshed = await client.listChats();
      if (!deletingSelectedChat) {
        chatListActions.setChats(refreshed);
        return;
      }

      let next: Chat | undefined = refreshed[0];
      if (!next) {
        next = await client.createChat();
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

  /**
   * Opens `next` as the selected conversation. Everything scoped to a single
   * conversation — its agent runs, its pending requests, its turn controls —
   * lives in the chat pane, which is keyed on the chat id, so this remounts it
   * with nothing carried over from the chat being left behind.
   */
  function activateChat(next: Chat) {
    uiActions.selectChatWorkspace(next.id);
    chatSelectionRef.current += 1;
    terminalHydrationGenerationRef.current += 1;
    selectedChatIdRef.current = next.id;
    updateSession((session) => ({
      ...session,
      markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    }));
    chatListActions.setSelected(next);
  }

  function selectChat(next: Chat, force = false) {
    // Re-selecting the open chat still has work to do: it leaves a global
    // surface like Settings and restores the layout this chat was left in.
    if (next.id === chat?.id) uiActions.selectChatWorkspace(next.id);
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
    setComposerDraft("");
    setRecentSource(null);
    setSourceAttachmentError(null);
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

  async function onModelChange(modelId: ModelSelectionKey | null) {
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
    <div
      className={`app-shell${hasMacOverlayTitlebar() ? " with-titlebar" : ""}${sidebarCollapsed ? " sidebar-collapsed" : ""}`}
    >
      {confirmDialog}
      {hasMacOverlayTitlebar() && (
        <div className="titlebar" data-tauri-drag-region>
          <button
            type="button"
            className="titlebar-panel-toggle"
            aria-label={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}
            title={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}
            onClick={() => useUiStore.getState().toggleSidebar()}
          >
            {sidebarCollapsed ? (
              <PanelLeftOpen size={15} />
            ) : (
              <PanelLeftClose size={15} />
            )}
          </button>
        </div>
      )}
      <div className="app-body">
      {!hasMacOverlayTitlebar() && sidebarCollapsed && (
        <button
          type="button"
          className="sidebar-expand"
          aria-label="Show sidebar"
          title="Show sidebar"
          onClick={() => useUiStore.getState().toggleSidebar()}
        >
          <PanelLeftOpen size={15} />
        </button>
      )}
      {!sidebarCollapsed && <Sidebar
        collapseControl={!hasMacOverlayTitlebar()}
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
      />}

      <div className="main">
        {surface.kind === "settings" && (
          <SettingsView
            client={client}
            models={models}
            providers={providers}
            onProvidersChanged={() => void refreshCatalog()}
            onBack={() => uiActions.selectChatWorkspace(chat.id)}
            themeMode={themeMode}
            onThemeChange={setThemeMode}
            updateState={desktopUpdates.state}
            onCheckForUpdate={desktopUpdates.check}
            onRestartForUpdate={onRestartForUpdate}
          />
        )}
        {/*
          Kept mounted behind Settings, not swapped out for it. The pollers for
          this conversation's pending prompts live in the pane, and a reader who
          steps into Settings should still be told when the agent asks them
          something.
        */}
        <ChatWorkspace
          chat={chat}
          status={status}
          nativeHost={hasNativeHost()}
          hidden={surface.kind === "settings"}
          transcript={
            <ChatView
              key={chat.id}
              client={client}
              chat={chat}
              hydrated={hydratedChatId === chat.id}
              nativeHost={hasNativeHost()}
              deletingChat={deletingChatId !== null}
              draft={draft}
              draftRef={draftRef}
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
                  {modelForSelection(models, chat.model)?.supports_reasoning_effort && (
                    <ReasoningEffortMenu
                      value={chat.reasoning_effort}
                      disabled={deletingChatId !== null}
                      onChange={onReasoningEffortChange}
            />
                  )}
                </>
              }
              onDraftChange={setComposerDraft}
              onAddSource={onAddSource}
              onDismissAttachedSource={() => setRecentSource(null)}
              onSelectPrompt={setComposerDraft}
              onSend={onSend}
          />
          }
        />
      </div>
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
