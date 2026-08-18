import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowDown, SquareTerminal } from "lucide-react";
import { toast } from "sonner";

import { archiveForceKind, type ApiClient } from "../api/client";
import type {
  CodeApprovalSnapshot,
  CodePermissionMode,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  ModelInfo,
} from "../api/types";
import { useApp } from "@/AppContext";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ConfirmDialog";
import { WithTooltip } from "@/components/ui/tooltip";
import { PanelLayout } from "@/panel/PanelLayout";
import type { PanelContent } from "@/panel/panelTypes";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { RouteFrame } from "@/RouteFrame";
import { followScrollBehavior } from "@/ChatScroll";
import { useStreamStalled } from "@/useStreamStalled";
import { useTranscriptFollow } from "@/useTranscriptFollow";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import {
  SHELL_SHORTCUTS,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutAction,
} from "@/ShellShortcuts";
import { AttentionBadge } from "./AttentionBadge";
import {
  closeCodeChromeTab,
  focusCodeChromeTab,
  splitCodeChromeLayout,
  toggleTerminalLayout,
} from "./codeChrome";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeInspector } from "./CodeInspector";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { liveCodeSession } from "./parsers";
import { CodeComposer } from "./CodeComposer";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "./CodeSessionRegistry";
import { submitAcceptedTurn } from "./CodeSessionSend";
import { CodeSidebar } from "./CodeSidebar";
import { CodeTranscript } from "./CodeTranscript";
import { StartSessionPrompt } from "./StartSessionPrompt";
import { TerminalDrawer } from "./TerminalDrawer";
import { TerminalPane } from "./TerminalPane";
import { useCodeContentRevision } from "./useLiveContent";
import {
  createPermissionModes,
  fenceReasonText,
  gatewayCodeModels,
  harnessCodeModels,
  harnessHonorsTurnModel,
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
  const { client, models, defaultModelKey } = useApp();
  const { confirm, dialog } = useConfirm();
  const navigate = useNavigate();
  const catalog = useCodeCatalogStore();
  const layout = useLayoutState();
  const { setLayout } = usePanelNav();
  const chrome = splitCodeChromeLayout(layout);
  const reviewSidebarOpen = useCodeUiStore((state) => state.reviewSidebarOpen);
  const shortcutHints = useCodeShortcutHints();
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
  const contentRevision = useCodeContentRevision(session?.id ?? null, client);

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

  async function startSession(
    harness: HarnessKind,
    permissionMode: CodePermissionMode,
    message: string,
    model?: string,
  ) {
    setStarting(true);
    try {
      const gateway = gatewayCodeModels(models, harness, defaultModelKey);
      const listed =
        gateway.length > 0
          ? gateway
          : await catalog.ensureHarnessModels(client, harness);
      const posted =
        model ?? listed.find((option) => option.default)?.id ?? listed[0]?.id;
      const created = await client.createCodeSession(workspaceId, {
        harness,
        permission_mode: permissionMode,
        model: posted,
      });
      catalog.rememberSession(created);
      setSession(created);
      await client.submitCodeTurn(created.id, message);
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
        "The worktree is removed. Commit, push, or create a pull request from the review sidebar first if you want to keep the work.",
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
          description: `${err instanceof Error ? err.message : String(err)} Commit and push from the review sidebar, or discard.`,
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
    // The archived row stays in the catalog — the rail filters archived
    // workspaces out and the repo page lists them with their status — but the
    // session it held is gone with the worktree.
    catalog.upsertWorkspace(archived);
    catalog.forgetWorkspaceSession(id);
    setWorkspace(archived);
    toast.success("Workspace archived");
    // This page is addressed by the workspace that was just archived, so
    // staying here leaves the reader on a dead worktree. The repo lists it
    // again, archived and labelled.
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

  const fenced =
    session?.lifecycle === "fenced" || session?.fence_reason !== undefined;
  const doctorHarnesses = catalog.doctor?.harnesses ?? [];

  return (
    <>
      {dialog}
      <header className="flex h-12 shrink-0 items-center justify-between gap-3 border-b px-4">
        <div className="min-w-0">
          <h1 className="flex min-w-0 items-baseline gap-2 text-sm font-medium">
            <span className="truncate">
              {digest?.title ?? workspace?.title ?? "Workspace"}
            </span>
            <span className="text-muted-foreground truncate text-xs font-normal">
              {repo?.display_name ?? workspace?.repo_id}
            </span>
          </h1>
        </div>
        <div className="flex items-center gap-2">
          {session && (
            <>
              <AttentionBadge
                compact
                attention={digest?.attention ?? session.attention}
              />
              <PendingApprovalBadge sessionId={session.id} client={client} />
              <span className="text-muted-foreground text-xs">
                {LIFECYCLE_LABELS[digest?.lifecycle ?? session.lifecycle]}
                {" · "}
                {HARNESS_LABELS[session.harness_kind]}
              </span>
            </>
          )}
          <WithTooltip
            label={
              chrome.terminal
                ? `Hide terminal ${shortcutHints.terminal}`
                : `Terminal ${shortcutHints.terminal}`
            }
          >
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-pressed={chrome.terminal !== null}
              aria-label="Terminal"
              onClick={() => setLayout(toggleTerminalLayout(layout))}
            >
              <SquareTerminal />
            </Button>
          </WithTooltip>
          {workspace && workspace.status !== "archived" && (
            <Button type="button" variant="ghost" size="xs" onClick={() => void archive()}>
              Archive
            </Button>
          )}
        </div>
      </header>
      {error && <p className="text-critical px-4 py-2 text-sm">{error}</p>}
      <div className="flex min-h-0 flex-1 overflow-hidden">
        <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          <PanelLayout
            layout={chrome.panels}
            framed={false}
            onFocusTab={(index) => setLayout(focusCodeChromeTab(layout, index))}
            onCloseTab={(index) => setLayout(closeCodeChromeTab(layout, index))}
            renderChat={(visible) => (
              // The panel slot this sits in is a plain block, so nothing stretches
              // the pane to the slot's height — `flex-1` resolves to nothing and
              // the transcript never becomes a scroller. It claims the height
              // itself, the same way `.chat-pane` does.
              <div
                className="flex h-full min-h-0 flex-col overflow-hidden"
                hidden={!visible}
              >
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
                    client={client}
                    catalogModels={models}
                    defaultModelKey={defaultModelKey}
                    onStart={(harness, mode, message, model) =>
                      startSession(harness, mode, message, model)
                    }
                  />
                )}
                {session && (
                  <CodeSessionPane
                    key={session.id}
                    session={session}
                    client={client}
                    catalogModels={models}
                    defaultModelKey={defaultModelKey}
                    disabled={fenced || workspace?.status !== "active"}
                  />
                )}
              </div>
            )}
            renderPanel={(panel) =>
              renderCodePanel(panel, client, workspaceId)
            }
          />
          {chrome.terminal && (
            <TerminalDrawer
              client={client}
              workspaceId={workspaceId}
              shortcutHint={shortcutHints.terminal}
              onClose={() => setLayout(toggleTerminalLayout(layout))}
            />
          )}
        </div>
        {reviewSidebarOpen && (
          <CodeInspector
            client={client}
            workspaceId={workspaceId}
            workspace={workspace}
            contentRevision={contentRevision}
          />
        )}
      </div>
    </>
  );
}

