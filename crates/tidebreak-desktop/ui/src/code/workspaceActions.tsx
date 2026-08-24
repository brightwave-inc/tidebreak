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
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";

/**
 * Workspace commands shared by the card context menu and the workspace
 * header overflow. One list, two surfaces.
 */
export type WorkspaceCommandId =
  | "open"
  | "new-session"
  | "rename"
  | "copy-branch"
  | "copy-worktree"
  | "copy-debug-json"
  | "open-pr"
  | "toggle-terminal"
  | "pin-attention"
  | "clear-attention"
  /** Handled by the workspace page, which owns the tab the fork opens into. */
  | "fork-agent"
  | "run-quick-action"
  | "archive"
  | "force-archive"
  | "restore";

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
  actionName?: string;
};

export function workspaceCommands(input: {
  hasPr: boolean;
  archived: boolean;
  hasSession?: boolean;
  attentionPinned?: boolean;
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
    { id: "copy-worktree", label: "Copy worktree path" },
  ];
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

/**
 * Header overflow: rename, pin/clear, repo quick actions, then archive.
 * Card-only items (open, copy, terminal) stay off this surface.
 */
export function workspaceHeaderCommands(input: {
  archived: boolean;
  hasSession: boolean;
  attentionPinned: boolean;
  quickActions: readonly { name: string }[];
  /** The shown agent has a transcript worth handing to a sibling. */
  canFork?: boolean;
}): WorkspaceCommand[] {
  const items: WorkspaceCommand[] = [{ id: "rename", label: "Rename…" }];
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
}): Promise<CodeWorkspaceSnapshot | null> {
  try {
    return await options.client.archiveCodeWorkspace(
      options.workspaceId,
      false,
    );
  } catch (error) {
    if (!archiveForceKind(error)) throw error;
    const forced = await options.confirm({
      title: "Discard leftover work?",
      description: `${error instanceof Error ? error.message : String(error)} Commit and push from the review sidebar, or discard.`,
      confirmLabel: "Discard and archive",
      destructive: true,
    });
    if (!forced) return null;
    return await options.client.archiveCodeWorkspace(options.workspaceId, true);
  }
}

export function useWorkspaceCardCommands(): {
  run: (command: WorkspaceCommandId, context: WorkspaceCommandContext) => void;
  dialogs: ReactElement;
} {
  const { client } = useApp();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
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

  async function afterArchive(archived: CodeWorkspaceSnapshot) {
    upsertWorkspace(archived);
    forgetWorkspaceSession(archived.id);
    toast.success("Workspace archived");
    if (pathname === `/code/w/${archived.id}`) {
      await navigate({ to: "/code", replace: true });
    }
  }

  async function runArchive(workspace: CodeWorkspaceSnapshot) {
    try {
      const archived = await archiveWorkspaceWithConfirm({
        client,
        workspaceId: workspace.id,
        confirm,
      });
      if (!archived) return;
      await afterArchive(archived);
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
  async function runForceArchive(workspace: CodeWorkspaceSnapshot) {
    const ok = await confirm({
      title: "Discard changes and archive?",
      description:
        "Uncommitted and unpushed work is lost and a running session is stopped. The branch and its commits are kept.",
      confirmLabel: "Discard and archive",
      destructive: true,
    });
    if (!ok) return;
    try {
      await afterArchive(await client.archiveCodeWorkspace(workspace.id, true));
    } catch (error) {
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
          title: "The branch is gone",
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
      case "copy-worktree":
        void copyPlainText(context.workspace.worktree_path)
          .then(() => toast.success("Worktree path copied"))
          .catch(() => toast.error("Could not copy worktree path"));
        return;
      case "copy-debug-json": {
        if (!context.session) return;
        void client
          .getCodeSessionDebug(context.session.id)
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
    }
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
    dialogs: (
      <>
        {dialog}
        {renameDialog}
        {outputDialog}
      </>
    ),
  };
}

export function quickActionToast(result: CodeActionSnapshot): string {
  if (result.timed_out) return `${result.name} timed out`;
  if (result.exit_code !== undefined) {
    return `${result.name} exited ${result.exit_code}`;
  }
  return result.success ? `${result.name} finished` : `${result.name} failed`;
}
