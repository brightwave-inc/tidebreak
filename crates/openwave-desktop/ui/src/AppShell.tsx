import { useEffect, useRef, useState } from "react";
import { Outlet, useNavigate } from "@tanstack/react-router";

import {
  ApiClient,
  type Chat,
  type ModelInfo,
  type ProviderInfo,
  type ServerInfo,
} from "./api";
import { AppContextProvider } from "./AppContext";
import { resolveServerInfo } from "./boot";
import {
  deletionDescription,
  detachChatFolders,
  prependReplacementChat,
} from "./ChatDeletion";
import { useChatListStore } from "./ChatListStore";
import { useComposerDrafts } from "./ComposerDrafts";
import { useConfirm } from "./components/ConfirmDialog";
import { hasMacOverlayTitlebar } from "./host";
import { useInterfaceZoom } from "./InterfaceZoom";
import { Logomark } from "./Logomark";
import { ManagedGate } from "./ManagedGate";
import { resolvedRoleKey } from "./ModelSelection";
import { Titlebar } from "./Titlebar";
import { WindowDragStrip } from "./WindowDragStrip";
import { useActiveChatId } from "./useActiveChatId";
import { useChatPromptWatcher } from "./useChatPromptWatcher";
import { useShellShortcuts, type ShellShortcutHandlers } from "./ShellShortcuts";
import { ShortcutsDialog } from "./ShortcutsDialog";
import { useUiStore } from "./UiStore";
import { useDesktopUpdates } from "./updates";

/** Move focus to whichever composer the current route has on screen. */
function focusComposer(): void {
  document
    .querySelector<HTMLTextAreaElement>("[data-composer-input]")
    ?.focus();
}

// Store actions are stable for the store's lifetime; these handles are for
// calling actions only — never read state fields from them.
const chatListActions = useChatListStore.getState();

/**
 * The shell hooks that do work on their own — the parked-prompt poll and the
 * shortcuts that create chats. Mounted as a child of the managed gate rather
 * than in the shell itself, so that while the sign-in gate is up the app does
 * none of it: the gate is a hard stop, not a curtain in front of a running
 * app.
 */
function GatedShellHooks({
  client,
  chatId,
  shortcuts,
}: {
  client: ApiClient;
  chatId: string | null;
  shortcuts: ShellShortcutHandlers;
}) {
  // Watched here rather than in the conversation, so that the agent parking a
  // turn on a question is noticed whatever screen the reader is on.
  useChatPromptWatcher(client, chatId);
  useShellShortcuts(shortcuts);
  return null;
}

/**
 * The frame every route hangs in: the titlebar, the window, and the connection
 * to the local server.
 *
 * Everything here outlives a conversation. Anything scoped to one — its
 * transcript, its socket, its composer, and now its rail — belongs to the chat
 * route, which is remounted per chat and so cannot carry state across a switch.
 *
 * The mutations below stay here because they outlive the route that triggers
 * them: deleting the open conversation has to survive that conversation's own
 * unmount in order to decide what to open next. They reach the rails through
 * the app context rather than through props.
 */
