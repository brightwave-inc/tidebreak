import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import { cn } from "@/lib/utils";
import { RouteFrame } from "@/RouteFrame";
import { AttentionBadge } from "./AttentionBadge";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeSidebar } from "./CodeSidebar";
import { useCodeUiStore } from "./CodeUiStore";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { WORKSPACE_STATUS_LABELS } from "./labels";
import { middleTruncate } from "./workspaceCards";

/**
 * One registered repo: its workspaces and the new-workspace flow.
 */

export function CodeRepoPage({ repoId }: { repoId: string }) {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
        <CodeRepoBody repoId={repoId} />
      </div>
    </RouteFrame>
  );
}

function CodeRepoBody({ repoId }: { repoId: string }) {
  const navigate = useNavigate();
  const { client } = useApp();
  const repos = useCodeCatalogStore((state) => state.repos);
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const digests = useCodeUpdatesStore((state) => state.byWorkspace);
  // The dialog itself is mounted once, by the rail this page renders beside
  // it, so that Cmd+N and the buttons all drive the same one.
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  const repo = repos.find((item) => item.id === repoId);
  const listed = workspaces.filter((workspace) => workspace.repo_id === repoId);

  if (!repo) {
    return (
      <p className="text-muted-foreground p-6 text-sm">Loading repo…</p>
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-6 py-8">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="truncate text-2xl font-medium tracking-tight" title={repo.display_name}>
            {repo.display_name}
          </h1>
          <p
            className="text-muted-foreground truncate font-mono text-xs"
            title={repo.root_path}
          >
            {middleTruncate(repo.root_path, 72)}
          </p>
          <p className="text-muted-foreground text-xs">
            Default base {repo.default_base_ref}
          </p>
        </div>
        <Button
          type="button"
          size="sm"
          className="shrink-0"
          onClick={() => startNewWorkspace(repoId)}
        >
          New workspace
        </Button>
      </div>
      {listed.length === 0 ? (
        // The action is already in this page's header, a few centimetres away,
        // so the zero state explains what a workspace is instead of repeating
        // the button.
        <Empty>
          <EmptyHeader>
            <EmptyTitle>No workspaces yet</EmptyTitle>
            <EmptyDescription>
              A workspace is an isolated worktree on {repo.display_name}, with
              its own branch and its own coding session.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <ul className="flex flex-col gap-1">
          {listed.map((workspace) => (
            <li key={workspace.id}>
              <button
                type="button"
                className={cn(
                  "hover:bg-muted flex w-full cursor-pointer items-center justify-between gap-3 rounded-md px-3 py-2 text-left text-sm",
                  FOCUS_RING,
                  HOVER_TINT,
                )}
                onClick={() =>
                  void navigate({
                    to: "/code/w/$workspaceId",
                    params: { workspaceId: workspace.id },
                  })
                }
              >
                <span className="flex min-w-0 items-center gap-2">
                  <span
                    className="min-w-0 shrink truncate font-medium"
                    title={digests[workspace.id]?.title ?? workspace.title}
                  >
                    {digests[workspace.id]?.title ?? workspace.title}
                  </span>
                  <span
                    className="text-muted-foreground shrink-0 font-mono text-xs"
                    title={workspace.branch_name}
                  >
                    {middleTruncate(workspace.branch_name, 32)}
                  </span>
                  <AttentionBadge
                    attention={digests[workspace.id]?.attention}
                    compact
                  />
                </span>
                <span className="text-muted-foreground flex shrink-0 items-center gap-2 text-xs">
                  {digests[workspace.id]?.pr_state && (
                    <span>PR #{digests[workspace.id]?.pr_state?.number}</span>
                  )}
                  {WORKSPACE_STATUS_LABELS[workspace.status]}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
