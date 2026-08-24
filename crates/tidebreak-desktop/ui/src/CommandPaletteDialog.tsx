import { useEffect, useMemo, useState } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
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

/** The tree read is bounded the same way the file picker's is. */
const TREE_LIMIT = 5000;

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
export function CommandPaletteDialog() {
  const { client } = useApp();
  const navigate = useNavigate();
  const { managed } = useManagedPolicy();
  const open = useUiStore((state) => state.commandPaletteOpen);
  const setOpen = useUiStore((state) => state.setCommandPaletteOpen);
  const [query, setQuery] = useState("");
  const [recents, setRecents] = useState<string[]>([]);

  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const mode = shellShortcutMode(pathname);
  const workspaceId = codeWorkspaceIdFromPath(pathname);

  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const repos = useCodeCatalogStore((state) => state.repos);
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

  const rows = useMemo<PaletteRow[]>(() => {
    // Settings sections are addressed from a runtime table and never enter the
    // router's generated path union, so the cast happens once, here.
    const go = (path: string) => void navigate({ to: path as "/" });
    const settings = settingsPaletteRows({ managed, navigate: go });
    // Store actions are stable for the store's lifetime, so reading them here
    // keeps the memo from re-running on every unrelated store write.
    const codeUi = useCodeUiStore.getState();

    if (mode === "chat") {
      return [
        ...chatPaletteRows({
          chats,
          projects,
          activeChatId: chatIdFromPath(pathname),
          onOpen: (chat) => go(`/c/${chat.id}`),
        }),
        ...projectPaletteRows({
          projects,
          onOpen: (project) => go(`/p/${project.id}`),
        }),
        ...chatNavigationPaletteRows({
          navigate: go,
          onNewChat: () => go("/"),
        }),
        ...settings,
      ];
    }

    const arranged = arrangeWorkspaces(
      railPrefs.sortMode,
      repos,
      workspaces,
      digests,
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
            hasSession: Boolean(digest),
            attentionPinned: digest?.attention.state.type === "manual",
            quickActions: repo?.quick_actions ?? [],
            onCommand: (command) =>
              workspaceRunner.run(command.id, {
                workspace,
                title: workspace.title,
                pr: workspace.pr,
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
    ];
  }, [
    mode,
    managed,
    navigate,
    pathname,
    workspaceId,
    workspaces,
    repos,
    digests,
    railPrefs,
    suggestion,
    chats,
    projects,
    files,
    workspaceRunner,
  ]);

  const groups = useMemo(
    () => rankPaletteRows(rows, query, { recents }),
    [rows, query, recents],
  );

  function choose(row: PaletteRow) {
    setOpen(false);
    if (!row.transient) setRecents(rememberPaletteRow(row.id, recents));
    row.onSelect();
  }

  return (
    <>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          withCloseButton={false}
          className="top-1/2 max-w-2xl gap-0 overflow-hidden rounded-xl p-0 shadow-2xl"
          overlayClassName="bg-black/45 backdrop-blur-[1px]"
        >
          <DialogTitle className="sr-only">Command palette</DialogTitle>
          <DialogDescription className="sr-only">
            Search commands, workspaces, files, and settings.
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
          />
        </DialogContent>
      </Dialog>
      {workspaceRunner.dialogs}
    </>
  );
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