function useCodeShortcutHints(): { terminal: string } {
  return useMemo(() => {
    const command = usesCommandModifier(navigator.userAgent);
    return {
      terminal: shortcutHint("toggle-code-terminal", command),
    };
  }, []);
}

function shortcutHint(id: ShellShortcutAction, command: boolean): string {
  const def = SHELL_SHORTCUTS.find((item) => item.id === id);
  return def ? shortcutKeycaps(def, command).join("") : "";
}

function renderCodePanel(
  panel: PanelContent,
  client: ApiClient,
  workspaceId: string,
) {
  if (panel.type === "terminal") {
    return <TerminalPane client={client} workspaceId={workspaceId} />;
  }
  return (
    <p className="text-muted-foreground px-3 py-6 text-sm">
      This panel is not available here.
    </p>
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
    <button
      type="button"
      data-testid="pending-approval-badge"
      className="cursor-pointer rounded-full border-0 bg-transparent p-0"
      onClick={() => {
        document
          .querySelector('[data-code-approval-state="pending"]')
          ?.scrollIntoView({
            block: "nearest",
            behavior: followScrollBehavior(false),
          });
      }}
    >
      <Badge variant="warning" size="sm">
        {pending} {pending === 1 ? "approval" : "approvals"}
      </Badge>
    </button>
  );
}

/**
 * The engine dropped part of its own stream. Saying so where the session is
 * read is the point of counting it at all — a silently degraded transcript
 * looks exactly like a complete one (decision 0031). The count comes from the
 * session row, so it settles at the end of a turn rather than mid-stream.
 */
