import { useState, type ReactElement } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { toast } from "sonner";

import { archiveForceKind, type ApiClient } from "../api/client";
import type { CodeWorkspaceSnapshot, PullRequestDigest } from "../api/types";
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
import { Input } from "@/components/ui/input";
import { friendlyErrorMessage } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import { searchFromLayout } from "@/panel/panelUrl";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { toggleTerminalLayout } from "./codeChrome";
import { useCodeCatalogStore } from "./CodeCatalogStore";

/**
 * Workspace commands shared by the card context menu and, later, the
 * workspace header overflow. One list, two surfaces.
 */
export type WorkspaceCommandId =
  | "open"
  | "new-session"
  | "rename"
  | "copy-branch"
  | "copy-worktree"
  | "open-pr"
  | "toggle-terminal"
  | "archive";

export type WorkspaceCommand = {
  id: WorkspaceCommandId;
  label: string;
  destructive?: boolean;
  /** Draw a separator before this item. */
  separated?: boolean;
};

export type WorkspaceCommandContext = {
  workspace: CodeWorkspaceSnapshot;
  title: string;
  pr?: PullRequestDigest;
};

export function workspaceCommands(input: {
  hasPr: boolean;
  archived: boolean;
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
  const forgetWorkspaceSession = useCodeCatalogStore(
    (state) => state.forgetWorkspaceSession,
  );
  const [rename, setRename] = useState<{ id: string; title: string } | null>(
    null,
  );
  const [renameValue, setRenameValue] = useState("");
  const [renaming, setRenaming] = useState(false);

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

  return {
    run,
    dialogs: (
      <>
        {dialog}
        {renameDialog}
      </>
    ),
  };
}
