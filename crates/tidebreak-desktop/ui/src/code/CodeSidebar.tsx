import { useEffect } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { FolderGit2, GitBranch, MessageSquare, Plus } from "lucide-react";

import { useApp } from "@/AppContext";
import { cn } from "@/lib/utils";
import {
  SidebarButton,
  SidebarSectionTitle,
  useSidebarWidth,
} from "@/sidebar/primitives";
import { SidebarFrame } from "@/sidebar/SidebarFrame";
import type {
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { AttentionBadge } from "./AttentionBadge";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { connectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { AddRepoPalette } from "./AddRepoPalette";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";
import {
  PR_CHIP_TONE_CLASSES,
  groupWorkspacesByRepo,
  prTone,
  workspaceStateLabel,
} from "./workspaceCards";

/**
 * The code-mode rail: repos, workspace cards grouped by repo, and the cheap
 * switch back to chat.
 *
 * Built on the same frame and primitives as the chat rail so the two modes
 * share chrome. It must not touch chat session stores — that separation is
 * what later convergence merges rather than translates.
 *
 * It also hosts code mode's two dialogs. The rail is on screen for every
 * `/code` route, so mounting them here is what makes them reachable from the
 * keyboard anywhere in the mode without a second instance per page.
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
  const newWorkspaceOpen = useCodeUiStore((state) => state.newWorkspaceOpen);
  const newWorkspaceRepoId = useCodeUiStore(
    (state) => state.newWorkspaceRepoId,
  );
  const addRepoOpen = useCodeUiStore((state) => state.addRepoOpen);
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);
  const setNewWorkspaceOpen = useCodeUiStore(
    (state) => state.setNewWorkspaceOpen,
  );
  const setAddRepoOpen = useCodeUiStore((state) => state.setAddRepoOpen);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  useEffect(() => connectCodeUpdates(client), [client]);

  const groups = groupWorkspacesByRepo(repos, workspaces);

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
            onClick={() => startNewWorkspace()}
          >
            <Plus size={14} />
          </button>
        </div>
      )}
      {isCompact && (
        <SidebarButton
          aria-label="New workspace"
          onClick={() => startNewWorkspace()}
        >
          <Plus />
          <span>New workspace</span>
        </SidebarButton>
      )}
      <div className="flex min-h-0 flex-col gap-0.5">
        {isCompact &&
          groups
            .flatMap((group) => group.workspaces)
            .map((workspace) => {
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
        {!isCompact &&
          groups.map((group) => (
            <div
              key={group.repo?.id ?? "unknown-repo"}
              className="flex flex-col gap-0.5"
            >
              <div className="mt-1 truncate px-2 pt-1 pb-0.5 text-[11px] font-medium text-muted-foreground/90">
                {group.repo?.display_name ?? "Other repos"}
              </div>
              {group.workspaces.map((workspace) => (
                <WorkspaceCard
                  key={workspace.id}
                  workspace={workspace}
                  digest={digests[workspace.id]}
                  active={pathname === `/code/w/${workspace.id}`}
                  onOpen={() =>
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                    })
                  }
                />
              ))}
            </div>
          ))}
        {!isCompact && groups.length === 0 && (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            No workspaces yet
          </p>
        )}
      </div>

      <NewWorkspaceDialog
        open={newWorkspaceOpen}
        onOpenChange={setNewWorkspaceOpen}
        repos={repos}
        defaultRepoId={newWorkspaceRepoId}
      />
      <AddRepoPalette open={addRepoOpen} onOpenChange={setAddRepoOpen} />
    </SidebarFrame>
  );
}

/**
 * One workspace in the expanded rail: title, branch, and a status row with
 * the session state, the attention badge, and the PR chip. The accessible
 * name stays the title so the row reads like the flat list it replaced.
 */
function WorkspaceCard({
  workspace,
  digest,
  active,
  onOpen,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  active: boolean;
  onOpen: () => void;
}) {
  // The digest restates the title on every notice, so a background rename
  // lands here without a catalog refresh.
  const title = digest?.title ?? workspace.title;
  const stateLabel = workspaceStateLabel(workspace.status, digest?.lifecycle);
  // The catalog snapshot carries the PR too, so the chip survives the gap
  // before the updates socket has restated its digest.
  const pr = digest?.pr_state ?? workspace.pr;
  const hasAttention = digest && digest.attention.state.type !== "working";
  return (
    <button
      type="button"
      aria-label={title}
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className="flex w-full cursor-pointer flex-col gap-0.5 rounded-md px-2 py-1.5 text-left ring-offset-background transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none data-[active]:bg-muted"
      onClick={onOpen}
    >
      <span className="truncate text-sm font-[450]">{title}</span>
      <span className="flex min-w-0 items-center gap-1 font-mono text-[11px] text-muted-foreground">
        <GitBranch className="size-3 shrink-0" aria-hidden />
        <span className="truncate">{workspace.branch_name}</span>
      </span>
      {(stateLabel || hasAttention || pr) && (
        <span className="mt-0.5 flex min-w-0 items-center gap-1.5">
          {stateLabel && (
            <span className="text-[11px] text-muted-foreground">
              {stateLabel}
            </span>
          )}
          <AttentionBadge
            attention={digest?.attention}
            className="min-w-0 overflow-hidden"
          />
          <span className="grow" />
          {pr && <PrChip pr={pr} />}
        </span>
      )}
    </button>
  );
}

/** PR number pill, colored by the host state token. */
function PrChip({ pr }: { pr: PullRequestDigest }) {
  const tone = prTone(pr);
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center rounded-full px-1.5 py-px text-[11px] font-medium",
        PR_CHIP_TONE_CLASSES[tone],
      )}
      title={pr.title ? `#${pr.number} ${pr.title}` : `PR #${pr.number}`}
      data-pr-state={tone}
    >
      #{pr.number}
    </span>
  );
}
