import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { Outlet, useNavigate, useRouter } from "@tanstack/react-router";
import { getVersion } from "@tauri-apps/api/app";
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
import { AppContextProvider, type AppContextValue } from "./AppContext";

import { resolveServerInfo } from "./boot";
import {
  BootFailure,
  type BootAttachment,
  type BootStage,
} from "./BootFailure";
import {
  disconnectRemoteMachine,
  remoteMachineAccessToken,
  remoteMachineState,
} from "./remoteMachine";
import {
  deletionDescription,
  detachChatFolders,
  purgeDeletedChatHostAuthority,
  prependReplacementChat,
} from "./ChatDeletion";
import { useChatListStore } from "./ChatListStore";
import {
  centerTabCount,
  closeFocusedCodeTab,
  selectCenterTab,
  splitFocusedEditor,
  stepCenterTab,
} from "./code/codeChrome";
import { CodeDeliveryMonitor } from "./code/CodeDeliveryMonitor";
import { activateCodeClient } from "./code/CodeClientScope";
import { useCodeUiStore } from "./code/CodeUiStore";
import { stepRailWorkspace } from "./code/railNavigation";
import {
  codeWorkspaceIdFromPath,
  isCodeRoute,
  shellShortcutMode,
} from "./code/routes";
import type { WorkflowShortcut } from "./code/workspaceWorkflow";
import {
  layoutFromSearch,
  searchFromLayout,
  type PanelSearch,
} from "./panel/panelUrl";
import type { LayoutState } from "./panel/panelTypes";
import { connectOutputs } from "./deliverables";
import { useProjectListStore } from "./ProjectListStore";
import { useComposerDrafts } from "./ComposerDrafts";
import { useConfirm } from "./components/ConfirmDialog";
import { ComputerUseIndicator } from "./ComputerUseIndicator";
import { useDesktopNavigation } from "./DesktopNavigation";
import {
  hasMacOverlayTitlebar,
  hasNativeHost,
  setAttachedRemotely,
} from "./host";
import { friendlyErrorMessage } from "./lib/utils";
import { useInterfaceZoom } from "./InterfaceZoom";
import { Logomark } from "./Logomark";
import { ManagedGate } from "./ManagedGate";
import { resolvedRoleKey } from "./ModelSelection";
import { SidebarExpandStrip } from "./sidebar/SidebarExpandStrip";
import { Titlebar } from "./Titlebar";
import { WindowDragStrip } from "./WindowDragStrip";
import { useActiveChatId } from "./useActiveChatId";
import { useAgentNotifications } from "./useAgentNotifications";
import { useChatPromptWatcher } from "./useChatPromptWatcher";
import {
  hasOpenModalDialog,
  numberedTabIndex,
  useShellShortcuts,
  type ShellShortcutHandlers,
  type ShellShortcutMode,
} from "./ShellShortcuts";
import { CommandPaletteDialog } from "./CommandPaletteDialog";
import { ShortcutsDialog } from "./ShortcutsDialog";
import { useUiStore } from "./UiStore";
import { UPDATE_CHECK_REQUESTED_EVENT, useDesktopUpdates } from "./updates";
import { UpdateReadyCard } from "./UpdateReadyCard";

/**
 * Raised by the native "Close Tab" menu item, which owns Cmd+W.
 *
 * The item exists so that chord can never reach macOS's close-window command:
 * this app has one window, so closing it ends the app, and a reader reaching
 * for Cmd+W means the tab in front of them.
 */
const CLOSE_TAB_REQUESTED_EVENT = "desktop-close-tab-requested";
const DISMISSED_UPDATE_VERSION_KEY = "tidebreak.dismissed-update-version";
const UNKNOWN_UPDATE_VERSION = "unknown";

/**
 * Run `handler` whenever the native host raises `event`.
 *
 * The handler is read through a ref so the listener registers once for the
 * shell's lifetime instead of being torn down and rebound every time a
 * callback changes identity between renders. Outside the desktop app there is
 * no host to raise anything, so nothing is registered at all.
 */
function useNativeHostEvent(event: string, handler: () => void): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void listen(event, () => handlerRef.current()).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [event]);
}

/** Move focus to whichever composer the current route has on screen. */
function focusComposer(): void {
  document.querySelector<HTMLTextAreaElement>("[data-composer-input]")?.focus();
}

