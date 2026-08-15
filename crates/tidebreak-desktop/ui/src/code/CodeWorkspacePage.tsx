import { useEffect, useRef, useState } from "react";
import { FolderOpen } from "lucide-react";
import { toast } from "sonner";

import { ARCHIVE_FORCE_KINDS, HttpError, type ApiClient } from "../api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
} from "../api/types";
import { useApp } from "@/AppContext";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ClipboardCopyButton } from "@/ClipboardCopyButton";
import { useConfirm } from "@/components/ConfirmDialog";
import { openExternal } from "@/host";
import { RouteFrame } from "@/RouteFrame";
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { liveCodeSession } from "./parsers";
import { CodeComposer } from "./CodeComposer";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "./CodeSessionRegistry";
import { CodeSidebar } from "./CodeSidebar";
import { CodeTranscript } from "./CodeTranscript";
import {
  fenceReasonText,
  HARNESS_LABELS,
  isHarnessReady,
  LIFECYCLE_LABELS,
} from "./labels";

/**
 * One workspace: header, transcript, composer, and the fence/reap path.
 *
 * The session store lives in the registry so two views of the same session
 * share one socket. This page is the only mounted session view in the
 * walking skeleton.
 */

export function CodeWorkspacePage({ workspaceId }: { workspaceId: string }) {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden">
        <CodeWorkspaceBody workspaceId={workspaceId} />
      </div>
    </RouteFrame>
  );
}

function CodeWorkspaceBody({ workspaceId }: { workspaceId: string }) {
  const { client } = useApp();
  const { confirm, dialog } = useConfirm();
  const catalog = useCodeCatalogStore();
  const [workspace, setWorkspace] = useState<CodeWorkspaceSnapshot | null>(null);
  const [repo, setRepo] = useState<CodeRepoSnapshot | null>(null);
  const [session, setSession] = useState<CodeSessionSnapshot | null>(
    catalog.sessionsByWorkspace[workspaceId] ?? null,
  );
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [next, sessions] = await Promise.all([
          client.getCodeWorkspace(workspaceId),
          client.listCodeWorkspaceSessions(workspaceId),
        ]);
        if (cancelled) return;
        setWorkspace(next);
        const catalogState = useCodeCatalogStore.getState();
        catalogState.upsertWorkspace(next);
        const live = liveCodeSession(sessions);
        if (live) {
          catalogState.rememberSession(live);
          setSession(live);
        } else if (!catalogState.sessionsByWorkspace[workspaceId]) {
          setSession(null);
        }
        const nextRepo = await client.getCodeRepo(next.repo_id);
        if (!cancelled) setRepo(nextRepo);
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not load this workspace"));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId]);

  async function startSession(harness: HarnessKind) {
    setStarting(true);
    try {
      const created = await client.createCodeSession(workspaceId, {
        harness,
        permission_mode: "plan",
      });
      catalog.rememberSession(created);
      setSession(created);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not start a session"));
    } finally {
      setStarting(false);
    }
  }

  async function reap() {
    if (!session) return;
    try {
      const next = await client.reapCodeSession(session.id);
      catalog.rememberSession(next);
      setSession(next);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not reap the session"));
    }
  }

  async function archive() {
    if (!workspace) return;
    const ok = await confirm({
      title: "Archive this workspace?",
      description: "The worktree is removed. Uncommitted work is kept only if you cancel.",
      confirmLabel: "Archive",
      destructive: true,
    });
    if (!ok) return;
    try {
      await archiveWorkspace(client, workspace.id, false);
    } catch (err) {
      if (err instanceof HttpError && err.kind && ARCHIVE_FORCE_KINDS.has(err.kind)) {
        const forced = await confirm({
          title: "Discard uncommitted work?",
          description: err.message,
          confirmLabel: "Discard and archive",
          destructive: true,
        });
        if (!forced) return;
        try {
          await archiveWorkspace(client, workspace.id, true);
        } catch (forceErr) {
          toast.error(friendlyErrorMessage(forceErr, "Could not archive"));
        }
        return;
      }
      toast.error(friendlyErrorMessage(err, "Could not archive"));
    }
  }

  async function archiveWorkspace(
    api: ApiClient,
    id: string,
    force: boolean,
  ) {
    const archived = await api.archiveCodeWorkspace(id, force);
    catalog.upsertWorkspace(archived);
    catalog.forgetWorkspace(id);
    setWorkspace(archived);
    toast.success("Workspace archived");
  }

  const fenced =
    session?.lifecycle === "fenced" || session?.fence_reason !== undefined;
  const readyHarnesses =
    catalog.doctor?.harnesses.filter((entry) => isHarnessReady(entry)) ?? [];

  return (
    <>
      {dialog}
      <header className="flex shrink-0 flex-col gap-2 border-b px-4 py-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="min-w-0">
            <h1 className="truncate text-lg font-medium">
              {workspace?.title ?? "Workspace"}
            </h1>
            <p className="text-muted-foreground truncate text-xs">
              {repo?.display_name ?? workspace?.repo_id} · {workspace?.branch_name}
            </p>
          </div>
          <div className="flex items-center gap-2">
            {session && (
              <Badge
                variant={
                  session.lifecycle === "running"
                    ? "success"
                    : session.lifecycle === "fenced"
                      ? "warning"
                      : "outline"
                }
                size="sm"
              >
                {LIFECYCLE_LABELS[session.lifecycle]}
              </Badge>
            )}
            {workspace && workspace.status !== "archived" && (
              <Button type="button" variant="ghost" size="xs" onClick={() => void archive()}>
                Archive
              </Button>
            )}
          </div>
        </div>
        {workspace && (
          <div className="text-muted-foreground flex flex-wrap items-center gap-2 text-xs">
            <span className="font-mono">{workspace.worktree_path}</span>
            <ClipboardCopyButton
              value={workspace.worktree_path}
              label="Copy worktree path"
              copiedAnnouncement="Copied worktree path"
              failedAnnouncement="Could not copy worktree path"
            />
            <Button
              type="button"
              variant="ghost"
              size="2xs"
              onClick={() => void revealPath(workspace.worktree_path)}
            >
              <FolderOpen />
              Reveal
            </Button>
            {session && (
              <span>
                {HARNESS_LABELS[session.harness_kind]}
                {session.harness_version ? ` ${session.harness_version}` : ""}
              </span>
            )}
          </div>
        )}
      </header>
      {error && <p className="text-critical px-4 py-2 text-sm">{error}</p>}
      {fenced && session?.fence_reason && (
        <div className="border-warning-border bg-warning-background text-warning-foreground mx-4 mt-3 flex flex-col gap-2 rounded-md border px-3 py-2 text-sm">
          <p>{fenceReasonText(session.fence_reason)}</p>
          <Button type="button" size="sm" className="self-start" onClick={() => void reap()}>
            Reap
          </Button>
        </div>
      )}
      {!session && workspace?.status === "active" && (
        <div className="flex flex-col gap-2 px-4 py-6">
          <p className="text-sm">Start a Plan-mode session on this workspace.</p>
          <div className="flex flex-wrap gap-2">
            {readyHarnesses.map((entry) => (
              <Button
                key={entry.kind}
                type="button"
                size="sm"
                disabled={starting}
                onClick={() => void startSession(entry.kind)}
              >
                {HARNESS_LABELS[entry.kind]}
              </Button>
            ))}
          </div>
        </div>
      )}
      {session && (
        <CodeSessionPane
          key={session.id}
          session={session}
          client={client}
          disabled={fenced || workspace?.status !== "active"}
        />
      )}
    </>
  );
}

