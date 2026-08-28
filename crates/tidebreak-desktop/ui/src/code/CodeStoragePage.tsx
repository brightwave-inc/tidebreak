import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { HardDrive, LoaderCircle, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { useApp } from "@/AppContext";
import { archiveForceKind, HttpError } from "@/api/client";
import type {
  CodeRepoStorageSnapshot,
  CodeStorageSnapshot,
  CodeWorkspaceStorageSnapshot,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes } from "@/lib/formatBytes";
import { friendlyErrorMessage } from "@/lib/utils";
import { useConfirm } from "@/components/ConfirmDialog";
import { RouteFrame } from "@/RouteFrame";
import { CodeSidebar } from "./CodeSidebar";
import { WORKSPACE_STATUS_LABELS } from "./labels";

export function CodeStoragePage() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeStorageBody />
      </div>
    </RouteFrame>
  );
}

function CodeStorageBody() {
  const { client } = useApp();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const [report, setReport] = useState<CodeStorageSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const next = await client.listCodeStorage();
      setReport(next);
      setError(null);
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not load storage."));
    } finally {
      setLoaded(true);
    }
  };

  useEffect(() => {
    void refresh();
  }, [client]);

  const act = async (row: CodeWorkspaceStorageSnapshot) => {
    if (busy || !row.next_action) return;
    setBusy(row.id);
    try {
      if (row.next_action === "archive") {
        const archived = await archiveWorkspace(client, confirm, row.id);
        if (!archived) return;
        toast.success("Workspace archived");
      } else {
        const released = await releaseWorkspace(client, confirm, row.id);
        if (!released) return;
        toast.success("Workspace released");
      }
      await refresh();
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(
          caught,
          row.next_action === "archive"
            ? "Could not archive"
            : "Could not release",
        ),
      );
    } finally {
      setBusy(null);
    }
  };

  const repos = report?.repos ?? [];
  const workspaceCount = repos.reduce(
    (sum, repo) => sum + repo.workspaces.length,
    0,
  );

  return (
    <div className="flex size-full min-h-0 flex-col bg-background">
      {dialog}
      <header className="shrink-0 border-b border-border-subtle px-5 py-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">Storage</h1>
              {loaded && (
                <span className="text-xs text-muted-foreground">
                  {workspaceCount} workspace
                  {workspaceCount === 1 ? "" : "s"}
                </span>
              )}
            </div>
            <p className="mt-0.5 text-sm text-muted-foreground">
              See what each reclaim tier would free, then archive or release.
            </p>
          </div>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => void refresh()}
          >
            <RefreshCw />
            Refresh
          </Button>
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {!loaded ? (
          <StorageSkeleton />
        ) : error ? (
          <div className="m-5 rounded-lg border border-critical-border bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted">
            {error}
          </div>
        ) : repos.length === 0 ? (
          <Empty className="min-h-80">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <HardDrive />
              </EmptyMedia>
              <EmptyTitle>No repositories yet</EmptyTitle>
              <EmptyDescription>
                Register a repository to see how much disk its workspaces use.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <div className="flex flex-col gap-6 px-5 py-4">
            {repos.map((repo) => (
              <RepoStorage
                key={repo.id}
                repo={repo}
                busy={busy}
                onOpen={(id) =>
                  void navigate({
                    to: "/code/w/$workspaceId",
                    params: { workspaceId: id },
                  })
                }
                onAct={(row) => void act(row)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function RepoStorage({
  repo,
  busy,
  onOpen,
  onAct,
}: {
  repo: CodeRepoStorageSnapshot;
  busy: string | null;
  onOpen: (id: string) => void;
  onAct: (row: CodeWorkspaceStorageSnapshot) => void;
}) {
  return (
    <section>
      <div className="mb-2 flex flex-wrap items-baseline justify-between gap-2">
        <h2 className="text-sm font-medium">{repo.display_name}</h2>
        <p className="text-xs text-muted-foreground">
          Clone {formatBytes(repo.clone_bytes)}
          {repo.clone_reclaimable ? " · checkout can be removed" : ""}
        </p>
      </div>
      {repo.workspaces.length === 0 ? (
        <p className="text-xs text-muted-foreground">No workspaces.</p>
      ) : (
        <div
          role="list"
          aria-label={`${repo.display_name} workspaces`}
          className="min-w-[720px]"
        >
          <div className="grid grid-cols-[minmax(220px,1fr)_110px_110px_180px] gap-4 border-b border-border-subtle py-2 text-xs font-medium text-muted-foreground">
            <span>Workspace</span>
            <span>Tier</span>
            <span>On disk</span>
            <span className="text-right">Next reclaim</span>
          </div>
          {repo.workspaces.map((workspace) => (
            <div
              key={workspace.id}
              role="listitem"
              className="grid grid-cols-[minmax(220px,1fr)_110px_110px_180px] gap-4 border-b border-border-subtle py-3"
            >
              <button
                type="button"
                className="min-w-0 cursor-pointer truncate text-left text-sm font-medium hover:text-primary"
                onClick={() => onOpen(workspace.id)}
              >
                {workspace.title}
              </button>
              <span className="flex items-center text-xs text-muted-foreground">
                {WORKSPACE_STATUS_LABELS[workspace.status]}
              </span>
              <span className="flex items-center font-mono text-xs tabular-nums text-muted-foreground">
                {formatBytes(workspace.on_disk_bytes)}
              </span>
              <span className="flex items-center justify-end gap-2">
                {workspace.next_action ? (
                  <Button
                    type="button"
                    size="xs"
                    variant={
                      workspace.next_action === "release"
                        ? "outline"
                        : "default"
                    }
                    disabled={Boolean(busy)}
                    onClick={() => onAct(workspace)}
                  >
                    {busy === workspace.id ? (
                      <LoaderCircle className="animate-spin" />
                    ) : null}
                    {workspace.next_action === "archive"
                      ? "Archive"
                      : "Release"}{" "}
                    {formatBytes(workspace.next_reclaim_bytes)}
                  </Button>
                ) : (
                  <span className="text-xs text-muted-foreground">
                    Bundle {formatBytes(workspace.on_disk_bytes)}
                  </span>
                )}
              </span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function StorageSkeleton() {
  return (
    <div className="p-5" role="status">
      <span className="sr-only">Loading storage</span>
      {Array.from({ length: 4 }, (_, index) => (
        <Skeleton key={index} className="mb-2 h-10 w-full" />
      ))}
    </div>
  );
}

async function archiveWorkspace(
  client: {
    archiveCodeWorkspace: (id: string, force?: boolean) => Promise<unknown>;
  },
  confirm: (options: {
    title: string;
    description: string;
    confirmLabel: string;
    destructive?: boolean;
  }) => Promise<boolean>,
  workspaceId: string,
): Promise<boolean> {
  try {
    await client.archiveCodeWorkspace(workspaceId, false);
    return true;
  } catch (error) {
    if (!archiveForceKind(error)) throw error;
    const forced = await confirm({
      title: "Discard leftover work?",
      description: `${error instanceof Error ? error.message : String(error)} Commit and push from the review sidebar, or discard.`,
      confirmLabel: "Discard and archive",
      destructive: true,
    });
    if (!forced) return false;
    await client.archiveCodeWorkspace(workspaceId, true);
    return true;
  }
}

async function releaseWorkspace(
  client: {
    releaseCodeWorkspace: (id: string, force?: boolean) => Promise<unknown>;
  },
  confirm: (options: {
    title: string;
    description: string;
    confirmLabel: string;
    destructive?: boolean;
  }) => Promise<boolean>,
  workspaceId: string,
): Promise<boolean> {
  const proceed = await confirm({
    title: "Drop this branch?",
    description:
      "Tidebreak keeps a bundle so you can restore the workspace. The branch itself leaves the clone.",
    confirmLabel: "Release",
  });
  if (!proceed) return false;
  try {
    await client.releaseCodeWorkspace(workspaceId, false);
    return true;
  } catch (error) {
    if (!(error instanceof HttpError) || error.kind !== "branch_unmerged") {
      throw error;
    }
    const forced = await confirm({
      title: "Release an unmerged branch?",
      description: error.message,
      confirmLabel: "Bundle and drop",
      destructive: true,
    });
    if (!forced) return false;
    await client.releaseCodeWorkspace(workspaceId, true);
    return true;
  }
}