function CodeSessionPane({
  session,
  client,
  catalogModels,
  defaultModelKey,
  disabled,
  onOpenTurnDiff,
}: {
  session: CodeSessionSnapshot;
  client: ApiClient;
  catalogModels: ModelInfo[];
  defaultModelKey: string | null;
  disabled: boolean;
  /**
   * Scope the review sidebar to one turn's changes, from a turn's diffstat.
   * Left unset until the inspector can hold a scope; the diffstat reads as a
   * plain count meanwhile rather than a control that does nothing.
   */
  onOpenTurnDiff?: (turnId: string) => void;
}) {
  const follow = useTranscriptFollow();
  const store = useRegisteredCodeSession(session.id, client);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const hydrated = store((state) => state.hydrated);
  const animateStreaming = store((state) => state.animateStreaming);
  // The reducer's own applied-event cursor is the activity signal the stall
  // timer wants: every delta, tool result, and boundary advances it.
  const lastSeq = store((state) => state.lastSeq);
  const streamStalled = useStreamStalled(busy, lastSeq);
  const lifecycle = store((state) => state.lifecycle) ?? session.lifecycle;
  const [approvals, setApprovals] = useState<Record<string, CodeApprovalSnapshot>>(
    {},
  );
  const [decidingId, setDecidingId] = useState<string | null>(null);
  const [approvalError, setApprovalError] = useState<string | undefined>();
  // No `?? []` fallback here: a fresh array is a new snapshot every render,
  // and zustand v5 loops on referentially unstable snapshots.
  const cachedModels = useCodeCatalogStore(
    (state) => state.modelsByHarness[session.harness_kind],
  );
  const rememberHarnessModels = useCodeCatalogStore(
    (state) => state.rememberHarnessModels,
  );
  const modelOptions = useMemo(() => {
    const gateway = gatewayCodeModels(
      catalogModels,
      session.harness_kind,
      defaultModelKey,
    );
    return gateway.length > 0 ? gateway : (cachedModels ?? []);
  }, [cachedModels, catalogModels, defaultModelKey, session.harness_kind]);
  const inferred = modelOptions.find((option) => option.default)?.id;
  const [model, setModel] = useState(session.model ?? inferred);

  useEffect(() => {
    setModel(session.model ?? inferred);
  }, [inferred, session.model]);

  useEffect(() => {
    if (gatewayCodeModels(catalogModels, session.harness_kind, defaultModelKey).length > 0) {
      return;
    }
    // An empty list is a finished fetch: this engine advertised no models.
    // Treating [] as "not yet loaded" remembers a new [] forever.
    if (cachedModels !== undefined) return;
    let cancelled = false;
    void client.listCodeHarnessModels(session.harness_kind).then(
      (listed) => {
        if (cancelled) return;
        rememberHarnessModels(
          session.harness_kind,
          harnessCodeModels(listed.models, session.harness_kind),
        );
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [
    cachedModels,
    catalogModels,
    client,
    defaultModelKey,
    rememberHarnessModels,
    session.harness_kind,
  ]);
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
    : ["plan", "ask", "auto", "allow"];

  // `items` is a fresh array on every streamed delta, so keying the fetch on it
  // would list approvals again for every token of a turn. Only an approval
  // appearing or changing state can change what the list would return.
  const approvalKey = useMemo(
    () =>
      items
        .filter((item) => item.kind === "approval")
        .map((item) => `${item.approvalId}:${item.state}`)
        .join(","),
    [items],
  );

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
  }, [client, session.id, approvalKey]);

  function send(message: string) {
    // Sending is a deliberate return to the tail: whatever the reader was
    // reading, they now want to watch their own turn run.
    follow.armFollow();
    follow.requestSmoothFollow();
    // Outcome and refusal both belong to the composer: it says whether the
    // message ran or queued, and it holds the draft when the server refuses.
    return submitAcceptedTurn(store.getState().update, () =>
      client.submitCodeTurn(session.id, message, model ?? undefined),
    );
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
      <div className={cn("message-view", follow.fadeClass)}>
        <CodeTranscript
          items={items}
          hydrated={hydrated}
          busy={busy}
          streamStalled={streamStalled}
          animateStreaming={animateStreaming}
          approvals={approvals}
          decidingId={decidingId}
          approvalError={approvalError}
          onOpenTurnDiff={onOpenTurnDiff}
          scrollRef={follow.scrollRef}
          contentRef={follow.contentRef}
          onScroll={follow.onScroll}
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
        <button
          type="button"
          className={cn(
            "border-border text-foreground bg-background pointer-events-none absolute bottom-3 left-1/2 z-[1] inline-flex -translate-x-1/2 items-center justify-center rounded-full border p-2 opacity-0 shadow transition-[opacity,background-color] duration-150 ease-in-out hover:bg-accent motion-reduce:transition-none",
            follow.scrolledAway && "pointer-events-auto opacity-100",
          )}
          aria-label="Scroll to latest"
          aria-hidden={!follow.scrolledAway}
          tabIndex={follow.scrolledAway ? 0 : -1}
          onClick={() => follow.armFollow(followScrollBehavior(false))}
        >
          <ArrowDown size={16} />
        </button>
      </div>
      {lifecycle !== "ended" && (
        <CodeComposer
          running={busy || lifecycle === "running"}
          disabled={disabled}
          permissionMode={session.permission_mode}
          availableModes={availableModes}
          harness={session.harness_kind}
          model={model ?? undefined}
          modelOptions={modelOptions}
          sessionId={session.id}
          onModelChange={
            harnessHonorsTurnModel(session.harness_kind) ? setModel : undefined
          }
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


