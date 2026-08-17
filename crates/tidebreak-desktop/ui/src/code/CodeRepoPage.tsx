import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import { RouteFrame } from "@/RouteFrame";
import { AttentionBadge } from "./AttentionBadge";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeSidebar } from "./CodeSidebar";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";
import { WORKSPACE_STATUS_LABELS } from "./labels";

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
  const [open, setOpen] = useState(false);

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
        <div>
          <h1 className="text-2xl font-medium tracking-tight">{repo.display_name}</h1>
          <p className="text-muted-foreground font-mono text-xs">{repo.root_path}</p>
          <p className="text-muted-foreground text-xs">
            Default base {repo.default_base_ref}
          </p>
        </div>
        <Button type="button" size="sm" onClick={() => setOpen(true)}>
          New workspace
        </Button>
      </div>
      <ul className="flex flex-col gap-1">
        {listed.length === 0 && (
          <li className="text-muted-foreground text-sm">No workspaces yet.</li>
        )}
        {listed.map((workspace) => (
          <li key={workspace.id}>
            <button
              type="button"
              className="hover:bg-muted flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm"
              onClick={() =>
                void navigate({
                  to: "/code/w/$workspaceId",
                  params: { workspaceId: workspace.id },
                })
              }
            >
              <span className="flex min-w-0 items-center gap-2">
                <span className="font-medium">
                  {digests[workspace.id]?.title ?? workspace.title}
                </span>
                <span className="text-muted-foreground font-mono text-xs">
                  {workspace.branch_name}
                </span>
                <AttentionBadge attention={digests[workspace.id]?.attention} compact />
              </span>
              <span className="text-muted-foreground flex items-center gap-2 text-xs">
                {digests[workspace.id]?.pr_state && (
                  <span>PR #{digests[workspace.id]?.pr_state?.number}</span>
                )}
                {WORKSPACE_STATUS_LABELS[workspace.status]}
              </span>
            </button>
          </li>
        ))}
      </ul>
      <NewWorkspaceDialog
        open={open}
        onOpenChange={setOpen}
        repos={repos}
        defaultRepoId={repoId}
      />
    </div>
  );
}
