import { useState, type ReactElement } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { MoreHorizontal } from "lucide-react";
import { toast } from "sonner";

import { archiveForceKind, type ApiClient } from "../api/client";
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
import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import { searchFromLayout } from "@/panel/panelUrl";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { toggleTerminalLayout } from "./codeChrome";
import { useCodeCatalogStore } from "./CodeCatalogStore";

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
  | "run-quick-action"
  | "archive";

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
  if (!input.archived) {
    items.push({
      id: "archive",
      label: "Archive",
      destructive: true,
      separated: true,
    });
  }
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
}): WorkspaceCommand[] {
  const items: WorkspaceCommand[] = [{ id: "rename", label: "Rename…" }];
  if (input.hasSession) {
    items.push(
      input.attentionPinned
        ? { id: "clear-attention", label: "Clear attention" }
        : { id: "pin-attention", label: "Pin attention" },
    );
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
  }
  return items;
}

export function WorkspaceOverflowMenu({
  commands,
  onCommand,
}: {
  commands: readonly WorkspaceCommand[];
  onCommand: (command: WorkspaceCommand) => void;
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
  const ok = await options.confirm({
    title: "Archive this workspace?",
    description:
      "The worktree is removed. Commit, push, or create a pull request from the review sidebar first if you want to keep the work.",
    confirmLabel: "Archive",
    destructive: true,
  });
  if (!ok) return null;
  try {
    return await options.client.archiveCodeWorkspace(options.workspaceId, false);
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
  run: (
    command: WorkspaceCommandId,
    context: WorkspaceCommandContext,
  ) => void;
  dialogs: ReactElement;
} {
  const { client } = useApp();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const layout = useLayoutState();
  const { setLayout } = usePanelNav();
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

  async function runArchive(workspace: CodeWorkspaceSnapshot) {
    try {
      const archived = await archiveWorkspaceWithConfirm({
        client,
        workspaceId: workspace.id,
        confirm,
      });
      if (!archived) return;
      upsertWorkspace(archived);
      forgetWorkspaceSession(archived.id);
      toast.success("Workspace archived");
      if (pathname === `/code/w/${archived.id}`) {
        if (archived.repo_id) {
          await navigate({
            to: "/code/r/$repoId",
            params: { repoId: archived.repo_id },
            replace: true,
          });
        } else {
          await navigate({ to: "/code", replace: true });
        }
      }
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
          .then((bundle) =>
            copyPlainText(JSON.stringify(bundle, null, 2)),
          )
          .then(() =>
            toast.success("Debug JSON copied", {
              description:
                "Includes the session, turns, and journal events. Review it before sharing.",
            }),
          )
          .catch((error) =>
            toast.error(friendlyErrorMessage(error, "Could not copy debug JSON")),
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
        if (pathname === `/code/w/${context.workspace.id}`) {
          setLayout(toggleTerminalLayout(layout));
          return;
        }
        void navigate({
          to: "/code/w/$workspaceId",
          params: { workspaceId: context.workspace.id },
          search: searchFromLayout(toggleTerminalLayout(EMPTY_LAYOUT)),
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
            const show = result.success && !result.timed_out ? toast.success : toast.error;
            show(detail, {
              action: {
                label: "View output",
                onClick: () => setActionOutput(result),
              },
            });
          })
          .catch((error) =>
            toast.error(friendlyErrorMessage(error, "Could not run that action")),
          );
        return;
      }
      case "archive":
        void runArchive(context.workspace);
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
