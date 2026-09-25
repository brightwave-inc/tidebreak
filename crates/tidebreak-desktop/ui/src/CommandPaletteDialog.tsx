import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { isListableChat, sortChats } from "./chatListGroups";
import { useChatListStore } from "./ChatListStore";
import { useProjectListStore } from "./ProjectListStore";
import { useCodeCatalogStore } from "./code/CodeCatalogStore";
import { useCodeUiStore } from "./code/CodeUiStore";
import { useWorkspaceDigests } from "./code/CodeUpdatesStore";
import {
  codeNavigationPaletteRows,
  shipPaletteRows,
  suggestedShipRow,
  workspaceActionPaletteRows,
  workspacePaletteRows,
} from "./code/codePaletteRows";
import { codeWorkspaceIdFromPath, shellShortcutMode } from "./code/routes";
import { arrangeWorkspaces } from "./code/workspaceCards";
import { useWorkspaceCardCommands } from "./code/workspaceActions";
import {
  chatNavigationPaletteRows,
  chatPaletteRows,
  projectPaletteRows,
} from "./chatPaletteRows";
import {
  parsePaletteQuery,
  rankPaletteRows,
  readPaletteRecents,
  rememberPaletteRow,
  type PaletteRow,
} from "./CommandPalette";
import { CommandPaletteList } from "./CommandPaletteList";
import { settingsPaletteRows } from "./settingsPaletteRows";
import { useManagedPolicy } from "./managedPolicy";
import { useUiStore } from "./UiStore";
import {
  appPaletteRows,
  currentChatPaletteRows,
  findPaletteRow,
} from "./appPaletteRows";
import { exportChatConversation } from "./chatExport";
import type { InterfaceZoom } from "./InterfaceZoom";
import type { MessageSearchHit } from "./generated/wire";
import { hitRoute, hitTarget, queryTerms } from "./search/messageSearch";
import { useTranscriptFindStore } from "./search/transcriptFind";
import { useTranscriptRevealStore } from "./search/transcriptReveal";
import { useMessageSearch } from "./search/useMessageSearch";
import { sidebarUsesOverlay } from "./sidebar/sidebarLayout";
import { useTheme } from "./theme";

/** The tree read is bounded the same way the file picker's is. */
const TREE_LIMIT = 5000;

/** Message hits the palette shows; the find bar reaches the rest. */
const PALETTE_MESSAGE_HITS = 8;

/**
 * The command palette: one keyboard surface over everything the app can do.
 *
 * Rows come from each half of the app rather than from here, so this file only
 * decides what the reader is in front of — which mode, which workspace, which
 * conversation — and hands that to the sources. Adding a command is a change
 * to `codePaletteRows` or `chatPaletteRows`, never to this file.
 *
 * Mounted once in the shell. Cmd+K is in the shell keymap rather than a
 * listener here, so it appears in the shortcuts dialog and closes the palette
 * as well as opening it.
 */