/**
 * Whether a Monaco editor has the keyboard.
 *
 * Monaco binds its own Cmd+F to a find widget that searches the open file,
 * which is what a reader inside a file means by the chord. The container class
 * is Monaco's own and is on every instance, so this asks the DOM rather than
 * tracking which of several editors last mounted.
 */
function isMonacoFocused(): boolean {
  const active = document.activeElement;
  return active instanceof Element && active.closest(".monaco-editor") !== null;
}

// Store actions are stable for the store's lifetime; these handles are for
// calling actions only — never read state fields from them.
const chatListActions = useChatListStore.getState();
const projectListActions = useProjectListStore.getState();

/**
 * Every field of the app context that is invoked rather than read. The shell
 * rebuilds these closures each render (they capture that render's state), but
 * the context carries one stable set of forwarders so a callback changing
 * identity never invalidates the context value — see {@link AppShell}.
 */
type AppContextActions = Pick<
  AppContextValue,
  | "refreshCatalog"
  | "refreshChats"
  | "newChat"
  | "deleteChat"
  | "startRename"
  | "commitRename"
  | "cancelRename"
  | "newProject"
  | "deleteProject"
  | "startProjectRename"
  | "commitProjectRename"
  | "cancelProjectRename"
  | "newChatInProject"
  | "moveChatToProject"
  | "checkForUpdate"
  | "restartForUpdate"