export function AppShell() {
  const navigate = useNavigate();
  const [bootError, setBootError] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [defaultModelKey, setDefaultModelKey] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [status, setStatus] = useState("starting…");
  const openChatId = useActiveChatId();
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const skipRenameCommitRef = useRef(false);
  const creationInFlightRef = useRef(false);
  const deletionInFlightRef = useRef(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const desktopUpdates = useDesktopUpdates();
  const zoom = useInterfaceZoom();

  // Shell shortcuts are defined here because these actions outlive any one
  // route: toggling the frame, starting a chat, reaching the composer, and
  // scaling the window all work wherever the reader is. They are installed
  // below the gate, by GatedShellHooks.
  const shellShortcuts: ShellShortcutHandlers = {
    "toggle-sidebar": () => useUiStore.getState().toggleSidebar(),
    "new-chat": () => void onNewChat(),
    "focus-composer": focusComposer,
    "zoom-in": zoom.zoomIn,
    "zoom-out": zoom.zoomOut,
    "zoom-reset": zoom.resetZoom,
    "show-shortcuts": () => setShortcutsOpen(true),
  };

  useEffect(() => {
    let cancelled = false;
    void (async () => {
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
    };
  }, []);

  useEffect(() => {
    if (!client || !info) return;
    let cancelled = false;
    void (async () => {
      try {
        const [catalog, providerList] = await Promise.all([
          client.listModels(),
          client.listProviders(),
        ]);
        if (cancelled) return;
        setModels(catalog.models);
        setDefaultModelKey(resolvedRoleKey(catalog.roles, "chat"));
        setProviders(providerList.providers);
      } catch (err) {
        if (!cancelled) setBootError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, info]);

  // The chat list loads apart from the catalog: a failure here is a sidebar
  // problem, not a boot problem, and must not take down a shell the catalog
  // already stood up. The store hears about the failure so the gate that
  // sends a stale deep link home settles instead of waiting on a fetch that
  // already failed, and the rail shows the error with a way to retry.
  useEffect(() => {
    if (!client || !info) return;
    let cancelled = false;
    void (async () => {
      try {
        const existingChats = await client.listChats();
        if (!cancelled) chatListActions.setChats(existingChats);
      } catch (err) {
        if (!cancelled) {
          chatListActions.failChatsLoad(`Could not load chats: ${String(err)}`);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, info]);

  /** Reload the chat list after a failed fetch — the rail's retry calls this. */
  async function refreshChats() {
    if (!client) return;
    try {
      chatListActions.setChats(await client.listChats());
      chatListActions.setChatsError(null);
    } catch (err) {
      chatListActions.failChatsLoad(`Could not load chats: ${String(err)}`);
    }
  }

  async function refreshCatalog() {
    if (!client) return;
    const [catalog, providerList] = await Promise.all([
      client.listModels(),
      client.listProviders(),
    ]);
    setModels(catalog.models);
    setDefaultModelKey(resolvedRoleKey(catalog.roles, "chat"));
    setProviders(providerList.providers);
  }

  async function onNewChat() {
    if (!client || creationInFlightRef.current || deletionInFlightRef.current) return;
    creationInFlightRef.current = true;
    chatListActions.setCreatingChat(true);
    try {
      const created = await client.createChat();
      chatListActions.prependChat(created);
      chatListActions.setChatsError(null);
      await navigate({ to: "/c/$chatId", params: { chatId: created.id } });
    } catch (err) {
      chatListActions.setChatsError(`Could not create a chat: ${String(err)}`);
    } finally {
      creationInFlightRef.current = false;
      chatListActions.setCreatingChat(false);
    }
  }

  async function onDeleteChat(target: Chat) {
    if (!client || deletionInFlightRef.current || creationInFlightRef.current) return;
    const label = target.title?.trim() || "this chat";
    // The listed chat carries the folders it had at the last refresh, which
    // predates anything connected since. The server refuses the delete on its
    // own count, so ask it what is attached before promising to detach it.
    let current: Chat;
    try {
      current = await client.getChat(target.id);
    } catch (err) {
      chatListActions.setChatsError(`Could not delete chat: ${String(err)}`);
      return;
    }
    const confirmed = await confirm({
      title: `Delete ${label}?`,
      description: deletionDescription(current.root_attachments.length),
      confirmLabel: "Delete chat",
      destructive: true,
    });
    if (!confirmed) return;

    deletionInFlightRef.current = true;
    chatListActions.setDeletingChatId(target.id);
    chatListActions.setChatsError(null);
    // Read before the await: deleting the open conversation navigates away, and
    // by the time the request lands this is no longer the route we are on.
    const deletingOpenChat = openChatId === target.id;
    try {
      await detachChatFolders(current);
      await client.deleteChat(target.id);
      // Nothing left to send it to.
      useComposerDrafts.getState().clearDraft(target.id);
      let refreshed = await client.listChats();
      if (!deletingOpenChat) {
        chatListActions.setChats(refreshed);
        return;
      }
      let next: Chat | undefined = refreshed[0];
      if (!next) {
        next = await client.createChat();
        refreshed = prependReplacementChat(refreshed, next);
      }
      chatListActions.setChats(refreshed);
      await navigate({ to: "/c/$chatId", params: { chatId: next.id } });
    } catch (err) {
      chatListActions.setChatsError(`Could not delete chat: ${String(err)}`);
    } finally {
      deletionInFlightRef.current = false;
      chatListActions.setDeletingChatId(null);
    }
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
    chatListActions.setSavingTitle(true);
    try {
      const updated = await client.patchChatTitle(target.id, trimmed || null);
      chatListActions.replaceChat(updated);
      chatListActions.endRename();
    } catch (err) {
      chatListActions.setChatsError(`Could not rename chat: ${String(err)}`);
    } finally {
      chatListActions.setSavingTitle(false);
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
        <WindowDragStrip />
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{bootError}</p>
      </div>
    );
  }

  if (!client) {
    return (
      <div className="boot">
        <WindowDragStrip />
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{status}</p>
      </div>
    );
  }

  return (
    <ManagedGate client={client}>
      <GatedShellHooks
        client={client}
        chatId={openChatId}
        shortcuts={shellShortcuts}
      />
      <AppContextProvider
        value={{
          client,
          models,
          defaultModelKey,
          providers,
          refreshCatalog,
          refreshChats,
          status,
          setStatus,
          newChat: () => void onNewChat(),
          deleteChat: (target) => void onDeleteChat(target),
          startRename: startChatRename,
          commitRename: (target) => void commitChatRename(target),
          cancelRename: cancelChatRename,
          updateState: desktopUpdates.state,
          checkForUpdate: desktopUpdates.check,
          restartForUpdate: onRestartForUpdate,
        }}
      >
        <div className={`app-shell${hasMacOverlayTitlebar() ? " with-titlebar" : ""}`}>
          {confirmDialog}
          <ShortcutsDialog open={shortcutsOpen} onOpenChange={setShortcutsOpen} />
          {hasMacOverlayTitlebar() && <Titlebar />}
          {/* Each route renders its own rail beside its content — see RouteFrame. */}
          <div className="app-body">
            <Outlet />
          </div>
        </div>
      </AppContextProvider>
    </ManagedGate>
  );
}
