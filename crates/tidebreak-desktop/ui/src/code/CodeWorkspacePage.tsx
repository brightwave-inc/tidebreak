import { useEffect, useRef, useState } from "react";
import { FileCode, Files, FolderOpen, SquareTerminal } from "lucide-react";
import { toast } from "sonner";

import { archiveForceKind, type ApiClient } from "../api/client";
import type {
  CodeApprovalSnapshot,
  CodePermissionMode,
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
import { PanelLayout } from "@/panel/PanelLayout";
import type { PanelContent } from "@/panel/panelTypes";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { RouteFrame } from "@/RouteFrame";
import { friendlyErrorMessage } from "@/lib/utils";
import { AttentionBadge } from "./AttentionBadge";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { liveCodeSession } from "./parsers";
import { CodeComposer } from "./CodeComposer";
import { PrCard } from "./PrCard";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "./CodeSessionRegistry";
import { submitAcceptedTurn } from "./CodeSessionSend";
import { CodeSidebar } from "./CodeSidebar";
import { CodeTranscript } from "./CodeTranscript";
import { DiffPanel } from "./DiffPanel";
import { FilesPanel } from "./FilesPanel";
import { StartSessionPrompt } from "./StartSessionPrompt";
import { TerminalPane } from "./TerminalPane";
import {
  createPermissionModes,
  fenceReasonText,
  HARNESS_LABELS,
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
  const layout = useLayoutState();
  const { openPanel } = usePanelNav();
  const [workspace, setWorkspace] = useState<CodeWorkspaceSnapshot | null>(null);
  const [repo, setRepo] = useState<CodeRepoSnapshot | null>(null);
  const [session, setSession] = useState<CodeSessionSnapshot | null>(
    catalog.sessionsByWorkspace[workspaceId] ?? null,
  );
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [createMode, setCreateMode] = useState<CodePermissionMode | null>(null);
  const digest = useCodeUpdatesStore((state) => state.byWorkspace[workspaceId]);
  const setViewedWorkspace = useCodeUpdatesStore((state) => state.setViewedWorkspace);

  useEffect(() => {
    setViewedWorkspace(workspaceId);
    return () => setViewedWorkspace(null);
  }, [setViewedWorkspace, workspaceId]);

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

  async function startSession(harness: HarnessKind, permissionMode: CodePermissionMode) {
    setStarting(true);
    try {
      const created = await client.createCodeSession(workspaceId, {
        harness,
        permission_mode: permissionMode,
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
      description:
        "The worktree is removed. Commit, push, or create a pull request from the card on this page first if you want to keep the work.",
      confirmLabel: "Archive",
      destructive: true,
    });
    if (!ok) return;
    try {
      await archiveWorkspace(client, workspace.id, false);
    } catch (err) {
      if (archiveForceKind(err)) {
        const forced = await confirm({
          title: "Discard leftover work?",
          description: `${err instanceof Error ? err.message : String(err)} Commit and push from the pull-request card on this page, or discard.`,
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
  const doctorHarnesses = catalog.doctor?.harnesses ?? [];

  return (
    <>
      {dialog}
      <header className="flex shrink-0 flex-col gap-2 border-b px-4 py-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="min-w-0">
            <h1 className="truncate text-lg font-medium">
              {digest?.title ?? workspace?.title ?? "Workspace"}
            </h1>
            <p className="text-muted-foreground truncate text-xs">
              {repo?.display_name ?? workspace?.repo_id} · {workspace?.branch_name}
            </p>
          </div>
          <div className="flex items-center gap-2">
            {session && (
              <>
                <AttentionBadge attention={digest?.attention ?? session.attention} />
                <AttentionPin
                  sessionId={session.id}
                  attention={digest?.attention ?? session.attention}
                  client={client}
                />
                <PendingApprovalBadge sessionId={session.id} client={client} />
                <UnrecognizedEventsBadge session={session} />
                <SessionLifecycleBadge
                  session={session}
                  client={client}
                  lifecycle={digest?.lifecycle ?? session.lifecycle}
                />
              </>
            )}
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => openPanel({ type: "files" })}
            >
              <Files />
              Files
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => openPanel({ type: "diff" })}
            >
              <FileCode />
              Diff
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => openPanel({ type: "terminal" })}
            >
              <SquareTerminal />
              Terminal
            </Button>
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
      <PanelLayout
        layout={layout}
        renderChat={(visible) => (
          <div
            className="flex min-h-0 flex-1 flex-col overflow-hidden"
            hidden={!visible}
          >
            {workspace && workspace.status !== "archived" && (
              <div className="px-4 pt-3">
                <PrCard client={client} workspaceId={workspace.id} />
              </div>
            )}
            {fenced && session?.fence_reason && (
              <div className="border-warning-border bg-warning-background text-warning-foreground mx-4 mt-3 flex flex-col gap-2 rounded-md border px-3 py-2 text-sm">
                <p>{fenceReasonText(session.fence_reason)}</p>
                <Button type="button" size="sm" className="self-start" onClick={() => void reap()}>
                  Reap
                </Button>
              </div>
            )}
            {!session && workspace?.status === "active" && (
              <StartSessionPrompt
                harnesses={doctorHarnesses}
                starting={starting}
                selectedMode={createMode}
                onSelectMode={setCreateMode}
                onStart={(harness, mode) => void startSession(harness, mode)}
              />
            )}
            {session && (
              <CodeSessionPane
                key={session.id}
                session={session}
                client={client}
                disabled={fenced || workspace?.status !== "active"}
                onOpenTurnDiff={(turnId) => openPanel({ type: "diff", turnId })}
              />
            )}
          </div>
        )}
        renderPanel={(panel) =>
          renderCodePanel(panel, client, workspaceId, openPanel)
        }
      />
    </>
  );
}

function renderCodePanel(
  panel: PanelContent,
  client: ApiClient,
  workspaceId: string,
  openPanel: (panel: PanelContent) => void,
) {
  switch (panel.type) {
    case "files":
      return (
        <FilesPanel
          client={client}
          workspaceId={workspaceId}
          turnId={panel.turnId}
          onOpenFile={(file) =>
            openPanel({ type: "diff", turnId: panel.turnId, file })
          }
        />
      );
    case "diff":
      return (
        <DiffPanel
          client={client}
          workspaceId={workspaceId}
          turnId={panel.turnId}
          file={panel.file}
        />
      );
    case "terminal":
      return <TerminalPane client={client} workspaceId={workspaceId} />;
    default:
      return (
        <p className="text-muted-foreground px-3 py-6 text-sm">
          This panel is not available here.
        </p>
      );
  }
}

function AttentionPin({
  sessionId,
  attention,
  client,
}: {
  sessionId: string;
  attention: CodeSessionSnapshot["attention"];
  client: ApiClient;
}) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const remember = useCodeCatalogStore((state) => state.rememberSession);

  async function pin() {
    const next = note.trim();
    if (!next) return;
    setBusy(true);
    try {
      const session = await client.setCodeAttention(sessionId, { note: next });
      remember(session);
      setNote("");
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not pin attention"));
    } finally {
      setBusy(false);
    }
  }

  async function clear() {
    setBusy(true);
    try {
      const session = await client.setCodeAttention(sessionId, { clear: true });
      remember(session);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not clear the pin"));
    } finally {
      setBusy(false);
    }
  }

  if (attention.state.type === "manual") {
    return (
      <Button
        type="button"
        variant="ghost"
        size="xs"
        disabled={busy}
        onClick={() => void clear()}
      >
        Clear pin
      </Button>
    );
  }

  return (
    <form
      className="flex items-center gap-1"
      onSubmit={(event) => {
        event.preventDefault();
        void pin();
      }}
    >
      <input
        className="border-input bg-background h-6 w-36 rounded-md border px-2 text-xs"
        value={note}
        onChange={(event) => setNote(event.target.value)}
        placeholder="Pin a note"
        aria-label="Pin a note"
        disabled={busy}
      />
      <Button type="submit" variant="ghost" size="xs" disabled={busy || !note.trim()}>
        Pin
      </Button>
    </form>
  );
}

function SessionLifecycleBadge({
  session,
  client,
  lifecycle: digestLifecycle,
}: {
  session: CodeSessionSnapshot;
  client: ApiClient;
  lifecycle?: CodeSessionSnapshot["lifecycle"];
}) {
  const store = useRegisteredCodeSession(session.id, client);
  const fromStore = store((state) => state.lifecycle);
  const lifecycle = digestLifecycle ?? fromStore ?? session.lifecycle;
  return (
    <Badge
      variant={
        lifecycle === "running"
          ? "success"
          : lifecycle === "fenced"
            ? "warning"
            : "outline"
      }
      size="sm"
    >
      {LIFECYCLE_LABELS[lifecycle]}
    </Badge>
  );
}

function PendingApprovalBadge({
  sessionId,
  client,
}: {
  sessionId: string;
  client: ApiClient;
}) {
  const store = useRegisteredCodeSession(sessionId, client);
  // The selector must return a primitive: zustand v5 re-renders whenever the
  // snapshot is not referentially stable, so a fresh array here loops forever.
  const pending = store(
    (state) =>
      state.items.filter(
        (item) => item.kind === "approval" && item.state === "pending",
      ).length,
  );
  if (pending === 0) return null;
  return (
    <Badge variant="warning" size="sm" data-testid="pending-approval-badge">
      {pending} {pending === 1 ? "approval" : "approvals"}
    </Badge>
  );
}

/**
 * The engine dropped part of its own stream. Saying so where the session is
 * read is the point of counting it at all — a silently degraded transcript
 * looks exactly like a complete one (decision 0031). The count comes from the
 * session row, so it settles at the end of a turn rather than mid-stream.
 */
function UnrecognizedEventsBadge({
  session,
}: {
  session: CodeSessionSnapshot;
}) {
  const dropped = session.unrecognized_event_count;
  if (dropped <= 0) return null;
  return (
    <Badge
      variant="warning"
      size="sm"
      data-testid="unrecognized-events-badge"
      title={
        `${dropped} engine ${dropped === 1 ? "event" : "events"} could not be read by this ` +
        "build and are missing from the transcript. Check the harness doctor page."
      }
    >
      {dropped} unread {dropped === 1 ? "event" : "events"}
    </Badge>
  );
}

function CodeSessionPane({
  session,
  client,
  disabled,
  onOpenTurnDiff,
}: {
  session: CodeSessionSnapshot;
  client: ApiClient;
  disabled: boolean;
  onOpenTurnDiff: (turnId: string) => void;
}) {
  const store = useRegisteredCodeSession(session.id, client);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const harnessVersion = store((state) => state.harnessVersion);
  const lifecycle = store((state) => state.lifecycle) ?? session.lifecycle;
  const [approvals, setApprovals] = useState<Record<string, CodeApprovalSnapshot>>(
    {},
  );
  const [decidingId, setDecidingId] = useState<string | null>(null);
  const [approvalError, setApprovalError] = useState<string | undefined>();
  const doctorEntry = useCodeCatalogStore(
    (state) =>
      state.doctor?.harnesses.find(
        (entry) => entry.kind === session.harness_kind,
      ) ?? null,
  );
  // Doctor caps decide what this engine's picker offers; without a doctor
  // row yet, show everything and let the server refuse.
  const availableModes: CodePermissionMode[] = doctorEntry
    ? createPermissionModes(doctorEntry.caps)
    : ["plan", "ask", "auto"];

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const rows = await client.listCodeApprovals({ sessionId: session.id });
        if (cancelled) return;
        const next: Record<string, CodeApprovalSnapshot> = {};
        for (const row of rows) next[row.id] = row;
        setApprovals(next);
      } catch {
        // The journal still surfaces the card; the body loads on the next poll.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, session.id, items]);

  async function send(message: string) {
    try {
      await submitAcceptedTurn(store.getState().update, () =>
        client.submitCodeTurn(session.id, message),
      );
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
        <CodeTranscript
          items={items}
          onOpenTurnDiff={onOpenTurnDiff}
          approvals={approvals}
          decidingId={decidingId}
          approvalError={approvalError}
          onDecide={async (approvalId, decision, feedback) => {
            setDecidingId(approvalId);
            setApprovalError(undefined);
            try {
              const next = await client.decideCodeApproval(approvalId, {
                decision,
                feedback,
              });
              setApprovals((current) => ({ ...current, [approvalId]: next }));
            } catch (err) {
              setApprovalError(
                friendlyErrorMessage(err, "Could not record that decision"),
              );
            } finally {
              setDecidingId(null);
            }
          }}
        />
      </div>
      {lifecycle !== "ended" && (
        <CodeComposer
          running={busy || lifecycle === "running"}
          disabled={disabled}
          permissionMode={session.permission_mode}
          availableModes={availableModes}
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