>;

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
  useAgentNotifications(client);
  useShellShortcuts(shortcuts, shortcutMode);
  return <CodeDeliveryMonitor client={client} />;
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
  const [bootFailure, setBootFailure] = useState<{
    stage: BootStage;
    error: unknown;
  } | null>(null);
  // Bumped by the boot screen's "Try again". Boot is otherwise a mount-once
  // effect, so without this a reader who fixed the cause — reconnected the
  // VPN, woke the other machine — had no way to act on it but to quit.
  const [bootAttempt, setBootAttempt] = useState(0);
  // What the shell says this window is attached to. Read separately from
  // `info` because the connect stage can fail before `info` exists, and the
  // boot screen's whole job in that case is to name the machine it could not
  // reach.
  const [bootAttachment, setBootAttachment] = useState<BootAttachment | null>(
    null,
  );
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [defaultModelKey, setDefaultModelKey] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [status, setStatus] = useState("starting…");
  const [dismissedUpdateVersion, setDismissedUpdateVersion] = useState(() =>
    window.localStorage.getItem(DISMISSED_UPDATE_VERSION_KEY),
  );
  const [explicitUpdateCheckOpen, setExplicitUpdateCheckOpen] = useState(false);
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
  const sidebarWidth = useUiStore((state) => state.sidebarWidth);
  // Read at keydown rather than subscribed to: which mode a shortcut fires in
  // is the route's answer, and the shell has no other reason to re-render on
  // every navigation.
  const router = useRouter();
  const currentShortcutMode = () =>
    shellShortcutMode(router.state.location.pathname);

  // The native "Check for Updates…" menu item keeps the reader in place and
  // raises the update card while the explicit check runs. Automatic checks
  // stay quiet unless they stage an update.
  useNativeHostEvent(UPDATE_CHECK_REQUESTED_EVENT, () => {
    setExplicitUpdateCheckOpen(true);
    void desktopUpdates
      .check()
      .finally(() => setExplicitUpdateCheckOpen(false));
  });

  /**
   * Put the workspace's layout through one change and write it back.
   *
   * Every tab chord is the same three steps — read the layout out of the URL,
   * hand it to one of the pure functions in `codeChrome`, navigate to the
   * result — so they share this. Returning `false` declines the key: off a
   * workspace there is no strip to act on, and a change that returned the
   * layout untouched is one the strip could not make, so the key belongs to
   * whatever is focused instead.
   */
  function applyCodeLayout(
    change: (layout: LayoutState) => LayoutState,
  ): boolean | void {
    const { pathname, search } = router.state.location;
    const workspaceId = codeWorkspaceIdFromPath(pathname);
    if (!workspaceId) return false;
    const layout = layoutFromSearch(search as PanelSearch);
    const next = change(layout);
    if (next === layout) return false;
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
      search: searchFromLayout(next),
    });
  }

  /**
   * Raise a Ship chord for the workspace header to carry out.
   *
   * The shell knows the chord and nothing else: what "open a pull request"
   * means depends on the branch and pull-request state, which lives a route
   * down. Off a workspace there is nothing to ship, so the key goes back to
   * whatever is focused.
   */
  function askWorkspace(shortcut: WorkflowShortcut): boolean | void {
    const workspaceId = codeWorkspaceIdFromPath(router.state.location.pathname);
    if (!workspaceId) return false;
    useCodeUiStore.getState().requestWorkflowShortcut(workspaceId, shortcut);
  }

  /**
   * Move to the workspace the rail draws next, wherever in code mode we are.
   *
   * Works off a workspace too — from the code home or a delivery page, a step
   * enters the rail rather than doing nothing, which is what a reader arriving
   * by keyboard wants.
   */
  function stepWorkspace(delta: -1 | 1): boolean | void {
    const { pathname } = router.state.location;
    if (!isCodeRoute(pathname)) return false;
    const next = stepRailWorkspace(codeWorkspaceIdFromPath(pathname), delta);
    if (!next) return false;
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId: next },
    });
  }

  /**
   * Close whichever tab the reader is looking at.
   *
   * A closed tab is the one case the pure function reports with `null`, since
   * "nothing here to close" is not the same as "closing changed nothing".
   * Finding nothing declines the key and does nothing else: Cmd+W used to
   * reach the native close-window item and end the app, which is never what
   * the reader meant by it.
   */
  function closeTab(): boolean | void {
    return applyCodeLayout((layout) => closeFocusedCodeTab(layout) ?? layout);
  }

  // macOS claims a menu accelerator before the key reaches the webview, so on
  // the packaged app Cmd+W arrives as this event and never as a keydown the
  // shortcut table could match. Both paths run the same close, so the chord
  // means one thing whether the menu or the browser delivered it.
  useNativeHostEvent(CLOSE_TAB_REQUESTED_EVENT, () => {
    closeTab();
  });

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
      useCodeUiStore.getState().startNewWorkspace();
    },
    "toggle-code-review": () => {
      useCodeUiStore.getState().toggleReviewSidebar();
    },
    "toggle-code-terminal": () => {
      // Opening a shell is a server call, so the chord raises the ask and the
      // workspace page answers it.
      useCodeUiStore.getState().requestTerminal();
    },
    "code-new-tab": () => {
      const { pathname } = router.state.location;
      if (!codeWorkspaceIdFromPath(pathname)) return false;
      useCodeUiStore.getState().requestNewTabMenu();
    },
    "code-quick-open": () => {
      const { pathname } = router.state.location;
      if (!codeWorkspaceIdFromPath(pathname)) return false;
      useCodeUiStore.getState().requestQuickOpen();
    },
    "code-find": () => {
      // Monaco owns Cmd+F while it has focus, and its find widget is the
      // better answer inside a file than a filename search is. Declining the
      // key hands it back to the editor rather than to the shell.
      if (isMonacoFocused()) return false;
      const { pathname } = router.state.location;
      if (!codeWorkspaceIdFromPath(pathname)) return false;
      useCodeUiStore.getState().requestFilesSearch();
    },
    "code-prev-workspace": () => stepWorkspace(-1),
    "code-next-workspace": () => stepWorkspace(1),
    "code-workflow-next": () => askWorkspace("next"),
    "code-create-pr": () => askWorkspace("pull_request"),
    "code-update-branch": () => askWorkspace("update_branch"),
    "code-watch-pr": () => askWorkspace("watch"),
    "code-merge-pr": () => askWorkspace("merge"),
    "code-view-pr": () => askWorkspace("view_pr"),
    "code-source-control": () => askWorkspace("source_control"),
    "code-archive-workspace": () => {
      if (!codeWorkspaceIdFromPath(router.state.location.pathname))
        return false;
      useCodeUiStore.getState().requestArchiveWorkspace();
    },
    "code-prev-tab": () => applyCodeLayout((l) => stepCenterTab(l, -1)),
    "code-next-tab": () => applyCodeLayout((l) => stepCenterTab(l, 1)),
    "code-select-tab": (event) =>
      applyCodeLayout((layout) => {
        const position = numberedTabIndex(event.code, centerTabCount(layout));
        return position === null ? layout : selectCenterTab(layout, position);
      }),
    "code-split-editor": () => applyCodeLayout(splitFocusedEditor),
    "close-tab": closeTab,
    "open-command-palette": () => {
      // The chord is allowed through the modal guard so it can close the
      // palette. Any other dialog on screen means the reader is mid-decision,
      // so the key is declined rather than opening a second surface over it.
      const ui = useUiStore.getState();
      if (ui.commandPaletteOpen) ui.setCommandPaletteOpen(false);
      else if (hasOpenModalDialog(document)) return false;
      else ui.setCommandPaletteOpen(true);
    },
    "focus-composer": focusComposer,
    "zoom-in": zoom.zoomIn,
    "zoom-out": zoom.zoomOut,
    "zoom-reset": zoom.resetZoom,
    "reload-app": () => window.location.reload(),
    "show-shortcuts": () => setShortcutsOpen(true),
  };

  // The app version, for the boot screen's debug report. Only the packaged
  // host can report one; a browser dev build simply has none.
  useEffect(() => {
    if (!hasNativeHost()) return;
    let cancelled = false;
    void getVersion()
      .then((value) => {
        if (!cancelled) setAppVersion(value);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      // Asked before the connection, and independently of it: this is the
      // shell's own record of the address, so it still answers when the
      // attachment it describes is exactly what failed to connect.
      try {
        const state = await remoteMachineState();
        if (!cancelled) {
          setBootAttachment({
            attachment: state.attachment,
            baseUrl: state.baseUrl,
            gatewayAuth: null,
          });
        }
      } catch {
        // Not knowing the attachment costs the boot screen one sentence. It
        // must never be the reason boot itself reports a failure.
      }
      try {
        const server = await resolveServerInfo();
        if (cancelled) return;
        setBootFailure(null);
        setBootAttachment({
          attachment: server.attachment,
          baseUrl: server.attachment === "remote" ? server.baseUrl : null,
          gatewayAuth: server.gatewayAuth,
        });
        setInfo(server);
        // Before the client, and before anything can call one of them: the
        // three host callers that sit outside React read this flag rather
        // than a hook. See `host.ts`.
        setAttachedRemotely(server.attachment === "remote");
        const nextClient = new ApiClient(server.baseUrl, server.token);
        // Reset host-scoped Code state before React can mount routes against
        // the replacement authority. Every pending store write also checks
        // the generation activated here.
        activateCodeClient(nextClient);
        setClient(nextClient);
        // Outputs are read over the same API; their module holds the
        // connection because they are called from places with no client.
        connectOutputs(server.baseUrl, server.token);
        setStatus(`connected ${server.baseUrl}`);
      } catch (err) {
        if (!cancelled) setBootFailure({ stage: "connect", error: err });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [bootAttempt]);

  useEffect(() => {
    if (!client || !info?.gatewayAuth) return;
    let cancelled = false;
    const refresh = async () => {
      try {
        const token = await remoteMachineAccessToken();
        if (cancelled) return;
        client.setAccessToken(token);
        connectOutputs(client.baseUrl, token);
      } catch (error) {
        if (!cancelled)
          console.warn("could not refresh hosted Tidebreak access", error);
      }
    };
    const timer = window.setInterval(() => void refresh(), 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [client, info]);

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
        setBootFailure(null);
      } catch (err) {
        if (!cancelled) setBootFailure({ stage: "catalog", error: err });
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
          chatListActions.failChatsLoad(`Could not load work: ${String(err)}`);
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
      chatListActions.failChatsLoad(`Could not load work: ${String(err)}`);
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
      toast.error(friendlyErrorMessage(err, "Could not create the work."));
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
      toast.error(friendlyErrorMessage(err, "Could not move the work."));
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
    if (!client || creationInFlightRef.current || deletionInFlightRef.current)
      return;
    creationInFlightRef.current = true;
    chatListActions.setCreatingChat(true);
    try {
      const created = await client.createChat();
      chatListActions.prependChat(created);
      chatListActions.setChatsError(null);
      await navigate({ to: "/c/$chatId", params: { chatId: created.id } });
    } catch (err) {
      chatListActions.setChatsError(`Could not create work: ${String(err)}`);
    } finally {
      creationInFlightRef.current = false;
      chatListActions.setCreatingChat(false);
    }
  }

  async function onDeleteChat(target: Chat) {
    if (!client || deletionInFlightRef.current || creationInFlightRef.current)
      return;
    const label = target.title?.trim() || "this work";
    // The listed chat carries the folders it had at the last refresh, which
    // predates anything connected since. The server refuses the delete on its
    // own count, so ask it what is attached before promising to detach it.
    let current: Chat;
    try {
      current = await client.getChat(target.id);
    } catch (err) {
      chatListActions.setChatsError(`Could not delete work: ${String(err)}`);
      return;
    }
    const confirmed = await confirm({
      title: `Delete ${label}?`,
      description: deletionDescription(current.root_attachments.length),
      confirmLabel: "Delete work",
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
          `Work deleted, but host permissions could not be cleared: ${String(err)}`,
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
      chatListActions.setChatsError(`Could not delete work: ${String(err)}`);
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
      chatListActions.setChatsError(`Could not rename work: ${String(err)}`);
    } finally {
      chatListActions.setSavingTitle(false);
    }
  }

  async function onRestartForUpdate() {
    const version = desktopUpdates.state.version;
    const confirmed = await confirm({
      title: "Restart Tidebreak to update?",
      description: (
        <>
          <span>
            {version ? `Version ${version}` : "The update"} is ready. Tidebreak
            will close and reopen. Wait for active work to finish before
            restarting.
          </span>
          <span className="border-warning-border bg-warning-background text-warning-foreground mt-3 block rounded-md border px-3 py-2.5">
            <strong className="block font-medium">Pre-v1 data warning</strong>
            <span className="mt-0.5 block">
              Until Tidebreak reaches version 1.0, this update may wipe all
              Tidebreak data on this device.
            </span>
          </span>
        </>
      ),
      confirmLabel: "Restart and update",
    });
    if (confirmed) await desktopUpdates.restart();
  }

  function dismissUpdateNotice() {
    const version = desktopUpdates.state.version;
    setDismissedUpdateVersion(version ?? UNKNOWN_UPDATE_VERSION);
    if (version) {
      window.localStorage.setItem(DISMISSED_UPDATE_VERSION_KEY, version);
    }
  }

  /**
   * Run boot again from the top.
   *
   * Clearing the client as well as the failure matters: a catalog-stage
   * failure leaves a client built against a machine that just proved
   * unreachable, and re-running connect is the only thing that replaces it —
   * after a detach it has to, because the address it holds is gone.
   */
  function retryBoot() {
    setBootFailure(null);
    setClient(null);
    setInfo(null);
    setStatus("starting…");
    setBootAttempt((attempt) => attempt + 1);
  }

  /**
   * Forget the attached machine and boot against the server inside this app.
   *
   * The same command the Machine settings panel runs, offered here because a
   * reader whose remote machine is unreachable cannot get to that panel — the
   * panel lives behind the client that failed to reach it.
   */
  async function workOnThisComputer() {
    try {
      await disconnectRemoteMachine();
      setBootAttachment({
        attachment: "local",
        baseUrl: null,
        gatewayAuth: null,
      });
      retryBoot();
    } catch (err) {
      setBootFailure({ stage: "connect", error: err });
    }
  }

  // The context's mutations, rebound to this render's closures. Assigned into
  // a ref every render — the same pattern as `useNativeHostEvent` — so the
  // stable forwarders below always call the newest closure and never a stale
  // one.
  const contextActions: AppContextActions = {
    refreshCatalog,
    refreshChats,
    newChat: () => void onNewChat(),
    deleteChat: (target) => void onDeleteChat(target),
    startRename: startChatRename,
    commitRename: (target) => void commitChatRename(target),
    cancelRename: cancelChatRename,
    newProject: onNewProject,
    deleteProject: (target) => void onDeleteProject(target),
    startProjectRename,
    commitProjectRename: (target) => void commitProjectRename(target),
    cancelProjectRename,
    newChatInProject: (projectId) => void onNewChatInProject(projectId),
    moveChatToProject: (chat, projectId) =>
      void onMoveChatToProject(chat, projectId),
    checkForUpdate: desktopUpdates.check,
    restartForUpdate: onRestartForUpdate,
  };
  const contextActionsRef = useRef(contextActions);
  contextActionsRef.current = contextActions;
  // One set of forwarders for the shell's lifetime. Each has a stable identity
  // and delegates to whatever the ref holds now, which is what lets the
  // context value below memoize on data alone.
  const [stableActions] = useState<AppContextActions>(() => ({
    refreshCatalog: () => contextActionsRef.current.refreshCatalog(),
    refreshChats: () => contextActionsRef.current.refreshChats(),
    newChat: () => contextActionsRef.current.newChat(),
    deleteChat: (chat) => contextActionsRef.current.deleteChat(chat),
    startRename: (chat) => contextActionsRef.current.startRename(chat),
    commitRename: (chat) => contextActionsRef.current.commitRename(chat),
    cancelRename: () => contextActionsRef.current.cancelRename(),
    newProject: (title) => contextActionsRef.current.newProject(title),
    deleteProject: (project) =>
      contextActionsRef.current.deleteProject(project),
    startProjectRename: (project) =>
      contextActionsRef.current.startProjectRename(project),
    commitProjectRename: (project) =>
      contextActionsRef.current.commitProjectRename(project),
    cancelProjectRename: () => contextActionsRef.current.cancelProjectRename(),
    newChatInProject: (projectId) =>
      contextActionsRef.current.newChatInProject(projectId),
    moveChatToProject: (chat, projectId) =>
      contextActionsRef.current.moveChatToProject(chat, projectId),
    checkForUpdate: () => contextActionsRef.current.checkForUpdate(),
    restartForUpdate: () => contextActionsRef.current.restartForUpdate(),
  }));

  // Memoized so the ~34 useApp() consumers re-render when the data they read
  // changes, not whenever the shell does — before this, every shell render
  // (a status line update, an update-state event) re-rendered every consumer,
  // transcript included. `null` until boot lands a client; the shell early
  // returns before the provider ever sees that.
  const appContextValue = useMemo<AppContextValue | null>(
    () =>
      client && info
        ? {
            client,
            attachment: info.attachment,
            models,
            defaultModelKey,
            providers,
            status,
            setStatus,
            updateState: desktopUpdates.state,
            updateUpToDate: desktopUpdates.upToDate,
            ...stableActions,
          }
        : null,
    [
      client,
      info,
      models,
      defaultModelKey,
      providers,
      status,
      desktopUpdates.state,
      desktopUpdates.upToDate,
      stableActions,
    ],
  );

  if (bootFailure) {
    return (
      <BootFailure
        stage={bootFailure.stage}
        error={bootFailure.error}
        attachment={bootAttachment}
        appVersion={appVersion}
        onRetry={retryBoot}
        onWorkLocally={workOnThisComputer}
      />
    );
  }

  // `info` rides with `client`: boot sets both in one step, and the context
  // below reads the attachment off it. Guarding on both keeps that a fact
  // rather than an assumption.
  if (!client || !info) {
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
      <AppContextProvider value={appContextValue}>
        <div
          className={`app-shell${nativeTitlebar ? " with-titlebar" : ""}`}
          // Publish this as part of the shell's first render. An effect that
          // queries for `.app-shell` can run while the boot or managed gate is
          // still showing, then never retry when the shell finally mounts.
          style={
            {
              "--sidebar-expanded-width": `${sidebarWidth}px`,
            } as CSSProperties
          }
        >
          {confirmDialog}
          <ShortcutsDialog
            open={shortcutsOpen}
            onOpenChange={setShortcutsOpen}
          />
          <CommandPaletteDialog />
          {nativeTitlebar && !sidebarCollapsed && (
            <Titlebar
              macOverlay={macOverlayTitlebar}
              navigation={desktopNavigation}
            />
          )}
          <SidebarExpandStrip macOverlay={macOverlayTitlebar} />
          <ComputerUseIndicator />
          {explicitUpdateCheckOpen &&
            desktopUpdates.state.status !== "ready" && (
              <UpdateReadyCard
                status={
                  desktopUpdates.state.status === "downloading"
                    ? "downloading"
                    : "checking"
                }
                version={desktopUpdates.state.version}
                onDismiss={() => setExplicitUpdateCheckOpen(false)}
              />
            )}
          {desktopUpdates.state.status === "ready" &&
            dismissedUpdateVersion !==
              (desktopUpdates.state.version ?? UNKNOWN_UPDATE_VERSION) && (
              <UpdateReadyCard
                version={desktopUpdates.state.version}
                onRestart={() => void onRestartForUpdate()}
                onDismiss={dismissUpdateNotice}
              />
            )}
          {/* Each route renders its own rail beside its content — see RouteFrame. */}
          <div className="app-body">
            <Outlet />
          </div>
        </div>
      </AppContextProvider>
    </ManagedGate>
  );
}
