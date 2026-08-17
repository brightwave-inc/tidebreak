import { useEffect, useState } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { FolderGit2, MessageSquare, Plus } from "lucide-react";

import { useApp } from "@/AppContext";
import {
  SidebarButton,
  SidebarSectionTitle,
  useSidebarWidth,
} from "@/sidebar/primitives";
import { SidebarFrame } from "@/sidebar/SidebarFrame";
import { AttentionBadge } from "./AttentionBadge";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { connectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { AddRepoPalette } from "./AddRepoPalette";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";

/**
 * The code-mode rail: repos, recent workspaces, and the cheap switch back to
 * chat.
 *
 * Built on the same frame and primitives as the chat rail so the two modes
 * share chrome. It must not touch chat session stores — that separation is
 * what later convergence merges rather than translates.
 */

export function CodeSidebar() {
  const navigate = useNavigate();
  const { client } = useApp();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const isCompact = useSidebarWidth() === "compact";
  const repos = useCodeCatalogStore((state) => state.repos);
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const digests = useCodeUpdatesStore((state) => state.byWorkspace);
  const [newWorkspaceOpen, setNewWorkspaceOpen] = useState(false);
  const [addRepoOpen, setAddRepoOpen] = useState(false);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  useEffect(() => connectCodeUpdates(client), [client]);

  const recent = workspaces
    .filter((workspace) => workspace.status !== "archived")
    .slice(0, 12);

  return (
    <SidebarFrame>
      <div className="flex shrink-0 flex-col gap-0.5">
        <SidebarButton
          aria-label="Chat"
          onClick={() => void navigate({ to: "/" })}
        >
          <MessageSquare />
          <span>Chat</span>
        </SidebarButton>
        <SidebarButton
          aria-current={pathname === "/code" ? "page" : undefined}
          data-active={pathname === "/code" || undefined}
          className="data-[active]:bg-muted"
          onClick={() => void navigate({ to: "/code" })}
        >
          <FolderGit2 />
          <span>Code</span>
        </SidebarButton>
      </div>

      {!isCompact && (
        <div className="flex items-center justify-between px-2">
          <SidebarSectionTitle className="px-0">Repos</SidebarSectionTitle>
          <button
            type="button"
            className="cursor-pointer rounded-md p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
            aria-label="Add repo"
            onClick={() => setAddRepoOpen(true)}
          >
            <Plus size={14} />
          </button>
        </div>
      )}
      {isCompact && (
        <SidebarButton aria-label="Add repo" onClick={() => setAddRepoOpen(true)}>
          <Plus />
          <span>Add repo</span>
        </SidebarButton>
      )}
      <div className="flex shrink-0 flex-col gap-0.5">
        {repos.map((repo) => {
          const active = pathname === `/code/r/${repo.id}`;
          return (
            <SidebarButton
              key={repo.id}
              aria-current={active ? "page" : undefined}
              data-active={active || undefined}
              className="data-[active]:bg-muted"
              onClick={() =>
                void navigate({
                  to: "/code/r/$repoId",
                  params: { repoId: repo.id },
                })
              }
            >
              <FolderGit2 />
              <span>{repo.display_name}</span>
            </SidebarButton>
          );
        })}
        {repos.length === 0 && !isCompact && (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            No repos registered
          </p>
        )}
      </div>

      {!isCompact && (
        <div className="mt-3 flex items-center justify-between px-2">
          <SidebarSectionTitle className="px-0">Workspaces</SidebarSectionTitle>
          <button
            type="button"
            className="cursor-pointer rounded-md p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
            aria-label="New workspace"
            onClick={() => setNewWorkspaceOpen(true)}
          >
            <Plus size={14} />
          </button>
        </div>
      )}
      {isCompact && (
        <SidebarButton
          aria-label="New workspace"
          onClick={() => setNewWorkspaceOpen(true)}
        >
          <Plus />
          <span>New workspace</span>
        </SidebarButton>
      )}
      <div className="flex min-h-0 flex-col gap-0.5">
        {recent.map((workspace) => {
          const active = pathname === `/code/w/${workspace.id}`;
          const digest = digests[workspace.id];
          return (
            <SidebarButton
              key={workspace.id}
              aria-current={active ? "page" : undefined}
              data-active={active || undefined}
              className="data-[active]:bg-muted"
              onClick={() =>
                void navigate({
                  to: "/code/w/$workspaceId",
                  params: { workspaceId: workspace.id },
                })
              }
            >
              <FolderGit2 />
              {/* The digest restates the title on every notice, so a
                  background rename lands here without a catalog refresh. */}
              <span>{digest?.title ?? workspace.title}</span>
              <AttentionBadge attention={digest?.attention} compact />
            </SidebarButton>
          );
        })}
      </div>

      <NewWorkspaceDialog
        open={newWorkspaceOpen}
        onOpenChange={setNewWorkspaceOpen}
        repos={repos}
      />
      <AddRepoPalette open={addRepoOpen} onOpenChange={setAddRepoOpen} />
    </SidebarFrame>
  );
}
