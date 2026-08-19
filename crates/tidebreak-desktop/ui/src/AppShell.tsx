import { useEffect, useRef, useState } from "react";
import { Outlet, useNavigate, useRouter } from "@tanstack/react-router";
import { isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { toast } from "sonner";

import {
  ApiClient,
  type Chat,
  type ModelInfo,
  type Project,
  type ProviderInfo,
  type ServerInfo,
} from "./api";
import { AppContextProvider } from "./AppContext";

import { resolveServerInfo } from "./boot";
import {
  deletionDescription,
  detachChatFolders,
  purgeDeletedChatHostAuthority,
  prependReplacementChat,
} from "./ChatDeletion";
import { useChatListStore } from "./ChatListStore";
import { closeCodeChromeTab, splitCodeChromeLayout, toggleTerminalLayout } from "./code/codeChrome";
import { useCodeUiStore } from "./code/CodeUiStore";
import {
  codeRepoIdFromPath,
  codeWorkspaceIdFromPath,
  shellShortcutMode,
} from "./code/routes";
import { layoutFromSearch, searchFromLayout, type PanelSearch } from "./panel/panelUrl";
import { useExperimentalFlags } from "./experimental";
import { connectOutputs } from "./deliverables";
import { useProjectListStore } from "./ProjectListStore";
import { useComposerDrafts } from "./ComposerDrafts";
import { useConfirm } from "./components/ConfirmDialog";
import { ComputerUseIndicator } from "./ComputerUseIndicator";
import { useDesktopNavigation } from "./DesktopNavigation";
import { hasMacOverlayTitlebar, hasNativeHost } from "./host";
import { friendlyErrorMessage } from "./lib/utils";
import { useInterfaceZoom } from "./InterfaceZoom";
import { Logomark } from "./Logomark";
import { ManagedGate } from "./ManagedGate";
import { resolvedRoleKey } from "./ModelSelection";
import { SidebarExpandStrip } from "./sidebar/SidebarExpandStrip";
import { useSyncSidebarWidthCssVar } from "./sidebar/primitives";
import { Titlebar } from "./Titlebar";
import { WindowDragStrip } from "./WindowDragStrip";
import { useActiveChatId } from "./useActiveChatId";
import { useChatPromptWatcher } from "./useChatPromptWatcher";
import {
  useShellShortcuts,
  type ShellShortcutHandlers,
  type ShellShortcutMode,
} from "./ShellShortcuts";
import { ShortcutsDialog } from "./ShortcutsDialog";
import { useUiStore } from "./UiStore";
import { UPDATE_CHECK_REQUESTED_EVENT, useDesktopUpdates } from "./updates";

/** Move focus to whichever composer the current route has on screen. */
function focusComposer(): void {
  document
    .querySelector<HTMLTextAreaElement>("[data-composer-input]")
    ?.focus();
}

// Store actions are stable for the store's lifetime; these handles are for
// calling actions only — never read state fields from them.
const chatListActions = useChatListStore.getState();
const projectListActions = useProjectListStore.getState();

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
  shortcutMode,
}: {
  client: ApiClient;
  chatId: string | null;
  shortcuts: ShellShortcutHandlers;
  shortcutMode: () => ShellShortcutMode;
}) {
  // Watched here rather than in the conversation, so that the agent parking a
  // turn on a question is noticed whatever screen the reader is on.
  useChatPromptWatcher(client, chatId);
  useShellShortcuts(shortcuts, shortcutMode);
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
  // One fence over every project mutation, including the chat moves they drive:
  // create/rename/delete/move all rewrite the same rail and must not interleave.
  const projectMutationRef = useRef(false);
  const skipProjectRenameCommitRef = useRef(false);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const desktopUpdates = useDesktopUpdates();
  const desktopNavigation = useDesktopNavigation();
  const zoom = useInterfaceZoom();
  const sidebarCollapsed = useUiStore((state) => state.sidebarCollapsed);
  // The expanded rail width is published by the shell, not the rail itself, so
  // the titlebar can size to it while the rail is mounted.
  useSyncSidebarWidthCssVar();
  // Read at keydown rather than subscribed to: which mode a shortcut fires in
  // is the route's answer, and the shell has no other reason to re-render on
  // every navigation.
  const router = useRouter();
  const currentShortcutMode = () =>
    shellShortcutMode(router.state.location.pathname);

  // The native "Check for Updates…" menu item lands the reader on the
  // Updates settings panel and runs the same explicit check the panel's
  // button does, so the result (up to date, or an update staged) is visible.
  const checkForUpdateRef = useRef(desktopUpdates.check);
  checkForUpdateRef.current = desktopUpdates.check;
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void listen(UPDATE_CHECK_REQUESTED_EVENT, () => {
      const updatesPath: string = "/settings/updates";
      void navigate({ to: updatesPath });
      void checkForUpdateRef.current();
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [navigate]);

  // Shell shortcuts are defined here because these actions outlive any one
  // route: toggling the frame, starting a chat, reaching the composer, and
  // scaling the window all work wherever the reader is. Mode-scoped ones act
  // on whichever half of the app the route is in. They are installed below the
  // gate, by GatedShellHooks.
  const shellShortcuts: ShellShortcutHandlers = {
    "history-back": () => {
      if (desktopNavigation.canGoBack) desktopNavigation.goBack();
    },
    "history-forward": () => {
      if (desktopNavigation.canGoForward) desktopNavigation.goForward();
    },
    "toggle-sidebar": () => useUiStore.getState().toggleSidebar(),
    "new-chat": () => void onNewChat(),
    "code-new-workspace": () => {
      const { pathname } = router.state.location;
      useCodeUiStore
        .getState()
        .startNewWorkspace(codeRepoIdFromPath(pathname));
    },
    "toggle-code-review": () => {
      useCodeUiStore.getState().toggleReviewSidebar();
    },
    "toggle-code-terminal": () => {
      const { pathname, search } = router.state.location;
      const workspaceId = codeWorkspaceIdFromPath(pathname);
      if (!workspaceId) return;
      const next = toggleTerminalLayout(layoutFromSearch(search as PanelSearch));
      void navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId },
        search: searchFromLayout(next),
      });
    },
    "close-tab": () => {
      const { pathname, search } = router.state.location;
      const workspaceId = codeWorkspaceIdFromPath(pathname);
      if (!workspaceId) return false;
      const layout = layoutFromSearch(search as PanelSearch);
      const chrome = splitCodeChromeLayout(layout);
      if (chrome.panels.tabs.length === 0) return false;
      const next = closeCodeChromeTab(layout, chrome.panels.activeIndex);
      void navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId },
        search: searchFromLayout(next),
      });
    },
    "focus-composer": focusComposer,
    "zoom-in": zoom.zoomIn,
    "zoom-out": zoom.zoomOut,
    "zoom-reset": zoom.resetZoom,
    "reload-app": () => window.location.reload(),
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
        // Outputs are read over the same API; their module holds the
        // connection because they are called from places with no client.
        connectOutputs(server.baseUrl, server.token);
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
    // Loads apart from the catalog on purpose: a flags failure keeps the
    // opted-out defaults instead of taking down the shell.
    void useExperimentalFlags.getState().refresh(client);
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

  // Projects load beside the chats and fail as quietly: the rail simply shows
  // no Projects section, which is also what a reader with none sees. Nothing
  // else in the app depends on the list, so a failure here is not worth a
  // second error banner over the sidebar.
  useEffect(() => {
    if (!client || !info) return;
    let cancelled = false;
    void (async () => {
      try {
        const existing = await client.listProjects();
        if (!cancelled) projectListActions.setProjects(existing);
      } catch {
        if (!cancelled) projectListActions.failProjectsLoad();
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, info]);

  /**
   * Create a named project and the first chat inside it.
   *
   * A project with no conversation is a folder the reader has to fill. The
   * dialog already collected the name, so this does both writes and opens the
   * chat rather than leaving an empty row in the rail.
   */
  async function onNewProject(title: string): Promise<boolean> {
    const trimmed = title.trim();
    if (!client || !trimmed || projectMutationRef.current) return false;
    projectMutationRef.current = true;
    projectListActions.setCreatingProject(true);
    try {
      const created = await client.createProject(trimmed);
      projectListActions.prependProject(created);
      await onNewChatInProject(created.id);
      return true;
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not create the project."));
      return false;
    } finally {
      projectMutationRef.current = false;
      projectListActions.setCreatingProject(false);
    }
  }

  function startProjectRename(target: Project) {
    skipProjectRenameCommitRef.current = false;
    projectListActions.beginProjectRename(target);
  }

  function cancelProjectRename() {
    skipProjectRenameCommitRef.current = true;
    projectListActions.endProjectRename();
  }

  async function commitProjectRename(target: Project) {
    // Same blur/Escape dance as the chat rename above.
    if (skipProjectRenameCommitRef.current) {
      skipProjectRenameCommitRef.current = false;
      return;
    }
    if (!client || projectMutationRef.current) return;
    if (useProjectListStore.getState().savingProjectTitle) return;
    const trimmed = useProjectListStore.getState().renameProjectDraft.trim();
    if (trimmed === (target.title?.trim() ?? "")) {
      projectListActions.endProjectRename();
      return;
    }
    projectMutationRef.current = true;
    projectListActions.setSavingProjectTitle(true);
    try {
      const updated = await client.patchProjectTitle(
        target.id,
        trimmed || null,
      );
      projectListActions.replaceProject(updated);
      projectListActions.endProjectRename();
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not rename the project."));
    } finally {
      projectMutationRef.current = false;
      projectListActions.setSavingProjectTitle(false);
    }
  }

  /**
   * Delete a project, taking its conversations out of it first.
   *
   * The server refuses to delete a project that still holds anything, and
   * rightly so — but a reader deleting a folder means the folder, not the
   * conversations in it. So each chat is moved out and kept, and only the
   * project itself goes. A chat that cannot be moved (it still holds connected
   * folders) leaves the project standing, and the toast says why.
   */
  async function onDeleteProject(target: Project) {
    if (!client || projectMutationRef.current) return;
    const label = target.title?.trim() || "this project";
    const held = useChatListStore
      .getState()
      .chats.filter((chat) => chat.project_id === target.id);
    const confirmed = await confirm({
      title: `Delete ${label}?`,
      description: held.length
        ? `Its ${held.length === 1 ? "conversation moves" : `${held.length} conversations move`} back to Recents. Nothing is deleted but the project itself.`
        : "This project is empty.",
      confirmLabel: "Delete project",
      destructive: true,
    });
    if (!confirmed) return;

    projectMutationRef.current = true;
    projectListActions.setDeletingProjectId(target.id);
    try {
      // Re-read rather than reusing the set the prompt was worded from: the
      // dialog was open for as long as the reader took, and a chat filed
      // somewhere else in the meantime is not this project's to move.
      const moving = useChatListStore
        .getState()
        .chats.filter((chat) => chat.project_id === target.id);
      for (const chat of moving) {
        chatListActions.replaceChat(
          await client.moveChatToProject(chat.id, null),
        );
      }
      await client.deleteProject(target.id);
      projectListActions.removeProject(target.id);
      // The open conversation may have just moved out from under its URL.
      if (openChatId && moving.some((chat) => chat.id === openChatId)) {
        await navigate({ to: "/c/$chatId", params: { chatId: openChatId } });
      }
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not delete the project."));
      await refreshChats();
    } finally {
      projectMutationRef.current = false;
      projectListActions.setDeletingProjectId(null);
    }
  }

  /** Start a conversation inside a project and open it there. */
  async function onNewChatInProject(projectId: string) {
    if (!client || creationInFlightRef.current || deletionInFlightRef.current)
      return;
    creationInFlightRef.current = true;
    chatListActions.setCreatingChat(true);
    try {
      const created = await client.createChat(undefined, projectId);
      chatListActions.prependChat(created);
      chatListActions.setChatsError(null);
      projectListActions.expandProject(projectId);
      await navigate({
        to: "/p/$projectId/c/$chatId",
        params: { projectId, chatId: created.id },
      });
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not create the chat."));
    } finally {
      creationInFlightRef.current = false;
      chatListActions.setCreatingChat(false);
    }
  }

  /** File a conversation under a project, or take it back out with `null`. */
  async function onMoveChatToProject(chat: Chat, projectId: string | null) {
    if (!client || projectMutationRef.current) return;
    if ((chat.project_id ?? null) === projectId) return;
    projectMutationRef.current = true;
    try {
      const moved = await client.moveChatToProject(chat.id, projectId);
      chatListActions.replaceChat(moved);
      if (projectId) projectListActions.expandProject(projectId);
      // Keep the open conversation's URL honest about where it now lives.
      if (openChatId === chat.id) {
        await (projectId
          ? navigate({
              to: "/p/$projectId/c/$chatId",
              params: { projectId, chatId: chat.id },
            })
          : navigate({ to: "/c/$chatId", params: { chatId: chat.id } }));
      }
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not move the chat."));
    } finally {
      projectMutationRef.current = false;
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
      // Chat ids are never reused; residual broker grants for this subject are
      // leftover authority and would otherwise haunt Permissions as a ghost chat.
      try {
        await purgeDeletedChatHostAuthority(target.id);
      } catch (err) {
        // Product delete already committed. Surface the host cleanup failure
        // without undoing the delete — startup reconcile is the backup path.
        chatListActions.setChatsError(
          `Chat deleted, but host permissions could not be cleared: ${String(err)}`,
        );
      }
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
      chatListActions.replaceChat(updated, true);
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
      title: "Restart Tidebreak to update?",
      description: `${version ? `Version ${version}` : "The update"} is ready. Tidebreak will close and reopen. Wait for active work to finish before restarting.`,
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
          <h1>Tidebreak</h1>
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
          <h1>Tidebreak</h1>
        </div>
        <p>{status}</p>
      </div>
    );
  }

  const nativeTitlebar = hasNativeHost();
  const macOverlayTitlebar = hasMacOverlayTitlebar();

  return (
    <ManagedGate client={client}>
      <GatedShellHooks
        client={client}
        chatId={openChatId}
        shortcuts={shellShortcuts}
        shortcutMode={currentShortcutMode}
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
          newProject: (title) => onNewProject(title),
          deleteProject: (target) => void onDeleteProject(target),
          startProjectRename,
          commitProjectRename: (target) => void commitProjectRename(target),
          cancelProjectRename,
          newChatInProject: (projectId) => void onNewChatInProject(projectId),
          moveChatToProject: (chat, projectId) =>
            void onMoveChatToProject(chat, projectId),
          updateState: desktopUpdates.state,
          updateUpToDate: desktopUpdates.upToDate,
          checkForUpdate: desktopUpdates.check,
          restartForUpdate: onRestartForUpdate,
        }}
      >
        <div className={`app-shell${nativeTitlebar ? " with-titlebar" : ""}`}>
          {confirmDialog}
          <ShortcutsDialog open={shortcutsOpen} onOpenChange={setShortcutsOpen} />
          {nativeTitlebar && !sidebarCollapsed && (
            <Titlebar
              macOverlay={macOverlayTitlebar}
              navigation={desktopNavigation}
            />
          )}
          <SidebarExpandStrip macOverlay={macOverlayTitlebar} />
          <ComputerUseIndicator />
          {/* Each route renders its own rail beside its content — see RouteFrame. */}
          <div className="app-body">
            <Outlet />
          </div>
        </div>
      </AppContextProvider>
    </ManagedGate>
  );
}
