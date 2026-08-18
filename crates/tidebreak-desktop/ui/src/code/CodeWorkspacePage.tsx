import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, PanelRight, SquareTerminal } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  Attention,
  CodeApprovalSnapshot,
  CodePermissionMode,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  ModelInfo,
} from "../api/types";
import { useApp } from "@/AppContext";
import { copyPlainText, scheduleCopyStateReset } from "@/ClipboardCopyButton";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import { Skeleton } from "@/components/ui/skeleton";
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
  closeEditorTab,
  focusCodeChromeTab,
  focusConversation,
  focusEditorTab,
  splitCodeChromeLayout,
  toggleTerminalLayout,
} from "./codeChrome";
import { CodeCenterTabs } from "./CodeCenterTabs";
import { DiffPanel } from "./DiffPanel";

const FileViewer = lazy(async () => {
  const module = await import("./FileViewer");
  return { default: module.FileViewer };
});
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
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { StartSessionPrompt } from "./StartSessionPrompt";
import { TerminalDrawer } from "./TerminalDrawer";
import { TerminalPane } from "./TerminalPane";
import { useCodeContentRevision } from "./useLiveContent";
import {
  WorkspaceOverflowMenu,
  useWorkspaceCardCommands,
  workspaceHeaderCommands,
} from "./workspaceActions";
import { middleTruncate } from "./workspaceCards";
import {
  createPermissionModes,
  fenceReasonText,
  gatewayCodeModels,
  harnessCodeModels,
  harnessHonorsTurnModel,
  LIFECYCLE_LABELS,
  sessionLifecycleTooltip,
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
  const catalog = useCodeCatalogStore();
  const { run, dialogs } = useWorkspaceCardCommands();
  const layout = useLayoutState();
  const { setLayout, openPanel } = usePanelNav();
  const chrome = splitCodeChromeLayout(layout);
  const reviewSidebarOpen = useCodeUiStore((state) => state.reviewSidebarOpen);
  const toggleReviewSidebar = useCodeUiStore((state) => state.toggleReviewSidebar);
  const shortcutHints = useCodeShortcutHints();
  const [workspace, setWorkspace] = useState<CodeWorkspaceSnapshot | null>(null);
  const [repo, setRepo] = useState<CodeRepoSnapshot | null>(null);
  const [session, setSession] = useState<CodeSessionSnapshot | null>(
    catalog.sessionsByWorkspace[workspaceId] ?? null,
  );
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);
  const [starting, setStarting] = useState(false);
  const [createMode, setCreateMode] = useState<CodePermissionMode | null>(null);
  const digest = useCodeUpdatesStore((state) => state.byWorkspace[workspaceId]);
  const setViewedWorkspace = useCodeUpdatesStore((state) => state.setViewedWorkspace);
  const contentRevision = useCodeContentRevision(session?.id ?? null, client);
  const rememberedSession = useCodeCatalogStore(
    (state) => state.sessionsByWorkspace[workspaceId] ?? null,
  );

  useEffect(() => {
    setViewedWorkspace(workspaceId);
    return () => setViewedWorkspace(null);
  }, [setViewedWorkspace, workspaceId]);

  useEffect(() => {
    if (rememberedSession) setSession(rememberedSession);
  }, [rememberedSession]);

  useEffect(() => {
    return () => {
      useCodeUiStore.getState().setInspectorScope(null);
    };
  }, [workspaceId]);

  useEffect(() => {
    let cancelled = false;
    setError(null);
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
  }, [client, workspaceId, reloadToken]);

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

  const fenced =
    session?.lifecycle === "fenced" || session?.fence_reason !== undefined;
  const doctorHarnesses = catalog.doctor?.harnesses ?? [];
  const title = digest?.title ?? workspace?.title;
  const repoName = repo?.display_name;
  const headerCommands = workspace
    ? workspaceHeaderCommands({
        archived: workspace.status === "archived",
        hasSession: Boolean(session),
        attentionPinned:
          (digest?.attention ?? session?.attention)?.state.type === "manual",
        quickActions: repo?.quick_actions ?? [],
      })
    : [];

  function openTurnDiff(turnId: string) {
    openPanel({ type: "diff", turnId });
  }

  function openFile(path: string) {
    openPanel({ type: "file", path });
  }

  function openFileDiff(path: string) {
    openPanel({ type: "diff", path });
  }

  const editorTabs = chrome.editors.tabs;
  const showingChat =
    editorTabs.length === 0 || Boolean(chrome.editors.conversationFocused);
  const activeEditor = showingChat
    ? null
    : (editorTabs[chrome.editors.activeIndex] ?? null);

  const workspaceMain = (
    <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      <CodeCenterTabs
        editorTabs={editorTabs}
        editorActiveIndex={chrome.editors.activeIndex}
        conversationFocused={showingChat}
        onSelectChat={() => setLayout(focusConversation(layout))}
        onSelectEditor={(index) => setLayout(focusEditorTab(layout, index))}
        onCloseEditor={(index) => setLayout(closeEditorTab(layout, index))}
      />
      <div className={cn("min-h-0 flex-1", !showingChat && "hidden")}>
      <PanelLayout
        layout={chrome.panels}
        framed={false}
        onFocusTab={(index) => setLayout(focusCodeChromeTab(layout, index))}
        onCloseTab={(index) => setLayout(closeCodeChromeTab(layout, index))}
        renderChat={(visible) => (
          // The panel slot is a plain block. `.chat-pane` claims that height
          // so `.message-view` can grow and the composer stays at the bottom,
          // including on an empty transcript.
          <div className="chat-pane" hidden={!visible || !showingChat}>
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
                workspaceId={workspaceId}
                client={client}
                catalogModels={models}
                defaultModelKey={defaultModelKey}
                disabled={fenced || workspace?.status !== "active"}
                onOpenTurnDiff={openTurnDiff}
              />
            )}
          </div>
        )}
        renderPanel={(panel) =>
          renderCodePanel(panel, client, workspaceId)
        }
      />
      </div>
      {!showingChat && activeEditor && (
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
          {activeEditor.type === "file" ? (
            <Suspense fallback={<Skeleton className="h-full w-full" />}>
              <FileViewer
                client={client}
                workspaceId={workspaceId}
                path={activeEditor.path}
                contentRevision={contentRevision}
              />
            </Suspense>
          ) : activeEditor.type === "diff" ? (
            <DiffPanel
              client={client}
              workspaceId={workspaceId}
              turnId={activeEditor.turnId}
              file={activeEditor.path}
              contentRevision={contentRevision}
              onOpenFile={openFile}
            />
          ) : null}
        </div>
      )}
      {chrome.terminal && (
        <TerminalDrawer
          client={client}
          workspaceId={workspaceId}
          shortcutHint={shortcutHints.terminal}
          onClose={() => setLayout(toggleTerminalLayout(layout))}
        />
      )}
    </div>
  );

  return (
    <>
      {dialogs}
      <header className="flex h-12 shrink-0 items-center justify-between gap-3 border-b px-4">
        <div className="min-w-0">
          <h1 className="flex min-w-0 items-center gap-2 text-sm font-medium">
            {title ? (
              <span className="truncate" title={title}>
                {title}
              </span>
            ) : error ? null : (
              <span
                data-testid="workspace-header-skeleton"
                className="flex min-w-0 items-center gap-2"
              >
                <Skeleton className="h-4 w-28" />
                <Skeleton className="h-3 w-16" />
              </span>
            )}
            {repoName && (
              <span
                className="text-muted-foreground truncate text-xs font-normal"
                title={repoName}
              >
                {repoName}
              </span>
            )}
            {workspace && (
              <WorktreePathChip path={workspace.worktree_path} />
            )}
          </h1>
        </div>
        <div className="flex items-center gap-2">
          {session && (
            <>
              <SessionAttentionBadge
                sessionId={session.id}
                client={client}
                fallback={digest?.attention ?? session.attention}
              />
              <PendingApprovalBadge sessionId={session.id} client={client} />
              <SessionLifecycleMark
                lifecycle={digest?.lifecycle ?? session.lifecycle}
                harness={session.harness_kind}
                version={session.harness_version}
                unrecognizedEventCount={session.unrecognized_event_count}
              />
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
          <WithTooltip
            label={
              reviewSidebarOpen
                ? `Hide review sidebar ${shortcutHints.review}`
                : `Review sidebar ${shortcutHints.review}`
            }
          >
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-pressed={reviewSidebarOpen}
              aria-label="Review sidebar"
              onClick={() => toggleReviewSidebar()}
            >
              <PanelRight />
            </Button>
          </WithTooltip>
          {workspace && (
            <WorkspaceOverflowMenu
              commands={headerCommands}
              onCommand={(command) =>
                run(command.id, {
                  workspace,
                  title: title ?? workspace.title,
                  session: session ?? undefined,
                  actionName: command.actionName,
                })
              }
            />
          )}
        </div>
      </header>
      {error ? (
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 px-4">
          <p className="text-muted-foreground max-w-sm text-center text-sm">
            {error}
          </p>
          <Button
            type="button"
            size="sm"
            onClick={() => setReloadToken((token) => token + 1)}
          >
            Retry
          </Button>
        </div>
      ) : reviewSidebarOpen ? (
        <ResizablePanelGroup
          autoSaveId="code-inspector"
          direction="horizontal"
          className="min-h-0 flex-1"
        >
          <ResizablePanel defaultSize={72} minSize={36} className="h-full min-h-0 min-w-0">
            {workspaceMain}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel defaultSize={28} minSize={18} className="min-w-0">
            <CodeInspector
              key={workspaceId}
              client={client}
              workspaceId={workspaceId}
              workspace={workspace}
              contentRevision={contentRevision}
              onOpenFile={openFile}
              onOpenDiff={openFileDiff}
            />
          </ResizablePanel>
        </ResizablePanelGroup>
      ) : (
        <div className="flex h-full min-h-0 flex-1 overflow-hidden">{workspaceMain}</div>
      )}
    </>
  );
}

function useCodeShortcutHints(): { terminal: string; review: string } {
  return useMemo(() => {
    const command = usesCommandModifier(navigator.userAgent);
    return {
      terminal: shortcutHint("toggle-code-terminal", command),
      review: shortcutHint("toggle-code-review", command),
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

function SessionAttentionBadge({
  sessionId,
  client,
  fallback,
}: {
  sessionId: string;
  client: ApiClient;
  fallback: Attention | undefined;
}) {
  const store = useRegisteredCodeSession(sessionId, client);
  const live = store((state) => state.attention);
  return <AttentionBadge compact attention={live ?? fallback} />;
}

/**
 * The engine dropped part of its own stream. Saying so where the session is
 * read is the point of counting it at all — a silently degraded transcript
 * looks exactly like a complete one (decision 0031). The count comes from the
 * session row, so it settles at the end of a turn rather than mid-stream.
 */
function SessionLifecycleMark({
  lifecycle,
  harness,
  version,
  unrecognizedEventCount,
}: {
  lifecycle: CodeSessionSnapshot["lifecycle"];
  harness: CodeSessionSnapshot["harness_kind"];
  version?: string;
  unrecognizedEventCount: number;
}) {
  const tooltip = sessionLifecycleTooltip({
    lifecycle,
    harness,
    version,
    unrecognizedEventCount,
  });
  return (
    <WithTooltip label={tooltip}>
      <span className="text-muted-foreground inline-flex items-center gap-1.5 text-xs">
        {unrecognizedEventCount > 0 && (
          <span
            data-testid="unrecognized-event-dot"
            className="bg-warning-foreground-muted inline-block size-2 shrink-0 rounded-full"
            aria-label={`${unrecognizedEventCount} unread engine ${unrecognizedEventCount === 1 ? "event" : "events"}`}
          />
        )}
        <span>{LIFECYCLE_LABELS[lifecycle]}</span>
      </span>
    </WithTooltip>
  );
}

function WorktreePathChip({ path }: { path: string }) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );

  useEffect(() => {
    if (copyState === "idle") return;
    return scheduleCopyStateReset(() => setCopyState("idle"));
  }, [copyState]);

  async function onCopy() {
    try {
      await copyPlainText(path);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  const label =
    copyState === "copied"
      ? "Copied"
      : copyState === "failed"
        ? "Copy failed"
        : path;

  return (
    <>
      <WithTooltip label={label}>
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-muted hover:text-foreground max-w-44 shrink cursor-pointer truncate rounded-md px-1.5 py-0.5 font-mono text-[11px]",
            FOCUS_RING,
            HOVER_TINT,
          )}
          aria-label={
            copyState === "idle" ? `Copy worktree path ${path}` : label
          }
          onClick={() => void onCopy()}
        >
          {middleTruncate(path, 24)}
        </button>
      </WithTooltip>
      <span className="sr-only" role="status" aria-live="polite">
        {copyState === "copied"
          ? "Worktree path copied to clipboard."
          : copyState === "failed"
            ? "Worktree path could not be copied."
            : ""}
      </span>
    </>
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
      className={cn(
        "cursor-pointer rounded-full border-0 bg-transparent p-0",
        FOCUS_RING,
      )}
      onClick={() => {
        document
          .querySelector('[data-code-approval-state="pending"]')
          ?.scrollIntoView({
            block: "nearest",
            behavior: followScrollBehavior(false),
          });
      }}
    >
      <Badge variant="warning" size="sm" className="tabular-nums">
        {pending} {pending === 1 ? "approval" : "approvals"}
      </Badge>
    </button>
  );
}

function CodeSessionPane({
  session,
  workspaceId,
  client,
  catalogModels,
  defaultModelKey,
  disabled,
  onOpenTurnDiff,
}: {
  session: CodeSessionSnapshot;
  workspaceId: string;
  client: ApiClient;
  catalogModels: ModelInfo[];
  defaultModelKey: string | null;
  disabled: boolean;
  /** Scope the review sidebar to one turn's changes, from a turn's diffstat. */
  onOpenTurnDiff?: (turnId: string) => void;
}) {
  const follow = useTranscriptFollow();
  const store = useRegisteredCodeSession(session.id, client);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const hydrated = store((state) => state.hydrated);
  const animateStreaming = store((state) => state.animateStreaming);
  const connectionState = store((state) => state.connectionState);
  const lastTurnBeganId = store((state) => state.lastTurnBeganId);
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
  const [queued, setQueued] = useState(false);
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
  const steeringSupported = doctorEntry?.caps.mid_turn_steering === "supported";
  const composerHistory = useMemo(
    () =>
      items
        .flatMap((item) =>
          item.kind === "user" && item.text.trim() ? [item.text] : [],
        )
        .reverse(),
    [items],
  );

  useEffect(() => {
    setQueued(false);
  }, [lastTurnBeganId]);

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
    ).then((outcome) => {
      if (outcome.kind === "queued") setQueued(true);
      return outcome;
    });
  }

  async function steer(message: string) {
    await client.steerCodeSession(session.id, message);
  }

  async function interrupt() {
    try {
      await client.interruptCodeSession(session.id);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not interrupt"));
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className={cn("message-view", follow.fadeClass)}>
        {connectionState === "reconnecting" && (
          <p
            role="status"
            className="text-info-foreground-muted pointer-events-none absolute inset-x-0 top-2 z-[1] text-center text-[11px] [animation:code-reveal_140ms_ease-out] motion-reduce:animate-none"
          >
            Reconnecting to the session…
          </p>
        )}
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
          onReveal={follow.pauseFollow}
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
            "border-border text-foreground bg-background hover:bg-accent pointer-events-none absolute bottom-3 left-1/2 z-[1] inline-flex -translate-x-1/2 cursor-pointer items-center justify-center rounded-full border p-2 opacity-0 shadow transition-[opacity,background-color] duration-[140ms] ease-out motion-reduce:transition-none",
            FOCUS_RING,
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
          history={composerHistory}
          queued={queued}
          lastTurnBeganId={lastTurnBeganId}
          slashCommands={doctorEntry?.commands}
          searchPaths={(query) =>
            client
              .listCodeWorkspaceTree(workspaceId, { query })
              .then((tree) => tree.paths)
          }
          onModelChange={
            harnessHonorsTurnModel(session.harness_kind) ? setModel : undefined
          }
          onSend={send}
          onSteer={steeringSupported ? steer : undefined}
          onInterrupt={interrupt}
        />
      )}
    </div>
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


