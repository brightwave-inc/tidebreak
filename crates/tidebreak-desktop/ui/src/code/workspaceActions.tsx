import { useState, type ReactElement } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { MoreHorizontal } from "lucide-react";
import { toast } from "sonner";

import { archiveForceKind, HttpError, type ApiClient } from "../api/client";
import type {
  CodeActionSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { useApp } from "@/AppContext";
import { copyPlainText } from "@/ClipboardCopyButton";
import { useConfirm, type ConfirmOptions } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { ToolOutputPreview } from "@/ToolOutputPreview";
import { friendlyErrorMessage } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import {
  canOpenInExternalEditor,
  canOpenLocalCodeWorktree,
  codeEditorOpenFailureMessage,
  codeWorktreeOpenFailureMessage,
  openCodeWorktree,
  openInEditor,
} from "./codeWorktreeHost";
import { openInEditorLabel } from "./editorPreference";
import { useManagedPolicy } from "../managedPolicy";
import {
  OPTIMISTIC_WORKSPACE_ID_PREFIX,
  useCodeCatalogStore,
} from "./CodeCatalogStore";
import { useCodeUiStore, type WorkspaceStartupStep } from "./CodeUiStore";
import { nextWorkspaceAfterLeaving, railWorkspaceIds } from "./railNavigation";
import { codeWorkspaceIdFromPath } from "./routes";
import { startFirstSession } from "./startWorkspaceSession";
import {
  UNEFF_STARTUP_HEADING,
  prepareUneffMe,
  tidebreakProductRepo,
  uneffPreparationSteps,
  uneffSessionSettings,
} from "./uneffMe";

/**
 * Workspace commands shared by the card context menu and the workspace
 * header overflow. One list, two surfaces.
 */
export type WorkspaceCommandId =
  | "open"
  | "new-session"
  | "rename"
  | "copy-branch"
  | "open-worktree"
  | "open-in-editor"
  | "copy-worktree"
  | "copy-debug-json"
  | "uneff-me"
  | "open-pr"
  | "toggle-terminal"
  | "pin-attention"
  | "clear-attention"
  /** Handled by the workspace page, which owns the tab the fork opens into. */
  | "fork-agent"
  | "run-quick-action"
  | "archive"
  | "force-archive"
  | "restore"
  | "retry-setup";

export type WorkspaceCommand = {
  id: WorkspaceCommandId;
  label: string;
  destructive?: boolean;
  /** Draw a separator before this item. */
  separated?: boolean;
  /** Set when `id` is `run-quick-action`. */
  actionName?: string;
};

export type WorkspaceCommandContext = {
  workspace: CodeWorkspaceSnapshot;
  title: string;
  pr?: PullRequestDigest;
  session?: CodeSessionSnapshot;
  /** Session id when the snapshot is not loaded — the palette has a digest. */
  sessionId?: string;
  actionName?: string;
};

function contextSessionId(
  context: WorkspaceCommandContext,
): string | undefined {
  return context.session?.id ?? context.sessionId;
}

export function workspaceCommands(input: {
  hasPr: boolean;
  archived: boolean;
  hasSession?: boolean;
  attentionPinned?: boolean;
  canOpenWorktree?: boolean;
  /** This window can start an editor on the machine holding the worktree. */
  canOpenInEditor?: boolean;
  /** The setup script failed, so the workspace has a worktree but no session. */
  setupFailed?: boolean;
}): WorkspaceCommand[] {
  // An archived workspace has no worktree: nothing to open a terminal in,
  // no session to steer. What is left is reading, copying the branch that
  // survived, and bringing it back.
  if (input.archived) {
    return [
      { id: "open", label: "Open workspace" },
      { id: "copy-branch", label: "Copy branch name" },
      { id: "copy-worktree", label: "Copy worktree path" },
      { id: "restore", label: "Restore workspace", separated: true },
    ];
  }
  const items: WorkspaceCommand[] = [
    { id: "open", label: "Open workspace" },
    { id: "new-session", label: "New session" },
    { id: "rename", label: "Rename…" },
    { id: "copy-branch", label: "Copy branch name" },
    worktreePathCommand(input.canOpenWorktree ?? canOpenLocalCodeWorktree()),
  ];
  const editor = editorCommand(input.canOpenInEditor);
  if (editor) items.push(editor);
  // The checkout is there and the branch is cut; only the script fell over.
  // Running it again is the way back, and it is the only command here a
  // setup-failed workspace can currently complete.
  if (input.setupFailed) {
    items.splice(1, 0, { id: "retry-setup", label: "Retry setup" });
  }
  if (input.hasPr) {
    items.push({ id: "open-pr", label: "Open pull request" });
  }
  items.push({ id: "toggle-terminal", label: "Toggle terminal" });
  if (input.hasSession) {
    items.push(
      input.attentionPinned
        ? { id: "clear-attention", label: "Clear attention" }
        : { id: "pin-attention", label: "Pin attention" },
    );
    items.push({ id: "copy-debug-json", label: "Copy debug JSON" });
    items.push({ id: "uneff-me", label: "Uneff me" });
  }
  items.push({
    id: "archive",
    label: "Archive",
    destructive: true,
    separated: true,
  });
  items.push({
    id: "force-archive",
    label: "Force archive (discard changes)",
    destructive: true,
  });
  return items;
}

/** Right-click on a multi-selection. Count is always greater than one. */
export function workspaceBulkCommands(count: number): WorkspaceCommand[] {
  return [
    {
      id: "archive",
      label: `Archive ${count} workspaces`,
    },
    {
      id: "force-archive",
      label: `Force archive ${count} workspaces`,
      destructive: true,
      separated: true,
    },
  ];
}

/**
 * Header overflow: worktree access, rename, pin/clear, repo quick actions,
 * then archive. Opening the workspace and its terminal remain card-only.
 */
export function workspaceHeaderCommands(input: {
  archived: boolean;
  hasSession: boolean;
  attentionPinned: boolean;
  quickActions: readonly { name: string }[];
  canOpenWorktree?: boolean;
  canOpenInEditor?: boolean;
  /** The shown agent has a transcript worth handing to a sibling. */
  canFork?: boolean;
  /** The setup script failed, so the workspace has a worktree but no session. */
  setupFailed?: boolean;
}): WorkspaceCommand[] {
  const items: WorkspaceCommand[] = [
    worktreePathCommand(
      !input.archived && (input.canOpenWorktree ?? canOpenLocalCodeWorktree()),
    ),
    { id: "rename", label: "Rename…" },
  ];
  const editor = input.archived
    ? undefined
    : editorCommand(input.canOpenInEditor);
  if (editor) items.splice(1, 0, editor);
  // A broken setup leads the menu: it is the one command that changes the
  // workspace's state, and everything below it acts on a checkout the script
  // never finished preparing.
  if (input.setupFailed) {
    items.unshift({ id: "retry-setup", label: "Retry setup" });
  }
  if (input.hasSession) {
    items.push(
      input.attentionPinned
        ? { id: "clear-attention", label: "Clear attention" }
        : { id: "pin-attention", label: "Pin attention" },
    );
    if (input.canFork) {
      items.push({ id: "fork-agent", label: "Fork this agent" });
    }
    items.push({ id: "copy-debug-json", label: "Copy debug JSON" });
    items.push({ id: "uneff-me", label: "Uneff me" });
  }
  if (!input.archived) {
    for (const action of input.quickActions) {
      items.push({
        id: "run-quick-action",
        label: `Run: ${action.name}`,
        actionName: action.name,
      });
    }
    items.push({
      id: "archive",
      label: "Archive",
      destructive: true,
      separated: true,
    });
    items.push({
      id: "force-archive",
      label: "Force archive (discard changes)",
      destructive: true,
    });
  } else {
    items.push({ id: "restore", label: "Restore workspace", separated: true });
  }
  return items;
}

function worktreePathCommand(canOpenWorktree: boolean): WorkspaceCommand {
  return canOpenWorktree
    ? { id: "open-worktree", label: "Open worktree folder" }
    : { id: "copy-worktree", label: "Copy worktree path" };
}

/**
 * "Open in Zed", not "Open in editor": the reader picked one, and naming it is
 * the difference between a menu item they trust and one they have to try. A
 * custom command has no name worth showing, so that one stays generic. A window
 * attached to another machine gets no row at all — its editor would open
 * nothing.
 */
function editorCommand(
  canOpenInEditor?: boolean,
): WorkspaceCommand | undefined {
  if (!(canOpenInEditor ?? canOpenInExternalEditor())) return undefined;
  return { id: "open-in-editor", label: openInEditorLabel() };
}

/**
 * Start the reader's editor on one worktree file, and say so plainly when it
 * does not start. Shared by the workspace command and by the file and diff
 * panels, which pass the path and line they are already showing.
 */
export function openWorkspaceFileInEditor(input: {
  workspaceId: string;
  relativePath?: string;
  line?: number;
}): void {
  void openInEditor(input).catch((error) => {
    const notice = externalEditorOpenFailureNotice(error);
    toast.error(notice.title, { description: notice.description });
  });
}

export function WorkspaceOverflowMenu({
  commands,
  onCommand,
  context,
}: {
  commands: readonly WorkspaceCommand[];
  onCommand: (command: WorkspaceCommand) => void;
  context?: { repoName?: string; worktreePath?: string };
}) {
  if (commands.length === 0) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label="Workspace actions"
        >
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {(context?.repoName || context?.worktreePath) && (
          <>
            <div className="flex max-w-72 flex-col gap-0.5 px-2 py-2">
              {context.repoName && (
                <span className="truncate text-xs font-medium">
                  {context.repoName}
                </span>
              )}
              {context.worktreePath && (
                <span
                  className="text-muted-foreground truncate font-mono text-2xs"
                  title={context.worktreePath}
                >
                  {context.worktreePath}
                </span>
              )}
            </div>
            <DropdownMenuSeparator />
          </>
        )}
        {commands.map((command) => (
          <WorkspaceOverflowItem
            key={`${command.id}:${command.actionName ?? ""}`}
            command={command}
            onSelect={() => onCommand(command)}
          />
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function WorkspaceOverflowItem({
  command,
  onSelect,
}: {
  command: WorkspaceCommand;
  onSelect: () => void;
}) {
  return (
    <>
      {command.separated && <DropdownMenuSeparator />}
      <DropdownMenuItem
        variant={command.destructive ? "destructive" : "default"}
        onSelect={onSelect}
      >
        {command.label}
      </DropdownMenuItem>
    </>
  );
}

export async function archiveWorkspaceWithConfirm(options: {
  client: Pick<ApiClient, "archiveCodeWorkspace">;
  workspaceId: string;
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  onOptimisticChange?: (archived: boolean) => void;
}): Promise<CodeWorkspaceSnapshot | null> {
  options.onOptimisticChange?.(true);
  try {
    return await options.client.archiveCodeWorkspace(
      options.workspaceId,
      false,
    );
  } catch (error) {
    if (!archiveForceKind(error)) {
      options.onOptimisticChange?.(false);
      throw error;
    }
    const forced = await options.confirm({
      title: "Discard leftover work?",
      description: `${error instanceof Error ? error.message : String(error)} Commit and push from the review sidebar, or discard.`,
      confirmLabel: "Discard and archive",
      destructive: true,
    });
    if (!forced) {
      options.onOptimisticChange?.(false);
      return null;
    }
    try {
      return await options.client.archiveCodeWorkspace(
        options.workspaceId,
        true,
      );
    } catch (forceError) {
      options.onOptimisticChange?.(false);
      throw forceError;
    }
  }
}

export function useWorkspaceCardCommands(): {
  run: (command: WorkspaceCommandId, context: WorkspaceCommandContext) => void;
  runBulk: (
    command: Extract<WorkspaceCommandId, "archive" | "force-archive">,
    workspaces: readonly CodeWorkspaceSnapshot[],
  ) => void;
  dialogs: ReactElement;
} {
  const { client, models, defaultModelKey } = useApp();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const permissionCeiling = useManagedPolicy().permission_mode_ceiling;
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
  const replaceWorkspace = useCodeCatalogStore(
    (state) => state.replaceWorkspace,
  );
  const removeWorkspace = useCodeCatalogStore((state) => state.removeWorkspace);
  const rememberSession = useCodeCatalogStore((state) => state.rememberSession);
  const forgetWorkspaceSession = useCodeCatalogStore(
    (state) => state.forgetWorkspaceSession,
  );
  const [rename, setRename] = useState<{ id: string; title: string } | null>(
    null,
  );
  const [renameValue, setRenameValue] = useState("");
  const [renaming, setRenaming] = useState(false);
  const [actionOutput, setActionOutput] = useState<CodeActionSnapshot | null>(
    null,
  );

  function openWorkspace(workspaceId: string) {
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
    });
  }

  async function afterArchive(
    archived: CodeWorkspaceSnapshot,
    liveIds: readonly string[],
  ) {
    const viewing = codeWorkspaceIdFromPath(pathname) === archived.id;
    const nextId = viewing
      ? nextWorkspaceAfterLeaving(liveIds, archived.id)
      : null;
    upsertWorkspace(archived);
    forgetWorkspaceSession(archived.id);
    toast.success("Workspace archived");
    if (!viewing) return;
    if (nextId) {
      await navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: nextId },
        replace: true,
      });
      return;
    }
    await navigate({ to: "/code", replace: true });
  }

  async function runArchive(workspace: CodeWorkspaceSnapshot) {
    const liveIds = railWorkspaceIds();
    const onOptimisticChange = (archived: boolean) => {
      upsertWorkspace(
        archived
          ? {
              ...workspace,
              status: "archived",
              archived_at: new Date().toISOString(),
            }
          : workspace,
      );
    };
    try {
      const archived = await archiveWorkspaceWithConfirm({
        client,
        workspaceId: workspace.id,
        confirm,
        onOptimisticChange,
      });
      if (!archived) return;
      await afterArchive(archived, liveIds);
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not archive"));
    }
  }

  /**
   * The deliberate force path. The escalating flow in `runArchive` exists for
   * readers who find out mid-archive that work is dirty; this one is for the
   * reader who already knows and does not want two dialogs about it. One
   * confirmation still stands between the click and the worktree going away.
   */
  async function runBulkArchive(
    workspaces: readonly CodeWorkspaceSnapshot[],
    force: boolean,
  ) {
    const count = workspaces.length;
    if (count === 0) return;
    const ok = await confirm(
      force
        ? {
            title: `Discard changes and archive ${count} workspaces?`,
            description:
              "Uncommitted and unpushed work is lost and a running session is stopped. Tidebreak saves the branch's commits in a bundle, then drops the branch.",
            confirmLabel: "Discard and archive",
            destructive: true,
          }
        : {
            title: `Archive ${count} workspaces?`,
            description:
              "They leave the rail and collect in Archive. Worktrees and branches go away; Tidebreak saves a bundle so restore still rebuilds the work.",
            confirmLabel: "Archive",
            destructive: false,
          },
    );
    if (!ok) return;
    const liveIds = railWorkspaceIds();
    const viewing = codeWorkspaceIdFromPath(pathname);
    const archivedIds = new Set<string>();
    const failed: string[] = [];

    for (const workspace of workspaces) {
      try {
        const archived = await client.archiveCodeWorkspace(workspace.id, force);
        upsertWorkspace(archived);
        forgetWorkspaceSession(workspace.id);
        archivedIds.add(workspace.id);
      } catch {
        failed.push(workspace.title);
      }
    }

    useCodeUiStore.getState().clearWorkspaceSelection();
    if (archivedIds.size > 0) {
      toast.success(
        archivedIds.size === 1
          ? "Workspace archived"
          : `${archivedIds.size} workspaces archived`,
      );
    }
    if (failed.length > 0) {
      toast.error(
        failed.length === 1
          ? `Could not archive ${failed[0]}`
          : `Could not archive ${failed.length} workspaces`,
      );
    }
    if (viewing === undefined || !archivedIds.has(viewing)) return;
    const nextId = nextWorkspaceAfterLeaving(liveIds, viewing, archivedIds);
    if (nextId) {
      await navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: nextId },
        replace: true,
      });
      return;
    }
    await navigate({ to: "/code", replace: true });
  }

  async function runForceArchive(workspace: CodeWorkspaceSnapshot) {
    const liveIds = railWorkspaceIds();
    const ok = await confirm({
      title: "Discard changes and archive?",
      description:
        "Uncommitted and unpushed work is lost and a running session is stopped. Tidebreak saves the branch's commits in a bundle, then drops the branch.",
      confirmLabel: "Discard and archive",
      destructive: true,
    });
    if (!ok) return;
    const optimistic = {
      ...workspace,
      status: "archived" as const,
      archived_at: new Date().toISOString(),
    };
    upsertWorkspace(optimistic);
    try {
      await afterArchive(
        await client.archiveCodeWorkspace(workspace.id, true),
        liveIds,
      );
    } catch (error) {
      upsertWorkspace(workspace);
      toast.error(friendlyErrorMessage(error, "Could not archive"));
    }
  }

  async function submitRename() {
    if (!rename) return;
    const title = renameValue.trim();
    if (title.length === 0) return;
    setRenaming(true);
    try {
      const next = await client.patchCodeWorkspace(rename.id, { title });
      upsertWorkspace(next);
      toast.success("Workspace renamed");
      setRename(null);
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not rename"));
    } finally {
      setRenaming(false);
    }
  }

  /**
   * Turn this session's debug report into a Tidebreak session that files an
   * issue or opens a fix.
   *
   * The source workspace carries the startup handoff while the report is
   * collected. With a Tidebreak checkout connected, a fix workspace is
   * created on it and takes the handoff over as soon as it exists, the same
   * way a workspace created from the dialog does. Without one, the session
   * starts as a new agent right here, and nothing is cloned for the reader.
   * Either way the first turn is posted for them: the agent's opening move is
   * to ask what happened.
   */
  async function runUneffMe(context: WorkspaceCommandContext) {
    const sessionId = contextSessionId(context);
    if (!sessionId) return;
    const sourceId = context.workspace.id;
    if (useCodeUiStore.getState().workspaceStartups[sourceId]) {
      toast.error("Uneff me is already running");
      return;
    }
    const catalog = useCodeCatalogStore.getState();
    let doctor = catalog.doctor;
    if (!doctor) {
      try {
        doctor = await client.getHarnessDoctor();
      } catch {
        doctor = null;
      }
    }
    const settings = uneffSessionSettings({
      doctor,
      sourceHarness: context.session?.harness_kind,
      lastCreate: useCodeUiStore.getState().lastCreate,
      ceiling: permissionCeiling,
    });
    if (!settings) {
      toast.error(
        "No coding engine can start right now. Install or sign in to one, then try again.",
      );
      return;
    }
    const sourceRepo =
      catalog.repos.find((repo) => repo.id === context.workspace.repo_id)
        ?.display_name ?? context.workspace.repo_id;
    const setWorkspaceStartup = useCodeUiStore.getState().setWorkspaceStartup;
    const target = tidebreakProductRepo(catalog.repos)
      ? ("new_workspace" as const)
      : ("this_workspace" as const);
    const startup = {
      harness: settings.harness,
      heading: UNEFF_STARTUP_HEADING,
      target,
    };
    const showPreparation = (preparation: WorkspaceStartupStep[]) =>
      setWorkspaceStartup(sourceId, {
        ...startup,
        hasFirstMessage: true,
        phase: "preparing",
        preparation,
      });
    showPreparation(uneffPreparationSteps({ step: "debug" }));
    // The handoff draws on the source workspace, so the reader has to be on it.
    if (pathname !== `/code/w/${sourceId}`) {
      void navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: sourceId },
      });
    }
    let pendingId: string | null = null;
    let prepared: Awaited<ReturnType<typeof prepareUneffMe>>;
    try {
      prepared = await prepareUneffMe({
        repos: catalog.repos,
        sessionId,
        sourceTitle: context.title,
        sourceBranch: context.workspace.branch_name,
        sourceRepo,
        getDebug: (id) => client.getCodeSessionDebug(id),
        createWorkspace: async (body) => {
          // The rail shows the same optimistic card a dialog create shows.
          pendingId = `${OPTIMISTIC_WORKSPACE_ID_PREFIX}${crypto.randomUUID()}`;
          upsertWorkspace({
            id: pendingId,
            repo_id: body.repo_id,
            title: body.title ?? "Uneff",
            worktree_path: "",
            branch_name: "",
            base_ref: "",
            status: "creating",
            created_at: new Date().toISOString(),
          });
          return client.createCodeWorkspace(body);
        },
        onProgress: (progress) =>
          showPreparation(uneffPreparationSteps(progress)),
      });
    } catch (error) {
      if (pendingId) removeWorkspace(pendingId);
      setWorkspaceStartup(sourceId, null);
      toast.error(friendlyErrorMessage(error, "Could not start Uneff me"));
      return;
    }
    const { workspace, prompt } = prepared;
    const preparation = uneffPreparationSteps({ step: "create" });
    setWorkspaceStartup(sourceId, null);
    if (workspace) {
      if (pendingId) replaceWorkspace(pendingId, workspace);
      else upsertWorkspace(workspace);
      await startFirstSession({
        client,
        workspace,
        settings,
        prompt,
        models,
        defaultModelKey,
        startup: { heading: UNEFF_STARTUP_HEADING, preparation },
        reveal: () =>
          navigate({
            to: "/code/w/$workspaceId",
            params: { workspaceId: workspace.id },
          }),
      });
      return;
    }
    // No checkout: a new agent in this workspace, shown once it exists. The
    // prompt is generated, so a failed start does not leave a hundred
    // kilobytes of JSON in a composer that belongs to another conversation.
    await startFirstSession({
      client,
      workspace: context.workspace,
      settings,
      prompt,
      models,
      defaultModelKey,
      startup: { heading: UNEFF_STARTUP_HEADING, preparation, target },
      holdPromptOnFailure: false,
      onSessionCreated: (session) =>
        void navigate({
          to: "/code/w/$workspaceId",
          params: { workspaceId: sourceId },
          search: (current: Record<string, unknown>) => ({
            ...current,
            task: session.id,
            subagent: undefined,
          }),
        }),
    });
  }

  /**
   * True restore first; when the branch is gone that is impossible, so the
   * fallback offer is a fresh workspace on the same repo. A failed setup
   * script still restored the checkout — surface the error and open the
   * workspace anyway so the reader can see what they got back.
   */
  async function runRestore(workspace: CodeWorkspaceSnapshot) {
    const openRestored = () =>
      navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: workspace.id },
      });
    try {
      const restored = await client.restoreCodeWorkspace(workspace.id);
      upsertWorkspace(restored);
      toast.success("Workspace restored");
      await openRestored();
    } catch (error) {
      if (error instanceof HttpError && error.kind === "branch_missing") {
        const ok = await confirm({
          title: "The saved work is gone",
          description:
            "This workspace's branch was deleted after it was archived, so its work cannot come back. Start a new workspace on the same repo instead?",
          confirmLabel: "New workspace",
        });
        if (ok) {
          useCodeUiStore.getState().startNewWorkspace(workspace.repo_id);
        }
        return;
      }
      if (error instanceof HttpError && error.kind === "setup_failed") {
        toast.error(
          friendlyErrorMessage(error, "Restored, but the setup script failed"),
        );
        const refreshed = await client
          .getCodeWorkspace(workspace.id)
          .catch(() => null);
        if (refreshed) upsertWorkspace(refreshed);
        await openRestored();
        return;
      }
      toast.error(friendlyErrorMessage(error, "Could not restore"));
    }
  }

  /**
   * Run the repo's setup script again on the worktree the workspace already
   * has. A second failure leaves the workspace exactly where it was, so the
   * user can edit the script and try again without losing the checkout.
   */
  async function runRetrySetup(workspace: CodeWorkspaceSnapshot) {
    try {
      const revived = await client.retryCodeWorkspaceSetup(workspace.id);
      upsertWorkspace(revived);
      toast.success("Setup finished");
      await navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: workspace.id },
      });
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "The setup script failed again"));
    }
  }

  function run(
    command: WorkspaceCommandId,
    context: WorkspaceCommandContext,
  ): void {
    switch (command) {
      case "open":
      case "new-session":
        openWorkspace(context.workspace.id);
        return;
      case "rename":
        setRenameValue(context.title);
        setRename({ id: context.workspace.id, title: context.title });
        return;
      case "copy-branch":
        void copyPlainText(context.workspace.branch_name)
          .then(() => toast.success("Branch name copied"))
          .catch(() => toast.error("Could not copy branch name"));
        return;
      case "open-worktree":
        void openCodeWorktree(context.workspace.id).catch((error) => {
          const notice = worktreeOpenFailureNotice(error);
          toast.error(notice.title, {
            description: notice.description,
            action: {
              label: notice.actionLabel,
              onClick: () => {
                void copyPlainText(context.workspace.worktree_path)
                  .then(() => toast.success("Worktree path copied"))
                  .catch(() => toast.error("Could not copy worktree path"));
              },
            },
          });
        });
        return;
      case "open-in-editor":
        openWorkspaceFileInEditor({ workspaceId: context.workspace.id });
        return;
      case "copy-worktree":
        void copyPlainText(context.workspace.worktree_path)
          .then(() => toast.success("Worktree path copied"))
          .catch(() => toast.error("Could not copy worktree path"));
        return;
      case "copy-debug-json": {
        const sessionId = contextSessionId(context);
        if (!sessionId) return;
        void client
          .getCodeSessionDebug(sessionId)
          .then((bundle) => copyPlainText(JSON.stringify(bundle, null, 2)))
          .then(() =>
            toast.success("Debug JSON copied", {
              description:
                "Includes the session, turns, and journal events. Review it before sharing.",
            }),
          )
          .catch((error) =>
            toast.error(
              friendlyErrorMessage(error, "Could not copy debug JSON"),
            ),
          );
        return;
      }
      case "uneff-me": {
        void runUneffMe(context);
        return;
      }
      case "open-pr": {
        const url = context.pr?.url;
        if (!url) {
          toast.error("No pull request URL");
          return;
        }
        void openInBrowser(url);
        return;
      }
      case "toggle-terminal": {
        // The workspace page owns this: a terminal is a tab over a shell the
        // server has to start first. Raising the ask works whether the
        // workspace is already on screen or about to be.
        useCodeUiStore.getState().requestTerminal();
        if (pathname === `/code/w/${context.workspace.id}`) return;
        void navigate({
          to: "/code/w/$workspaceId",
          params: { workspaceId: context.workspace.id },
        });
        return;
      }
      case "pin-attention": {
        if (!context.session) return;
        void client
          .setCodeAttention(context.session.id, { note: "Pinned" })
          .then((next) => {
            rememberSession(next);
            toast.success("Attention pinned");
          })
          .catch((error) =>
            toast.error(friendlyErrorMessage(error, "Could not pin attention")),
          );
        return;
      }
      case "clear-attention": {
        if (!context.session) return;
        void client
          .setCodeAttention(context.session.id, { clear: true })
          .then((next) => {
            rememberSession(next);
            toast.success("Attention cleared");
          })
          .catch((error) =>
            toast.error(
              friendlyErrorMessage(error, "Could not clear attention"),
            ),
          );
        return;
      }
      case "run-quick-action": {
        const name = context.actionName;
        if (!name) return;
        void client
          .runCodeWorkspaceAction(context.workspace.id, name)
          .then((result) => {
            const detail = quickActionToast(result);
            const show =
              result.success && !result.timed_out ? toast.success : toast.error;
            show(detail, {
              action: {
                label: "View output",
                onClick: () => setActionOutput(result),
              },
            });
          })
          .catch((error) =>
            toast.error(
              friendlyErrorMessage(error, "Could not run that action"),
            ),
          );
        return;
      }
      case "archive":
        void runArchive(context.workspace);
        return;
      case "force-archive":
        void runForceArchive(context.workspace);
        return;
      case "restore":
        void runRestore(context.workspace);
        return;
      case "retry-setup":
        void runRetrySetup(context.workspace);
        return;
    }
  }

  function runBulk(
    command: Extract<WorkspaceCommandId, "archive" | "force-archive">,
    workspaces: readonly CodeWorkspaceSnapshot[],
  ): void {
    void runBulkArchive(workspaces, command === "force-archive");
  }

  const renameDialog = (
    <Dialog
      open={rename !== null}
      onOpenChange={(open) => {
        if (!open) setRename(null);
      }}
    >
      <DialogContent className="max-w-sm" withCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Rename workspace</DialogTitle>
        </DialogHeader>
        <form
          className="flex flex-col gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            void submitRename();
          }}
        >
          <Input
            aria-label="Workspace title"
            value={renameValue}
            onChange={(event) => setRenameValue(event.target.value)}
            autoFocus
          />
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              onClick={() => setRename(null)}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={renaming || renameValue.trim().length === 0}
            >
              Save
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );

  const outputDialog = (
    <Dialog
      open={actionOutput !== null}
      onOpenChange={(open) => {
        if (!open) setActionOutput(null);
      }}
    >
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {actionOutput
              ? `${actionOutput.name} · ${quickActionToast(actionOutput)}`
              : "Action output"}
          </DialogTitle>
        </DialogHeader>
        {actionOutput && (
          <div className="flex flex-col gap-3">
            <ToolOutputPreview text={actionOutput.stdout} label="stdout" />
            <ToolOutputPreview text={actionOutput.stderr} label="stderr" />
          </div>
        )}
      </DialogContent>
    </Dialog>
  );

  return {
    run,
    runBulk,
    dialogs: (
      <>
        {dialog}
        {renameDialog}
        {outputDialog}
      </>
    ),
  };
}

export function worktreeOpenFailureNotice(error: unknown): {
  title: string;
  description: string;
  actionLabel: string;
} {
  return {
    title: "Could not open worktree folder",
    description: codeWorktreeOpenFailureMessage(error),
    actionLabel: "Copy path",
  };
}

export function externalEditorOpenFailureNotice(error: unknown): {
  title: string;
  description: string;
} {
  return {
    title: "Could not open that file in your editor",
    description: codeEditorOpenFailureMessage(error),
  };
}

export function quickActionToast(result: CodeActionSnapshot): string {
  if (result.timed_out) return `${result.name} timed out`;
  if (result.exit_code !== undefined) {
    return `${result.name} exited ${result.exit_code}`;
  }
  return result.success ? `${result.name} finished` : `${result.name} failed`;
}