export function CommandPaletteDialog({
  onShowShortcuts,
  onCheckForUpdates,
  zoom,
}: {
  /** Open the keyboard shortcuts dialog the shell owns. */
  onShowShortcuts?: () => void;
  /** The native menu's explicit update check; absent where there is none. */
  onCheckForUpdates?: () => void;
  zoom?: InterfaceZoom;
} = {}) {
  const { client, newChat, startRename, deleteChat, moveChatToProject } =
    useApp();
  const navigate = useNavigate();
  const { managed } = useManagedPolicy();
  const theme = useTheme();
  const open = useUiStore((state) => state.commandPaletteOpen);
  const setOpen = useUiStore((state) => state.setCommandPaletteOpen);
  const [query, setQuery] = useState("");
  const [recents, setRecents] = useState<string[]>([]);
  // The shell's actions change identity on every render; the rows read them
  // when picked, so they are held here instead of re-ranking the list.
  const shellActions = useRef({ onShowShortcuts, onCheckForUpdates, zoom });
  shellActions.current = { onShowShortcuts, onCheckForUpdates, zoom };
  const canCheckForUpdates = onCheckForUpdates !== undefined;
  const canShowShortcuts = onShowShortcuts !== undefined;
  // Set by a row whose action takes focus somewhere of its own.
  const keepFocusOnClose = useRef(false);

  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const mode = shellShortcutMode(pathname);
  const workspaceId = codeWorkspaceIdFromPath(pathname);

  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const repos = useCodeCatalogStore((state) => state.repos);
  const sessions = useCodeCatalogStore((state) => state.sessionsByWorkspace);
  const digests = useWorkspaceDigests();
  const railPrefs = useCodeUiStore((state) => state.railPrefs);
  const suggestion = useCodeUiStore((state) => state.workflowSuggestion);
  const chats = useChatListStore((state) => state.chats);
  const projects = useProjectListStore((state) => state.projects);
  // The same runner the card context menu and the header overflow use, with
  // its own rename box and archive confirmation. The palette picks a command;
  // it does not learn how to carry one out.
  const workspaceRunner = useWorkspaceCardCommands();

  // Read on open rather than on mount: another window may have picked
  // something since, and the memory is only consulted while the list is up.
  useEffect(() => {
    if (open) setRecents(readPaletteRecents());
    else setQuery("");
  }, [open]);

  const parsed = parsePaletteQuery(query);
  const filesWanted = open && (parsed.sections?.includes("files") ?? false);
  const files = useWorkspaceFilePaths(client, workspaceId, filesWanted);
  // What was said, searched as the reader types. A prefix narrows the list
  // to one kind of row, and messages are not one of them.
  const messages = useMessageSearch(client, parsed.query, {
    enabled: open && parsed.sections === null,
    limit: PALETTE_MESSAGE_HITS,
  });

  const rows = useMemo<PaletteRow[]>(() => {
    // Settings sections are addressed from a runtime table and never enter the
    // router's generated path union, so the cast happens once, here.
    const go = (path: string) => void navigate({ to: path as "/" });
    const settings = settingsPaletteRows({ managed, navigate: go });
    // Store actions are stable for the store's lifetime, so reading them here
    // keeps the memo from re-running on every unrelated store write.
    const codeUi = useCodeUiStore.getState();
    const findInConversation = () => {
      useTranscriptFindStore.getState().requestOpen();
    };
    const app = appPaletteRows({
      theme: theme.mode,
      onTheme: theme.setMode,
      onZoomIn: () => shellActions.current.zoom?.zoomIn(),
      onZoomOut: () => shellActions.current.zoom?.zoomOut(),
      onZoomReset: () => shellActions.current.zoom?.resetZoom(),
      onNotifications: showNotifications,
      onShortcuts: canShowShortcuts
        ? () => shellActions.current.onShowShortcuts?.()
        : undefined,
      onCheckForUpdates: canCheckForUpdates
        ? () => shellActions.current.onCheckForUpdates?.()
        : undefined,
    });

    if (mode === "chat") {
      const openChatId = chatIdFromPath(pathname);
      const openChat = openChatId
        ? chats.find((chat) => chat.id === openChatId)
        : undefined;
      return [
        ...(openChat
          ? currentChatPaletteRows({
              chat: openChat,
              projects,
              onFind: findInConversation,
              onRename: () => startRename(openChat),
              onDelete: () => deleteChat(openChat),
              onMove: (projectId) => moveChatToProject(openChat, projectId),
              onExport: () => void exportChatConversation(client, openChat.id),
            })
          : []),
        ...chatPaletteRows({
          // The rail's rows in the rail's order, so with nothing typed the
          // palette opens on the conversations the reader already sees.
          chats: sortChats(chats.filter((chat) => isListableChat(chat))),
          projects,
          activeChatId: openChatId,
          onOpen: (chat) => go(`/c/${chat.id}`),
        }),
        ...projectPaletteRows({
          projects,
          onOpen: (project) => go(`/p/${project.id}`),
        }),
        ...chatNavigationPaletteRows({
          navigate: go,
          onNewChat: newChat,
        }),
        ...settings,
        ...app,
      ];
    }

    const arranged = arrangeWorkspaces(
      railPrefs.sortMode,
      repos,
      workspaces,
      digests,
      sessions,
    );
    const workspace = workspaces.find((entry) => entry.id === workspaceId);
    const repo = repos.find((entry) => entry.id === workspace?.repo_id);
    const digest = workspace ? digests[workspace.id] : undefined;

    return [
      ...suggestedShipRow({
        suggestion,
        workspaceId,
        onRun: (shortcut) => {
          if (workspaceId)
            codeUi.requestWorkflowShortcut(workspaceId, shortcut);
        },
      }),
      ...workspacePaletteRows({
        workspaces: arranged.flatMap((group) => group.workspaces),
        repos,
        digests,
        activeWorkspaceId: workspaceId,
        onOpen: (id) => go(`/code/w/${id}`),
      }),
      ...(workspace
        ? workspaceActionPaletteRows({
            workspace,
            hasPr: Boolean(workspace.pr),
            hasSession: Boolean(digest) || Boolean(sessions[workspace.id]),
            attentionPinned: digest?.attention.state.type === "manual",
            quickActions: repo?.quick_actions ?? [],
            onCommand: (command) =>
              workspaceRunner.run(command.id, {
                workspace,
                title: workspace.title,
                pr: workspace.pr,
                session: sessions[workspace.id],
                sessionId: digest?.session,
                actionName: command.actionName,
              }),
          })
        : []),
      ...(workspaceId
        ? shipPaletteRows({
            onRun: (shortcut) => {
              codeUi.requestWorkflowShortcut(workspaceId, shortcut);
            },
          })
        : []),
      ...(workspaceId
        ? [findPaletteRow(findInConversation, "Find in this conversation")]
        : []),
      ...codeNavigationPaletteRows({
        navigate: go,
        onNewWorkspace: () => codeUi.startNewWorkspace(repo?.id),
        onQuickOpen: () => codeUi.requestQuickOpen(),
      }),
      ...files.map<PaletteRow>((path) => ({
        id: `file:${path}`,
        section: "files",
        label: path,
        // A path is a place, not a habit worth floating to the top later.
        transient: true,
        onSelect: () => codeUi.requestOpenFilePath(path),
      })),
      ...settings,
      ...app,
    ];
  }, [
    mode,
    managed,
    navigate,
    newChat,
    pathname,
    workspaceId,
    workspaces,
    repos,
    sessions,
    digests,
    railPrefs,
    suggestion,
    chats,
    projects,
    files,
    workspaceRunner,
    theme.mode,
    theme.setMode,
    canShowShortcuts,
    canCheckForUpdates,
    client,
    startRename,
    deleteChat,
    moveChatToProject,
  ]);

  const groups = useMemo(
    () => rankPaletteRows(rows, query, { recents }),
    [rows, query, recents],
  );

  function choose(row: PaletteRow) {
    keepFocusOnClose.current = row.movesFocus === true;
    setOpen(false);
    if (!row.transient) setRecents(rememberPaletteRow(row.id, recents));
    row.onSelect();
  }

  // A hit opens its conversation at the message. The request is raised
  // first: the transcript answers it once its history has loaded, whether
  // the conversation was already open or opens now.
  function chooseMessage(hit: MessageSearchHit) {
    const target = hitTarget(hit);
    setOpen(false);
    if (!target) return;
    useTranscriptRevealStore
      .getState()
      .reveal(target, queryTerms(messages.query || parsed.query));
    if (target.kind === "code" && target.workspaceId) {
      const sameWorkspace = target.workspaceId === workspaceId;
      void navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: target.workspaceId },
        // The open workspace keeps its tabs; another one opens on the session.
        search: sameWorkspace
          ? (previous: Record<string, unknown>) => ({
              ...previous,
              task: target.sessionId,
            })
          : { task: target.sessionId },
      });
      return;
    }
    void navigate({ to: hitRoute(target) as "/" });
  }

  return (
    <>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          withCloseButton={false}
          className="top-1/2 max-w-2xl gap-0 overflow-hidden rounded-xl p-0 shadow-2xl"
          overlayClassName="bg-black/45 backdrop-blur-[1px]"
          onCloseAutoFocus={(event) => {
            if (!keepFocusOnClose.current) return;
            keepFocusOnClose.current = false;
            event.preventDefault();
          }}
        >
          <DialogTitle className="sr-only">Command palette</DialogTitle>
          <DialogDescription className="sr-only">
            Search commands, conversations, messages, files, and settings.
          </DialogDescription>
          <CommandPaletteList
            groups={groups}
            query={query}
            onQueryChange={setQuery}
            onSelect={choose}
            scopeLabel={parsed.scopeLabel}
            mode={mode}
            loading={filesWanted && files.length === 0}
            emptyLabel={
              parsed.query
                ? `Nothing matches “${parsed.query}”.`
                : "Nothing here yet."
            }
            messages={messages}
            onSelectMessage={chooseMessage}
          />
        </DialogContent>
      </Dialog>
      {workspaceRunner.dialogs}
    </>
  );
}