function CodeSessionPane({
  session,
  client,
  disabled,
}: {
  session: CodeSessionSnapshot;
  client: ApiClient;
  disabled: boolean;
}) {
  const store = useRegisteredCodeSession(session.id, client);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const harnessVersion = store((state) => state.harnessVersion);

  async function send(message: string) {
    store.getState().update((current) => ({
      ...current,
      items: [
        ...current.items,
        {
          kind: "user",
          id: `user-${Date.now()}`,
          turnId: current.activeTurnId,
          text: message,
        },
      ],
    }));
    try {
      await client.submitCodeTurn(session.id, message);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not send that turn"));
    }
  }

  async function interrupt() {
    try {
      await client.interruptCodeSession(session.id);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not interrupt"));
    }
  }

  return (
    <>
      {harnessVersion && harnessVersion !== session.harness_version && (
        <p className="text-muted-foreground px-4 pt-2 text-xs">
          {HARNESS_LABELS[session.harness_kind]} {harnessVersion}
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <CodeTranscript items={items} />
      </div>
      {session.lifecycle !== "ended" && (
        <CodeComposer
          running={busy || session.lifecycle === "running"}
          disabled={disabled}
          permissionMode={session.permission_mode}
          onSend={send}
          onInterrupt={interrupt}
        />
      )}
    </>
  );
}

function useRegisteredCodeSession(
  sessionId: string,
  client: ApiClient,
) {
  const storeRef = useRef<ReturnType<typeof acquireCodeSessionFromClient> | null>(
    null,
  );
  if (storeRef.current === null) {
    storeRef.current = acquireCodeSessionFromClient(sessionId, client);
  }
  useEffect(() => {
    return () => {
      releaseCodeSession(sessionId);
      storeRef.current = null;
    };
  }, [sessionId, client]);
  return storeRef.current;
}

async function revealPath(path: string): Promise<void> {
  const fileUrl = path.startsWith("/") ? `file://${path}` : path;
  if (!(await openExternal(fileUrl).catch(() => false))) {
    toast.message("Copy the path to open it in the Finder.");
  }
}