/**
 * Open the notifications popover from outside its bell. The bell lives in
 * the rail, so the rail comes out first when it is folded away.
 */
function showNotifications() {
  const ui = useUiStore.getState();
  if (sidebarUsesOverlay(window.innerWidth)) ui.setSidebarOverlayOpen(true);
  else if (ui.sidebarCollapsed) ui.toggleSidebar();
  ui.requestNotifications();
}

/**
 * The worktree's paths, read only once the reader asks for files.
 *
 * A palette that loaded the tree on every open would pay for a search nobody
 * requested, on a surface whose whole point is that it opens instantly. The
 * `#` scope is the ask, and the result is kept for as long as the palette
 * stays on that workspace.
 */
function useWorkspaceFilePaths(
  client: {
    listCodeWorkspaceTree: (
      id: string,
      options: { limit: number },
    ) => Promise<{ paths: string[] }>;
  },
  workspaceId: string | undefined,
  wanted: boolean,
): string[] {
  const [paths, setPaths] = useState<string[]>([]);
  const [loadedFor, setLoadedFor] = useState<string | null>(null);

  useEffect(() => {
    if (!wanted || !workspaceId || loadedFor === workspaceId) return;
    let cancelled = false;
    void client
      .listCodeWorkspaceTree(workspaceId, { limit: TREE_LIMIT })
      .then((tree) => {
        if (cancelled) return;
        setPaths(tree.paths);
        setLoadedFor(workspaceId);
      })
      .catch(() => {
        // The palette still answers with everything else; a failed tree read
        // is not worth taking the surface down for.
        if (!cancelled) setLoadedFor(workspaceId);
      });
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId, wanted, loadedFor]);

  return loadedFor === workspaceId ? paths : [];
}

/** The conversation a path is showing, when it is showing one. */
function chatIdFromPath(pathname: string): string | undefined {
  return /^\/(?:p\/[^/]+\/)?c\/([^/]+)$/.exec(pathname)?.[1];
}
