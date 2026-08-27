import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCenter,
  pointerWithin,
  useDroppable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type PointerSensorOptions,
} from "@dnd-kit/core";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { ArrowDown, Bot, CircleDotDashed } from "lucide-react";
import { useDefaultLayout, useGroupRef } from "react-resizable-panels";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  Attention,
  CodeApprovalSnapshot,
  CodeForkTranscript,
  PermissionMode,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeSubagentStatus,
  CodeSubagentSummary,
  CodeWatchSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  ModelInfo,
  ReasoningEffort,
} from "../api/types";
import { useNavigate, useSearch } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { copyPlainText } from "@/ClipboardCopyButton";
import { attachedRemotely } from "@/host";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import { Skeleton } from "@/components/ui/skeleton";
import { PanelLayout } from "@/panel/PanelLayout";
import type { LayoutState, PanelContent } from "@/panel/panelTypes";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { RouteFrame } from "@/RouteFrame";
import { QueueTray, useCodeQueueApi } from "@/QueueTray";
import { followScrollBehavior } from "@/ChatScroll";
import { useStreamStalled } from "@/useStreamStalled";
import { useTranscriptFollow } from "@/useTranscriptFollow";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { ErrorBoundary } from "@/ErrorBoundary";
import { MarkdownLinkProvider } from "@/MessageMarkdown";
import { usePortalOverlayOpen } from "@/lib/usePortalOverlayOpen";
import {
  SHELL_SHORTCUTS,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutAction,
} from "@/ShellShortcuts";
import { AttentionBadge } from "./AttentionBadge";
import { attentionMarkForDigest, STATUS_MARK } from "./statusTone";
import {
  codeBrowserIds,
  codeTerminalIds,
  closeAllEditorTabs,
  closeCodeChromeTab,
  closeEditorTab,
  closeEditorTabsToRight,
  closeOtherEditorTabs,
  type CodeEditorRegion,
  focusCodeChromeTab,
  focusConversation,
  focusedEditorPosition,
  focusEditorTab,
  mergeEditorSplit,
  moveEditorTab,
  openCodeEditor,
  removedCodeBrowserIds,
  reorderEditorTab,
  splitCodeChromeLayout,
  adoptCodeTerminalId,
  findCodeTerminalTab,
  removedCodeTerminalIds,
} from "./codeChrome";
import {
  centerEditorTabId,
  centerTabParts,
  CenterTabIcon,
  CHAT_PANEL_ID,
  CodeCenterTabs,
  type CodeConversationTab,
  conversationTabId,
  EDITOR_PANEL_ID,
  SPLIT_EDITOR_PANEL_ID,
} from "./CodeCenterTabs";
import { DiffPanel } from "./DiffPanel";
import { closeCodeBrowser } from "./browser/browserHost";
import {
  seedBrowserSession,
  storedBrowserTitle,
} from "./browser/browserPersistence";

const FileViewer = lazy(async () => {
  const module = await import("./FileViewer");
  return { default: module.FileViewer };
});
const CodeBrowserTab = lazy(async () => {
  const module = await import("./browser/CodeBrowserTab");
  return { default: module.CodeBrowserTab };
});
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeInspector, PrTab } from "./CodeInspector";
import { DiffOverview } from "./DiffOverview";
import { useCodeUiStore } from "./CodeUiStore";
import { forkFraming, forkTranscriptFile } from "./fork";
import {
  useCodeUpdatesStore,
  useConversationDigests,
  useSessionDigest,
} from "./CodeUpdatesStore";
import { liveCodeSessions } from "./parsers";
import { CodeComposer } from "./CodeComposer";
import { CodeQuickOpen } from "./CodeQuickOpen";
import { WorkspaceWorkflowControl } from "./WorkspaceWorkflowControl";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "./CodeSessionRegistry";
import { submitAcceptedTurn } from "./CodeSessionSend";
import { CodeSidebar } from "./CodeSidebar";
import { CodeTranscript } from "./CodeTranscript";
import {
  mainAgentTranscriptItems,
  subagentTranscriptItems,
  type CodeTranscriptItem,
} from "./CodeSessionReducer";
import {
  dropEditorTab,
  findEditorPanel,
  isEditorStripDropId,
  offersSplitDrop,
  EDITOR_SPLIT_DROP_ID,
} from "./editorDrag";
import { FOCUS_RING } from "./interactive";
import { StartSessionPrompt } from "./StartSessionPrompt";
import { TerminalPane } from "./TerminalPane";
import { useCodeWorkspacePr } from "./useCodeWorkspacePr";
import { useCodeContentRevision } from "./useLiveContent";
import { SessionLifecycleIndicator } from "./SessionLifecycleIndicator";
import { SessionPermissionIndicator } from "./SessionPermissionIndicator";
import { WorkspaceHeader } from "./WorkspaceHeader";
import { canOpenInExternalEditor } from "./codeWorktreeHost";
import {
  WorkspaceOverflowMenu,
  openWorkspaceFileInEditor,
  useWorkspaceCardCommands,
  workspaceHeaderCommands,
} from "./workspaceActions";
import { tidebreakProductRepo } from "./uneffMe";
import { sessionActivityLabel, isPutAway } from "./workspaceCards";
import {
  DEFAULT_INSPECTOR_LAYOUT,
  fitsInspectorSplit,
  INSPECTOR_LAYOUT_STORAGE_ID,
  INSPECTOR_PANEL_IDS,
  MAX_INSPECTOR_SIZE,
  MIN_INSPECTOR_SIZE,
  MIN_WORKSPACE_SIZE,
  usableInspectorLayout,
} from "./inspectorLayout";
import {
  createPermissionModes,
  fenceReasonText,
  gatewayCodeModels,
  harnessCodeModels,
  HARNESS_LABELS,
  preferredCodeModels,
  requiresHarnessModelIds,
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
        {/* Reset workspace-scoped state without remounting the shared rail. */}
        <CodeWorkspaceBody key={workspaceId} workspaceId={workspaceId} />
      </div>
    </RouteFrame>
  );
}

/**
 * Stable empty ladder. A fresh `[]` per render is a new snapshot every time,
 * and zustand v5 loops on referentially unstable selector results.
 */
const EMPTY_EFFORTS: readonly ReasoningEffort[] = [];

type FirstTurnRecovery = {
  id: string;
  sessionId: string;
  draft: string;
  forkSource: CodeForkTranscript | null;
  message: string;
  status: "sending" | "failed";
};

const firstTurnRecoveryByClient = new WeakMap<
  ApiClient,
  Map<string, FirstTurnRecovery>
>();
const firstTurnRecoveryListeners = new Set<() => void>();

function readFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
): FirstTurnRecovery | null {
  return firstTurnRecoveryByClient.get(client)?.get(sessionId) ?? null;
}

function writeFirstTurnRecovery(
  client: ApiClient,
  recovery: FirstTurnRecovery,
): void {
  let recoveries = firstTurnRecoveryByClient.get(client);
  if (!recoveries) {
    recoveries = new Map();
    firstTurnRecoveryByClient.set(client, recoveries);
  }
  recoveries.set(recovery.sessionId, recovery);
  for (const listener of firstTurnRecoveryListeners) listener();
}

function clearFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
  recoveryId: string,
): void {
  const recoveries = firstTurnRecoveryByClient.get(client);
  if (recoveries?.get(sessionId)?.id !== recoveryId) return;
  recoveries.delete(sessionId);
  for (const listener of firstTurnRecoveryListeners) listener();
}

function updateFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
  recoveryId: string,
  update: (current: FirstTurnRecovery) => FirstTurnRecovery,
): void {
  const current = readFirstTurnRecovery(client, sessionId);
  if (!current || current.id !== recoveryId) return;
  writeFirstTurnRecovery(client, update(current));
}

function useFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
): FirstTurnRecovery | null {
  return useSyncExternalStore(
    (listener) => {
      firstTurnRecoveryListeners.add(listener);
      return () => {
        firstTurnRecoveryListeners.delete(listener);
      };
    },
    () => readFirstTurnRecovery(client, sessionId),
    () => null,
  );
}

function CodeWorkspaceBody({ workspaceId }: { workspaceId: string }) {
  const { client, models, defaultModelKey } = useApp();
  const clientRef = useRef(client);
  clientRef.current = client;
  const catalog = useCodeCatalogStore();
  const { run, dialogs } = useWorkspaceCardCommands();
  const layout = useLayoutState();
  const { setLayout } = usePanelNav();
  const layoutRef = useRef(layout);
  const previousLayoutRef = useRef(layout);
  const closedBrowserIdsRef = useRef(new Set<string>());
  const closedTerminalIdsRef = useRef(new Set<string>());
  /** Where focus sat before the terminal chord jumped away from it. */
  const beforeTerminalRef = useRef<{
    region: CodeEditorRegion;
    index: number;
  } | null>(null);
  const mountedRef = useRef(true);
  const selectionRevisionRef = useRef(0);
  const startRequestRef = useRef(0);
  const workspaceBrowserIdsRef = useRef(new Set<string>());
  const chrome = splitCodeChromeLayout(layout);
  const workspaceOverlayOpen = usePortalOverlayOpen();
  const navigate = useNavigate();
  const workspaceSearch = useSearch({ strict: false }) as {
    task?: string;
    subagent?: string;
  };
  const taskParam = workspaceSearch.task;
  const subagentParam = workspaceSearch.subagent;
  const inspectorLayout = useDefaultLayout({
    id: INSPECTOR_LAYOUT_STORAGE_ID,
    panelIds: INSPECTOR_PANEL_IDS,
    onlySaveAfterUserInteractions: true,
  });
  const inspectorGroupRef = useGroupRef();
  const reviewSidebarOpen = useCodeUiStore((state) => state.reviewSidebarOpen);
  const toggleReviewSidebar = useCodeUiStore(
    (state) => state.toggleReviewSidebar,
  );
  const setReviewSidebarOpen = useCodeUiStore(
    (state) => state.setReviewSidebarOpen,
  );
  const quickOpenPending = useCodeUiStore((state) => state.quickOpenPending);
  const newTabMenuPending = useCodeUiStore((state) => state.newTabMenuPending);
  const openFilePending = useCodeUiStore((state) => state.openFilePending);
  const archivePending = useCodeUiStore((state) => state.archivePending);
  const terminalPending = useCodeUiStore((state) => state.terminalPending);
  const shortcutHints = useCodeShortcutHints();
  const catalogWorkspace = catalog.workspaces.find(
    (candidate) => candidate.id === workspaceId,
  );
  const [workspace, setWorkspace] = useState<CodeWorkspaceSnapshot | null>(
    () => catalogWorkspace ?? null,
  );
  const catalogRepo = catalogWorkspace
    ? catalog.repos.find(
        (candidate) => candidate.id === catalogWorkspace.repo_id,
      )
    : undefined;
  const [repo, setRepo] = useState<CodeRepoSnapshot | null>(
    () => catalogRepo ?? null,
  );
  /**
   * Every session the server knows about here, conversations and watches alike.
   *
   * A workspace runs several agents (record 55), so the page holds the list and
   * picks one to show rather than tracking a single session.
   */
  const [sessions, setSessions] = useState<CodeSessionSnapshot[]>(() => {
    const remembered = catalog.sessionsByWorkspace[workspaceId];
    return remembered ? [remembered] : [];
  });
  const [activeSessionId, setActiveSessionId] = useState<string | null>(
    catalog.sessionsByWorkspace[workspaceId]?.id ?? null,
  );

  // Optimistic catalog mutations also govern the open page. Archive can hide
  // the rail card before filesystem cleanup finishes; reflecting that same
  // snapshot here stops the composer from offering work against a checkout
  // that is being removed. A failed request puts the original snapshot back.
  useEffect(() => {
    if (!catalogWorkspace) return;
    setWorkspace((current) =>
      current?.id === catalogWorkspace.id ? catalogWorkspace : current,
    );
  }, [catalogWorkspace]);

  useEffect(() => {
    if (!catalogRepo) return;
    setRepo((current) =>
      current?.id === catalogRepo.id ? catalogRepo : current,
    );
  }, [catalogRepo]);
  /** True while the reader is filling in a new agent that has no session yet. */
  const [draftAgent, setDraftAgent] = useState(false);
  /**
   * True once the server's session list has arrived for this workspace.
   *
   * `?task=` can only be judged against a loaded list. Before it lands, a
   * param that names nothing looks exactly like one naming a session the page
   * has not heard of yet, and clearing it would drop a good link on reload.
   */
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  /**
   * The transcript a fork wrote, waiting for the draft agent to send it.
   *
   * It survives an engine change and a rewritten framing line, and clears
   * once a session starts or the reader closes the draft.
   */
  const [forkSource, setForkSource] = useState<CodeForkTranscript | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);
  const [quickOpenRequest, setQuickOpenRequest] = useState(0);
  const [quickOpenTarget, setQuickOpenTarget] =
    useState<CodeEditorRegion>("primary");
  const [newTabMenuRequest, setNewTabMenuRequest] = useState(0);
  const [newTabMenuRegion, setNewTabMenuRegion] =
    useState<CodeEditorRegion>("primary");
  const [draggedTabId, setDraggedTabId] = useState<string | null>(null);
  const tabDragSensors = useSensors(
    // Four pixels of travel before a press becomes a drag, so a click still
    // selects the tab it landed on.
    useSensor(TabPointerSensor, { activationConstraint: { distance: 4 } }),
  );

  const inspectorDefaultLayout = useMemo(
    () =>
      usableInspectorLayout(inspectorLayout.defaultLayout) ?? {
        ...DEFAULT_INSPECTOR_LAYOUT,
      },
    [inspectorLayout.defaultLayout],
  );

  const { paneRef: inspectorPaneRef, width: inspectorPaneWidth } =
    useMeasuredWidth();
  const inspectorFits = fitsInspectorSplit(inspectorPaneWidth);
  /**
   * The split only appears when the reader asked for it and the pane can
   * carry it. The stored preference survives a narrow window, so widening
   * one brings the inspector straight back.
   */
  const inspectorOpen = reviewSidebarOpen && inspectorFits;
  const [starting, setStarting] = useState(false);
  const [createMode, setCreateMode] = useState<PermissionMode | null>(null);
  const [fileReveal, setFileReveal] = useState<{
    path: string;
    line: number;
    revision: number;
  } | null>(null);
  const [browserTitles, setBrowserTitles] = useState<Record<string, string>>(
    () => storedBrowserTitles(layout),
  );
  const [terminalLabels, setTerminalLabels] = useState<Record<string, string>>(
    () => nameTerminals({}, codeTerminalIds(layout)),
  );
  const conversations = useMemo(() => liveCodeSessions(sessions), [sessions]);
  const session = useMemo(
    () => sessions.find((entry) => entry.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
  );
  const digest = useSessionDigest(workspaceId, session?.id ?? null);
  const conversationDigests = useConversationDigests(workspaceId);
  const setViewedWorkspace = useCodeUpdatesStore(
    (state) => state.setViewedWorkspace,
  );
  const contentRevision = useCodeContentRevision(session?.id ?? null, client);
  const prResource = useCodeWorkspacePr(
    client,
    workspaceId,
    contentRevision,
    digest?.pr_state,
  );
  const rememberedSession = useCodeCatalogStore(
    (state) => state.sessionsByWorkspace[workspaceId] ?? null,
  );

  layoutRef.current = layout;

  useEffect(() => {
    startRequestRef.current += 1;
    setStarting(false);
  }, [client]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      startRequestRef.current += 1;
    };
  }, []);

  const closeBrowserPanels = useCallback(
    (browserIds: readonly string[]) => {
      for (const browserId of browserIds) {
        if (closedBrowserIdsRef.current.has(browserId)) continue;
        closedBrowserIdsRef.current.add(browserId);
        void closeCodeBrowser(workspaceId, browserId);
      }
    },
    [workspaceId],
  );

  /**
   * End the shells whose tabs have gone.
   *
   * A workspace may only hold so many at once, so a closed tab has to give
   * its shell back rather than leave it running with nothing pointing at it.
   *
   * Closing a tab is the only thing that ends a shell. Browsers also close
   * when the page unmounts, because a native webview is worth nothing once it
   * is off screen; a shell is the opposite. A build running in one has to
   * survive a click on another workspace, and the tab's address names it, so
   * coming back re-attaches to the same process with its output intact.
   */
  const closeTerminalPanels = useCallback(
    (terminalIds: readonly string[]) => {
      for (const terminalId of terminalIds) {
        if (closedTerminalIdsRef.current.has(terminalId)) continue;
        closedTerminalIdsRef.current.add(terminalId);
        void client.deleteCodeTerminal(workspaceId, terminalId).catch(() => {
          // A shell that is already gone is the outcome we wanted anyway.
        });
      }
    },
    [client, workspaceId],
  );

  function setWorkspaceLayout(next: LayoutState) {
    setLayout(next);
  }

  useEffect(() => {
    const ids = codeBrowserIds(layout);
    for (const browserId of ids) {
      closedBrowserIdsRef.current.delete(browserId);
      workspaceBrowserIdsRef.current.add(browserId);
    }
    closeBrowserPanels(
      removedCodeBrowserIds(previousLayoutRef.current, layout),
    );
    closeTerminalPanels(
      removedCodeTerminalIds(previousLayoutRef.current, layout),
    );
    previousLayoutRef.current = layout;
    setTerminalLabels((current) =>
      nameTerminals(current, codeTerminalIds(layout)),
    );
    setBrowserTitles((current) => {
      let changed = false;
      const next: Record<string, string> = {};
      for (const browserId of ids) {
        const title = current[browserId] ?? storedBrowserTitle(browserId);
        next[browserId] = title;
        if (current[browserId] !== title) changed = true;
      }
      if (Object.keys(current).length !== ids.length) changed = true;
      return changed ? next : current;
    });
  }, [closeBrowserPanels, closeTerminalPanels, layout]);

  useEffect(() => {
    workspaceBrowserIdsRef.current = new Set(codeBrowserIds(layoutRef.current));
    return () => closeBrowserPanels([...workspaceBrowserIdsRef.current]);
  }, [closeBrowserPanels, workspaceId]);

  useEffect(() => {
    setViewedWorkspace(workspaceId);
    return () => setViewedWorkspace(null);
  }, [setViewedWorkspace, workspaceId]);

  // A session started elsewhere — the new-workspace dialog, say — reaches the
  // page through the catalog before the list request comes back.
  useEffect(() => {
    if (!rememberedSession) return;
    setSessions((current) =>
      current.some((entry) => entry.id === rememberedSession.id)
        ? current
        : [...current, rememberedSession],
    );
  }, [rememberedSession]);

  // `?task=` names the session to show: a sibling agent, or a watch child
  // opened from the rail. The param is a request, not a fact — a link outlives
  // the agent it points at, so it holds only while that agent is still here
  // and still live.
  const namedTask = useMemo(() => {
    if (!taskParam) return null;
    const found = sessions.find((entry) => entry.id === taskParam);
    return found && found.lifecycle !== "ended" ? found : null;
  }, [sessions, taskParam]);

  // A param that names nothing showable is stale: the agent ended, or the link
  // came from somewhere else. Drop it, so the fallback below runs and the URL
  // stops naming an agent that is not there. Replace rather than push, so Back
  // does not lead to the same dead link.
  useEffect(() => {
    if (!sessionsLoaded || !taskParam || namedTask) return;
    openWorkspaceTask(undefined, { replace: true });
  }, [namedTask, sessionsLoaded, taskParam]);

  // A named session wins over the default selection below.
  useEffect(() => {
    if (!namedTask) return;
    setActiveSessionId(namedTask.id);
    setDraftAgent(false);
  }, [namedTask]);

  // Nothing named, or the shown agent ended: fall back to the first one. The
  // check reads the live conversations, not every session, so an agent that
  // ended under the reader does not stay selected.
  useEffect(() => {
    if (namedTask || draftAgent) return;
    const shown = conversations.some((entry) => entry.id === activeSessionId);
    if (activeSessionId && shown) return;
    setActiveSessionId(conversations[0]?.id ?? null);
  }, [activeSessionId, conversations, draftAgent, namedTask]);

  useEffect(() => {
    return () => {
      useCodeUiStore.getState().setInspectorScope(null);
      useCodeUiStore.getState().finishComposerAction(workspaceId);
    };
  }, [workspaceId]);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    setSessionsLoaded(false);
    void (async () => {
      try {
        const [next, listed] = await Promise.all([
          client.getCodeWorkspace(workspaceId),
          client.listCodeWorkspaceSessions(workspaceId),
        ]);
        if (cancelled) return;
        setWorkspace(next);
        const catalogState = useCodeCatalogStore.getState();
        catalogState.upsertWorkspace(next);
        // Create navigates as soon as the workspace exists, so a session the
        // dialog just started can land in the catalog before this list does.
        const remembered = catalogState.sessionsByWorkspace[workspaceId];
        setSessions(
          remembered && !listed.some((entry) => entry.id === remembered.id)
            ? [remembered, ...listed]
            : listed,
        );
        setSessionsLoaded(true);
        // The card and the rail show one agent per workspace, so the catalog
        // remembers the first — the one the workspace was started with.
        const first = liveCodeSessions(listed)[0];
        if (first) catalogState.rememberSession(first);
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
    permissionMode: PermissionMode,
    message: string,
    model?: string,
    draft = message,
    reasoningEffort?: ReasoningEffort | null,
    fastMode = false,
  ) {
    const request = startRequestRef.current + 1;
    startRequestRef.current = request;
    const startedWithClient = client;
    const startedAtSelection = selectionRevisionRef.current;
    const startedWithFork = forkSource;
    const isCurrent = () =>
      mountedRef.current &&
      startRequestRef.current === request &&
      clientRef.current === startedWithClient;
    setStarting(true);
    try {
      let created: CodeSessionSnapshot;
      try {
        const gateway = gatewayCodeModels(models, harness, defaultModelKey);
        const native =
          requiresHarnessModelIds(harness) || gateway.length === 0
            ? await catalog.ensureHarnessModels(startedWithClient, harness)
            : [];
        if (!isCurrent()) {
          throw new Error(
            "The Code connection changed before the session started. Send the message again.",
          );
        }
        const listed = preferredCodeModels(harness, native, gateway);
        const posted =
          model ?? listed.find((option) => option.default)?.id ?? listed[0]?.id;
        created = await startedWithClient.createCodeSession(workspaceId, {
          harness,
          permission_mode: permissionMode,
          model: posted,
          ...(reasoningEffort ? { reasoning_effort: reasoningEffort } : {}),
          ...(fastMode ? { fast_mode: true } : {}),
        });
      } catch (err) {
        if (isCurrent()) {
          toast.error(friendlyErrorMessage(err, "Could not start a session"));
        }
        throw err;
      }

      const recovery: FirstTurnRecovery = {
        id: `${created.id}:${request}`,
        sessionId: created.id,
        draft,
        forkSource: startedWithFork,
        message: "Sending your first message…",
        status: "sending",
      };
      if (!isCurrent()) {
        const message =
          "The Code connection changed after the session was created. Send the message again.";
        writeFirstTurnRecovery(startedWithClient, {
          ...recovery,
          message,
          status: "failed",
        });
        throw new Error(message);
      }
      writeFirstTurnRecovery(startedWithClient, recovery);

      if (conversations.length === 0) catalog.rememberSession(created);
      setSessions((current) =>
        current.some((entry) => entry.id === created.id)
          ? current
          : [...current, created],
      );
      setForkSource((current) =>
        current === startedWithFork ? null : current,
      );
      if (selectionRevisionRef.current === startedAtSelection) {
        setActiveSessionId(created.id);
        setDraftAgent(false);
        // The first agent stays at a clean URL; a sibling names itself so a
        // reload comes back to the tab the reader was on.
        if (conversations.length > 0) openWorkspaceTask(created.id);
      }

      try {
        await startedWithClient.submitCodeTurn(created.id, message);
        clearFirstTurnRecovery(startedWithClient, created.id, recovery.id);
      } catch (err) {
        const detail = friendlyErrorMessage(err, "Try sending it again.");
        writeFirstTurnRecovery(startedWithClient, {
          ...recovery,
          message: `The first message was not sent. Review it, then choose Send to try again. ${detail}`,
          status: "failed",
        });
        if (isCurrent()) {
          toast.error(
            `Session started, but the first message was not sent. ${detail}`,
          );
        }
      }
    } finally {
      if (isCurrent()) setStarting(false);
    }
  }

  async function reap() {
    if (!session) return;
    try {
      const next = await client.reapCodeSession(session.id);
      catalog.rememberSession(next);
      setSessions((current) =>
        current.map((entry) => (entry.id === next.id ? next : entry)),
      );
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not reap the session"));
    }
  }

  const fenced =
    session?.lifecycle === "fenced" || session?.fence_reason !== undefined;
  const doctorHarnesses = catalog.doctor?.harnesses ?? [];
  const title = digest?.title ?? workspace?.title;
  const repoName = repo?.display_name;
  const pr = digest?.pr_state ?? workspace?.pr;
  const headerCommands = workspace
    ? workspaceHeaderCommands({
        archived: isPutAway(workspace),
        hasSession: Boolean(session),
        attentionPinned:
          (digest?.attention ?? session?.attention)?.state.type === "manual",
        // A watch child is the harness's own run, not a conversation to
        // continue, so only an interactive agent offers a fork.
        canFork: session?.kind === "interactive",
        canUneff: Boolean(tidebreakProductRepo(catalog.repos)),
        quickActions: repo?.quick_actions ?? [],
        setupFailed: workspace.status === "setup_failed",
      })
    : [];

  function openTurnDiff(turnId: string) {
    setWorkspaceLayout(openCodeEditor(layout, { type: "diff", turnId }));
  }

  function openFile(
    path: string,
    line?: number,
    preferredRegion?: CodeEditorRegion,
  ) {
    setFileReveal((current) =>
      line === undefined
        ? null
        : {
            path,
            line,
            revision: (current?.revision ?? 0) + 1,
          },
    );
    setWorkspaceLayout(
      openCodeEditor(layout, { type: "file", path }, preferredRegion),
    );
  }

  function openFileDiff(path: string) {
    setWorkspaceLayout(openCodeEditor(layout, { type: "diff", path }));
  }

  function openBrowser(url?: string, preferredRegion?: CodeEditorRegion) {
    const browserId = crypto.randomUUID();
    const browser = seedBrowserSession({
      browserId,
      workspaceId,
      initialUrl: url,
    });
    setBrowserTitles((current) => ({
      ...current,
      [browserId]: browser.title || "Browser",
    }));
    setWorkspaceLayout(
      openCodeEditor(layout, { type: "browser", browserId }, preferredRegion),
    );
  }

  /**
   * Start a shell and give it a tab.
   *
   * The server names the shell, so the tab cannot exist before the call
   * answers — unlike a browser, whose id the page mints itself.
   */
  async function openTerminal(preferredRegion?: CodeEditorRegion) {
    try {
      const snap = await client.createCodeTerminal(workspaceId);
      setTerminalLabels((current) => nameTerminals(current, [snap.id]));
      setWorkspaceLayout(
        openCodeEditor(
          layoutRef.current,
          { type: "terminal", terminalId: snap.id },
          preferredRegion,
        ),
      );
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not open a terminal"));
    }
  }

  /**
   * Jump to the terminal and back again.
   *
   * The chord used to show and hide a drawer. A terminal is a tab now, so the
   * same press moves focus there and the next one returns it where it was —
   * the flick there and back a drawer gave, without a second kind of surface.
   */
  function toggleTerminal() {
    const found = findCodeTerminalTab(layoutRef.current);
    if (!found) {
      beforeTerminalRef.current = focusedEditorPosition(layoutRef.current);
      void openTerminal("primary");
      return;
    }
    const focused = focusedEditorPosition(layoutRef.current);
    const onTerminal =
      focused?.region === found.region && focused.index === found.index;
    if (!onTerminal) {
      beforeTerminalRef.current = focused;
      setWorkspaceLayout(
        focusEditorTab(layoutRef.current, found.index, found.region),
      );
      return;
    }
    const back = beforeTerminalRef.current;
    beforeTerminalRef.current = null;
    setWorkspaceLayout(
      back
        ? focusEditorTab(layoutRef.current, back.index, back.region)
        : focusConversation(layoutRef.current),
    );
  }

  function requestNewTab(region: CodeEditorRegion) {
    setQuickOpenTarget(region);
    setQuickOpenRequest((request) => request + 1);
  }

  function showNewTabMenu(region: CodeEditorRegion) {
    setNewTabMenuRegion(region);
    setNewTabMenuRequest((request) => request + 1);
  }

  // The shell keymap raises the ask above the route; the workspace is what can
  // answer it. Taking the flag is what stops a remount from reopening the
  // picker over whatever the reader moved on to.
  const splitFocused =
    Boolean(layout.editorSplit?.focused) && chrome.splitEditors.tabs.length > 0;
  useEffect(() => {
    if (!quickOpenPending) return;
    if (!useCodeUiStore.getState().takeQuickOpen()) return;
    requestNewTab(splitFocused ? "secondary" : "primary");
  }, [quickOpenPending, splitFocused]);

  useEffect(() => {
    if (!newTabMenuPending) return;
    if (!useCodeUiStore.getState().takeNewTabMenu()) return;
    showNewTabMenu(splitFocused ? "secondary" : "primary");
  }, [newTabMenuPending, splitFocused]);

  // The palette ranks worktree files but has nowhere to put one; the tabs live
  // here, so it names a path and this opens it.
  useEffect(() => {
    if (!openFilePending) return;
    const path = useCodeUiStore.getState().takeOpenFilePath();
    if (path) openFile(path);
  }, [openFilePending]);

  // The chord and the rail command both raise the ask above the route, because
  // starting a shell is a server call neither of them can make.
  useEffect(() => {
    if (!terminalPending) return;
    if (!useCodeUiStore.getState().takeTerminal()) return;
    toggleTerminal();
    // The flag is the trigger; the rest is state read when it arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalPending]);

  /**
   * Archive from the keyboard, through the same confirmation the menu uses.
   *
   * The page takes this one rather than the header control, because archiving
   * is a workspace command and not a step in the pull-request workflow.
   */
  useEffect(() => {
    if (!archivePending) return;
    if (!useCodeUiStore.getState().takeArchiveWorkspace()) return;
    if (!workspace) return;
    if (isPutAway(workspace)) {
      toast.message("This workspace is already archived");
      return;
    }
    run("archive", {
      workspace,
      title: title ?? workspace.title,
      session: session ?? undefined,
    });
    // The chord is the trigger; the rest is state read when it arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [archivePending]);

  /**
   * Attach a child task's transcript (or the conversation when undefined).
   *
   * `replace` is for corrections rather than choices: clearing a stale param
   * should not leave a history entry the reader can walk back into.
   */
  function openWorkspaceTask(
    sessionId: string | undefined,
    options?: { replace?: boolean },
  ) {
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
      replace: options?.replace ?? false,
      search: (current: Record<string, unknown>) => ({
        ...current,
        task: sessionId,
        subagent: undefined,
      }),
    });
  }

  /**
   * Show one of the workspace's agents, or the unstarted draft when null.
   *
   * Selection lives in `?task=` so a reload or a shared link returns to the
   * same agent. The first one is the workspace's default, so it stays unnamed.
   */
  function selectConversation(sessionId: string | null) {
    selectionRevisionRef.current += 1;
    setWorkspaceLayout(focusConversation(layout));
    if (sessionId === null) {
      setDraftAgent(true);
      setActiveSessionId(null);
      openWorkspaceTask(undefined);
      return;
    }
    setDraftAgent(false);
    setActiveSessionId(sessionId);
    openWorkspaceTask(
      sessionId === conversations[0]?.id ? undefined : sessionId,
    );
  }

  /** Add a tab for an agent the reader has not filled in yet. */
  function newConversation() {
    setForkSource(null);
    selectConversation(null);
  }

  /**
   * Hand one agent's transcript to a fresh one.
   *
   * The server writes the fork into private storage — the condensed
   * transcript plus a full record per turn — so the child reads it from an
   * absolute path whatever engine it turns out to be. `atTurnId` forks at
   * the end of that turn; omitted, the fork covers the whole conversation.
   * Nothing is sent here: the draft tab opens with the transcript attached
   * and framing lines the reader edits first.
   */
  async function forkConversation(sessionId: string, atTurnId?: string) {
    try {
      const written = await client.forkCodeSession(sessionId, atTurnId);
      setForkSource(written);
      selectConversation(null);
      useCodeUiStore
        .getState()
        .offerComposerPrompt(workspaceId, forkFraming(written));
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not fork this agent"));
    }
  }

  /**
   * Close a conversation tab.
   *
   * Only the draft closes here. A started agent holds a worktree and a running
   * engine, so ending one is a server action rather than a tab control.
   */
  function closeConversation(sessionId: string | null) {
    if (sessionId !== null) return;
    setDraftAgent(false);
    setForkSource(null);
    const first = conversations[0];
    if (first) selectConversation(first.id);
    else setActiveSessionId(null);
  }

  /** Filter the parent transcript to one harness-owned child. */
  function openWorkspaceSubagent(callId: string | undefined) {
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
      search: (current: Record<string, unknown>) => ({
        ...current,
        task: undefined,
        subagent: callId,
      }),
    });
  }

  function finishTabDrag(event: DragEndEvent) {
    setDraggedTabId(null);
    const next = dropEditorTab(
      layout,
      String(event.active.id),
      event.over ? String(event.over.id) : null,
    );
    if (next) setWorkspaceLayout(next);
  }

  function copyEditorPath(path: string) {
    void copyPlainText(path)
      .then(() => toast.success("Copied path"))
      .catch(() => toast.error("Could not copy path"));
  }

  /**
   * The agent tab the strip should mark selected.
   *
   * A watch child opened from the rail is a drill-in with its own back bar, not
   * a peer tab, so the first agent stays selected underneath it.
   */
  const activeConversationId = draftAgent
    ? null
    : (conversations.find((entry) => entry.id === session?.id)?.id ??
      conversations[0]?.id ??
      null);
  const conversationTabs = useMemo<CodeConversationTab[]>(() => {
    const tabs: CodeConversationTab[] = conversations.map((entry, index) => {
      const digest = conversationDigests[entry.id];
      return {
        id: entry.id,
        label: conversationTabLabel(entry, index, conversations),
        harness: entry.harness_kind,
        attention: attentionMarkForDigest(digest),
      };
    });
    // A draft has no engine yet, so it wears the generic agent glyph. It is
    // also the one closable tab: nothing is running behind it. A workspace
    // with no agents at all still gets one, so the strip always names the
    // panel below it.
    if (draftAgent || tabs.length === 0) {
      tabs.push({
        id: null,
        label: tabs.length === 0 ? "Main agent" : "New agent",
        closable: tabs.length > 0,
      });
    }
    return tabs;
  }, [conversationDigests, conversations, draftAgent]);

  /** The picker shows for a first agent and for every one added after it. */
  const startingNewAgent = draftAgent || !session;

  const editorTabs = chrome.editors.tabs;
  const showingChat =
    editorTabs.length === 0 || Boolean(chrome.editors.conversationFocused);
  const activeEditor = showingChat
    ? null
    : (editorTabs[chrome.editors.activeIndex] ?? null);
  const splitEditorTabs = chrome.splitEditors.tabs;
  const activeSplitEditor =
    splitEditorTabs[chrome.splitEditors.activeIndex] ?? null;
  const hasEditorSplit = splitEditorTabs.length > 0;
  const openTerminalCount = codeTerminalIds(layout).length;
  const hasTerminal = findCodeTerminalTab(layout) !== null;
  const canNewTerminal = openTerminalCount < MAX_WORKSPACE_TERMINALS;
  // The browser opens as a child webview on this computer. A window working on
  // another machine has no such screen to lend — sharing one with an agent that
  // is not here shares the wrong browser — so the row is absent rather than
  // present and refusing.
  const canNewBrowser = !attachedRemotely();

  function editorPanel(
    panel: PanelContent,
    region: CodeEditorRegion,
    index: number,
  ) {
    const id = region === "primary" ? EDITOR_PANEL_ID : SPLIT_EDITOR_PANEL_ID;
    return (
      <div
        className="flex min-h-0 flex-1 flex-col overflow-hidden"
        id={id}
        role="tabpanel"
        aria-labelledby={centerEditorTabId(index, region)}
      >
        {panel.type === "file" ? (
          <Suspense fallback={<Skeleton className="h-full w-full" />}>
            <FileViewer
              client={client}
              workspaceId={workspaceId}
              path={panel.path}
              contentRevision={contentRevision}
              revealLine={
                fileReveal?.path === panel.path ? fileReveal.line : undefined
              }
              revealRevision={fileReveal?.revision}
              onOpenInEditor={
                canOpenInExternalEditor()
                  ? (path, line) =>
                      openWorkspaceFileInEditor({
                        workspaceId,
                        relativePath: path,
                        line,
                      })
                  : undefined
              }
            />
          </Suspense>
        ) : panel.type === "diff" ? (
          <DiffPanel
            client={client}
            workspaceId={workspaceId}
            turnId={panel.turnId}
            file={panel.path}
            contentRevision={contentRevision}
            onOpenFile={(path) => openFile(path, undefined, region)}
            onOpenInEditor={
              canOpenInExternalEditor()
                ? (path) =>
                    openWorkspaceFileInEditor({
                      workspaceId,
                      relativePath: path,
                    })
                : undefined
            }
          />
        ) : panel.type === "browser" ? (
          <Suspense fallback={<Skeleton className="h-full w-full" />}>
            <CodeBrowserTab
              workspaceId={workspaceId}
              browserId={panel.browserId}
              obscured={workspaceOverlayOpen}
              onTitleChange={(title) =>
                setBrowserTitles((current) =>
                  current[panel.browserId] === title
                    ? current
                    : { ...current, [panel.browserId]: title },
                )
              }
            />
          </Suspense>
        ) : panel.type === "terminal" ? (
          <TerminalPane
            client={client}
            workspaceId={workspaceId}
            terminalId={panel.terminalId}
            onAttach={(terminalId) =>
              setWorkspaceLayout(
                adoptCodeTerminalId(
                  layoutRef.current,
                  panel.terminalId,
                  terminalId,
                ),
              )
            }
            hideHeader
          />
        ) : panel.type === "source_control" ? (
          <div
            className="flex min-h-0 flex-1 flex-col overflow-hidden"
            data-testid="source-control-panel"
          >
            <DiffOverview
              client={client}
              workspaceId={workspaceId}
              contentRevision={contentRevision}
              onOpenFile={(path) => openFileDiff(path)}
            />
          </div>
        ) : panel.type === "pr" ? (
          <div
            className="flex min-h-0 flex-1 flex-col overflow-hidden"
            data-testid="pr-details-panel"
          >
            <PrTab
              client={client}
              workspaceId={workspaceId}
              pr={prResource.data?.pr ?? workspace?.pr}
              branch={workspace?.branch_name}
              prResource={prResource}
            />
          </div>
        ) : null}
      </div>
    );
  }

  const primaryEditorGroup = (
    <div
      className="flex h-full min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      data-testid="primary-editor-group"
    >
      <CodeCenterTabs
        editorTabs={editorTabs}
        browserTitles={browserTitles}
        terminalLabels={terminalLabels}
        editorActiveIndex={chrome.editors.activeIndex}
        conversationFocused={showingChat}
        conversations={conversationTabs}
        activeConversationId={activeConversationId}
        onSelectConversation={selectConversation}
        onNewConversation={newConversation}
        onCloseConversation={closeConversation}
        onForkConversation={(sessionId) => void forkConversation(sessionId)}
        onSelectEditor={(index) =>
          setWorkspaceLayout(focusEditorTab(layout, index, "primary"))
        }
        onCloseEditor={(index) =>
          setWorkspaceLayout(closeEditorTab(layout, index, "primary"))
        }
        onCloseAllEditors={() =>
          setWorkspaceLayout(closeAllEditorTabs(layout, "primary"))
        }
        onCloseEveryEditor={() =>
          setWorkspaceLayout(closeAllEditorTabs(layout))
        }
        onCloseOtherEditors={(index) =>
          setWorkspaceLayout(closeOtherEditorTabs(layout, index, "primary"))
        }
        onCloseEditorsToRight={(index) =>
          setWorkspaceLayout(closeEditorTabsToRight(layout, index, "primary"))
        }
        onCopyPath={copyEditorPath}
        onNewTab={() => requestNewTab("primary")}
        openMenuRequest={newTabMenuRegion === "primary" ? newTabMenuRequest : 0}
        onNewBrowser={
          canNewBrowser ? () => openBrowser(undefined, "primary") : undefined
        }
        onNewDiff={() =>
          setWorkspaceLayout(
            openCodeEditor(layout, { type: "diff" }, "primary"),
          )
        }
        onNewSourceControl={() =>
          setWorkspaceLayout(
            openCodeEditor(layout, { type: "source_control" }, "primary"),
          )
        }
        onNewPr={
          pr
            ? () =>
                setWorkspaceLayout(
                  openCodeEditor(layout, { type: "pr" }, "primary"),
                )
            : undefined
        }
        onNewTerminal={() => void openTerminal("primary")}
        canNewTerminal={canNewTerminal}
        onMoveEditorToOtherGroup={(index) =>
          setWorkspaceLayout(
            moveEditorTab(layout, "primary", index, "secondary"),
          )
        }
        onMoveEditor={(from, to) =>
          setWorkspaceLayout(reorderEditorTab(layout, "primary", from, to))
        }
        onSplitActive={() =>
          setWorkspaceLayout(
            moveEditorTab(
              layout,
              "primary",
              chrome.editors.activeIndex,
              "secondary",
            ),
          )
        }
      />
      <div
        className={cn("min-h-0 flex-1", !showingChat && "hidden")}
        id={CHAT_PANEL_ID}
        role="tabpanel"
        aria-labelledby={conversationTabId(activeConversationId)}
      >
        <PanelLayout
          layout={chrome.panels}
          framed={false}
          onFocusTab={(index) =>
            setWorkspaceLayout(focusCodeChromeTab(layout, index))
          }
          onCloseTab={(index) =>
            setWorkspaceLayout(closeCodeChromeTab(layout, index))
          }
          renderChat={(visible) => (
            // The panel slot is a plain block. `.chat-pane` claims that height
            // so `.message-view` can grow and the composer stays at the bottom,
            // including on an empty transcript.
            <div className="chat-pane" hidden={!visible || !showingChat}>
              {fenced && session?.fence_reason && (
                <div className="border-warning-border bg-warning-background text-warning-foreground mx-4 mt-3 flex flex-col gap-2 rounded-md border px-3 py-2 text-sm">
                  <p>{fenceReasonText(session.fence_reason)}</p>
                  <Button
                    type="button"
                    size="sm"
                    className="self-start"
                    onClick={() => void reap()}
                  >
                    Reap
                  </Button>
                </div>
              )}
              {startingNewAgent && workspace?.status === "active" && (
                <StartSessionPrompt
                  workspaceId={workspaceId}
                  harnesses={doctorHarnesses}
                  starting={starting}
                  selectedMode={createMode}
                  onSelectMode={setCreateMode}
                  client={client}
                  catalogModels={models}
                  defaultModelKey={defaultModelKey}
                  onStart={(
                    harness,
                    mode,
                    message,
                    model,
                    draft,
                    reasoningEffort,
                    fastMode,
                  ) =>
                    startSession(
                      harness,
                      mode,
                      message,
                      model,
                      draft,
                      reasoningEffort,
                      fastMode,
                    )
                  }
                  workspaceFiles={
                    forkSource
                      ? {
                          items: [forkTranscriptFile(forkSource)],
                          onRemove: () => setForkSource(null),
                        }
                      : undefined
                  }
                />
              )}
              {session && !draftAgent && (
                <CodeSessionPane
                  key={session.id}
                  session={session}
                  workspaceId={workspaceId}
                  client={client}
                  catalogModels={models}
                  defaultModelKey={defaultModelKey}
                  disabled={fenced || workspace?.status !== "active"}
                  onOpenTurnDiff={openTurnDiff}
                  onForkFromTurn={
                    session.kind === "interactive"
                      ? (turnId) => void forkConversation(session.id, turnId)
                      : undefined
                  }
                  subagentCallId={subagentParam}
                  subagentSummary={digest?.subagents?.find(
                    (entry) => entry.call_id === subagentParam,
                  )}
                  onBackFromSubagent={() => openWorkspaceSubagent(undefined)}
                  composerOverride={
                    session.kind === "watch" ? (
                      <WatchTaskBar
                        client={client}
                        workspaceId={workspaceId}
                        watch={prResource.data?.watch}
                        onBack={() => openWorkspaceTask(undefined)}
                        onStopped={() => {
                          openWorkspaceTask(undefined);
                          void prResource.refresh();
                        }}
                      />
                    ) : undefined
                  }
                />
              )}
            </div>
          )}
          renderPanel={() => renderCodePanel()}
        />
      </div>
      {!showingChat &&
        activeEditor &&
        editorPanel(activeEditor, "primary", chrome.editors.activeIndex)}
    </div>
  );

  const splitEditorGroup = activeSplitEditor ? (
    <div
      className="flex h-full min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      data-testid="secondary-editor-group"
    >
      <CodeCenterTabs
        region="secondary"
        editorTabs={splitEditorTabs}
        browserTitles={browserTitles}
        terminalLabels={terminalLabels}
        editorActiveIndex={chrome.splitEditors.activeIndex}
        conversationFocused={false}
        onSelectEditor={(index) =>
          setWorkspaceLayout(focusEditorTab(layout, index, "secondary"))
        }
        onCloseEditor={(index) =>
          setWorkspaceLayout(closeEditorTab(layout, index, "secondary"))
        }
        onCloseAllEditors={() =>
          setWorkspaceLayout(closeAllEditorTabs(layout, "secondary"))
        }
        onCloseOtherEditors={(index) =>
          setWorkspaceLayout(closeOtherEditorTabs(layout, index, "secondary"))
        }
        onCloseEditorsToRight={(index) =>
          setWorkspaceLayout(closeEditorTabsToRight(layout, index, "secondary"))
        }
        onCopyPath={copyEditorPath}
        onNewTab={() => requestNewTab("secondary")}
        openMenuRequest={
          newTabMenuRegion === "secondary" ? newTabMenuRequest : 0
        }
        onNewBrowser={
          canNewBrowser ? () => openBrowser(undefined, "secondary") : undefined
        }
        onNewDiff={() =>
          setWorkspaceLayout(
            openCodeEditor(layout, { type: "diff" }, "secondary"),
          )
        }
        onNewSourceControl={() =>
          setWorkspaceLayout(
            openCodeEditor(layout, { type: "source_control" }, "secondary"),
          )
        }
        onNewPr={
          pr
            ? () =>
                setWorkspaceLayout(
                  openCodeEditor(layout, { type: "pr" }, "secondary"),
                )
            : undefined
        }
        onNewTerminal={() => void openTerminal("secondary")}
        canNewTerminal={canNewTerminal}
        onMoveEditorToOtherGroup={(index) =>
          setWorkspaceLayout(
            moveEditorTab(layout, "secondary", index, "primary"),
          )
        }
        onMoveEditor={(from, to) =>
          setWorkspaceLayout(reorderEditorTab(layout, "secondary", from, to))
        }
        onCloseGroup={() => setWorkspaceLayout(mergeEditorSplit(layout))}
      />
      {editorPanel(
        activeSplitEditor,
        "secondary",
        chrome.splitEditors.activeIndex,
      )}
    </div>
  ) : null;

  const draggedPanel = draggedTabId
    ? findEditorPanel(layout, draggedTabId)
    : null;

  const workspaceMain = (
    <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      <DndContext
        sensors={tabDragSensors}
        collisionDetection={tabDropTarget}
        onDragStart={(event) => setDraggedTabId(String(event.active.id))}
        onDragCancel={() => setDraggedTabId(null)}
        onDragEnd={finishTabDrag}
      >
        <div className="relative min-h-0 min-w-0 flex-1 overflow-hidden">
          {hasEditorSplit ? (
            <ResizablePanelGroup
              orientation="horizontal"
              className="h-full min-h-0"
            >
              <ResizablePanel
                id="editor-primary"
                defaultSize="55"
                minSize="25"
                className="min-w-0"
              >
                {primaryEditorGroup}
              </ResizablePanel>
              <ResizableHandle />
              <ResizablePanel
                id="editor-split"
                defaultSize="45"
                minSize="25"
                className="min-w-0"
              >
                {splitEditorGroup}
              </ResizablePanel>
            </ResizablePanelGroup>
          ) : (
            primaryEditorGroup
          )}
          {draggedTabId && offersSplitDrop(layout, draggedTabId) && (
            <SplitDropZone />
          )}
        </div>
        {/* The strip scrolls, so a transformed child would be clipped by it.
            The overlay draws the moving tab above everything instead. */}
        <DragOverlay dropAnimation={null}>
          {draggedPanel ? (
            <span className="flex h-8 items-center gap-1.5 rounded-lg bg-background px-2.5 text-xs font-medium text-foreground shadow-lg">
              <CenterTabIcon panel={draggedPanel} />
              {centerTabParts(draggedPanel, browserTitles, terminalLabels).name}
            </span>
          ) : null}
        </DragOverlay>
      </DndContext>
    </div>
  );

  return (
    <MarkdownLinkProvider
      onOpenInApp={canNewBrowser ? (url) => openBrowser(url) : undefined}
    >
      {dialogs}
      <CodeQuickOpen
        client={client}
        workspaceId={workspaceId}
        contentRevision={contentRevision}
        onOpenFile={(path) => openFile(path, undefined, quickOpenTarget)}
        openRequest={quickOpenRequest}
      />
      <WorkspaceHeader
        title={title}
        repoName={repoName}
        branchName={workspace?.branch_name}
        worktreePath={workspace?.worktree_path}
        loading={!title && !error}
        workflow={
          workspace && !isPutAway(workspace) ? (
            <WorkspaceWorkflowControl
              client={client}
              workspaceId={workspaceId}
              branchName={workspace.branch_name}
              baseRef={workspace.base_ref}
              fallbackPr={pr}
              resource={prResource}
              onOpenSourceControl={() =>
                setWorkspaceLayout(
                  openCodeEditor(
                    layoutRef.current,
                    { type: "source_control" },
                    splitFocused ? "secondary" : "primary",
                  ),
                )
              }
              onOpenWatchTask={
                prResource.data?.watch
                  ? () => openWorkspaceTask(prResource.data?.watch?.session_id)
                  : undefined
              }
            />
          ) : undefined
        }
        sessionStatus={
          session ? (
            <>
              <SessionAttentionBadge
                sessionId={session.id}
                client={client}
                fallback={digest?.attention ?? session.attention}
              />
              <PendingApprovalBadge sessionId={session.id} client={client} />
              <SessionLifecycleIndicator
                lifecycle={digest?.lifecycle ?? session.lifecycle}
                harness={session.harness_kind}
                version={session.harness_version}
                unrecognizedEventCount={session.unrecognized_event_count}
                runningLabel={
                  digest?.lifecycle === "running"
                    ? sessionActivityLabel(digest)
                    : undefined
                }
              />
              <span className="text-border" aria-hidden>
                ·
              </span>
              <SessionPermissionIndicator mode={session.permission_mode} />
            </>
          ) : undefined
        }
        terminalOpen={hasTerminal}
        reviewOpen={inspectorOpen}
        reviewUnavailableReason={
          inspectorFits
            ? undefined
            : "Too narrow for the review sidebar — open Source control or Pull request as a tab"
        }
        terminalShortcut={shortcutHints.terminal}
        reviewShortcut={shortcutHints.review}
        onToggleTerminal={toggleTerminal}
        onToggleReview={toggleReviewSidebar}
        overflowAction={
          workspace ? (
            <WorkspaceOverflowMenu
              commands={headerCommands}
              context={{
                repoName: repoName ?? undefined,
                worktreePath: workspace.worktree_path,
              }}
              onCommand={(command) => {
                if (command.id === "fork-agent") {
                  if (session) void forkConversation(session.id);
                  return;
                }
                run(command.id, {
                  workspace,
                  title: title ?? workspace.title,
                  session: session ?? undefined,
                  actionName: command.actionName,
                });
              }}
            />
          ) : undefined
        }
      />
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
      ) : (
        <div
          ref={inspectorPaneRef}
          data-testid="workspace-pane"
          className="flex h-full min-h-0 flex-1 overflow-hidden"
        >
          {inspectorOpen ? (
            <ResizablePanelGroup
              id={INSPECTOR_LAYOUT_STORAGE_ID}
              groupRef={inspectorGroupRef}
              defaultLayout={inspectorDefaultLayout}
              onLayoutChanged={inspectorLayout.onLayoutChanged}
              orientation="horizontal"
              className="h-auto min-h-0 max-w-full min-w-0 flex-1 overflow-clip"
            >
              <ResizablePanel
                id="workspace"
                defaultSize={String(DEFAULT_INSPECTOR_LAYOUT.workspace)}
                minSize={String(MIN_WORKSPACE_SIZE)}
                className="h-full min-h-0 min-w-0"
              >
                {workspaceMain}
              </ResizablePanel>
              <ResizableHandle className="bg-border-subtle transition-colors hover:bg-border" />
              <ResizablePanel
                id="inspector"
                defaultSize={String(DEFAULT_INSPECTOR_LAYOUT.inspector)}
                minSize={String(MIN_INSPECTOR_SIZE)}
                maxSize={String(MAX_INSPECTOR_SIZE)}
                className="h-full min-h-0 min-w-0 bg-page-background"
              >
                <ErrorBoundary
                  resetKey={workspaceId}
                  fallback={
                    <p className="text-muted-foreground p-4 text-sm">
                      The review sidebar could not load. Git and pull-request
                      details stay here when they are available.
                    </p>
                  }
                >
                  <CodeInspector
                    key={workspaceId}
                    client={client}
                    workspaceId={workspaceId}
                    workspace={workspace}
                    contentRevision={contentRevision}
                    prResource={prResource}
                    onOpenFile={openFile}
                    onOpenDiff={openFileDiff}
                    onClose={() => setReviewSidebarOpen(false)}
                  />
                </ErrorBoundary>
              </ResizablePanel>
            </ResizablePanelGroup>
          ) : (
            workspaceMain
          )}
        </div>
      )}
    </MarkdownLinkProvider>
  );
}

/**
 * The pointer sensor, minus the controls that live inside a tab.
 *
 * A tab's close button sits within the draggable, so without this a press that
 * drifts a few pixels would pick the tab up rather than close it. A control
 * opts itself out by carrying the marker attribute.
 */
class TabPointerSensor extends PointerSensor {
  static activators = [
    {
      eventName: "onPointerDown" as const,
      handler: (
        { nativeEvent: event }: ReactPointerEvent,
        { onActivation }: PointerSensorOptions,
      ) => {
        if (!event.isPrimary || event.button !== 0) return false;
        const target = event.target;
        if (!(target instanceof Element)) return true;
        if (target.closest('[data-no-drag="true"]')) return false;
        onActivation?.({ event });
        return true;
      },
    },
  ];
}

/**
 * The drop target under the pointer, with the nearest one as a fallback.
 *
 * A strip contains its tabs and overlaps the split zone, so it collides on
 * every drop that lands on either. Dropping it whenever something more specific
 * was hit is what makes a tab a reorder and the strip's open space an append.
 * The nearest-center fallback covers the frames where a fast drag has the
 * pointer outside every registered box.
 */
const tabDropTarget: CollisionDetection = (args) => {
  const under = pointerWithin(args);
  const collisions = under.length > 0 ? under : closestCenter(args);
  const specific = collisions.filter(
    (collision) => !isEditorStripDropId(String(collision.id)),
  );
  return specific.length > 0 ? specific : collisions;
};

/** The mid-drag target that offers to open the tab beside the conversation. */
function SplitDropZone() {
  const { isOver, setNodeRef } = useDroppable({ id: EDITOR_SPLIT_DROP_ID });
  return (
    <div
      ref={setNodeRef}
      data-testid="split-drop-zone"
      data-over={isOver ? "true" : undefined}
      className="workspace-split-drop-zone absolute inset-y-3 right-3 z-10 flex w-[min(40%,22rem)] flex-col items-center justify-center gap-2 rounded-xl border border-border bg-background/92 px-6 text-center shadow-lg backdrop-blur-md data-[over=true]:border-ring data-[over=true]:bg-accent/60"
    >
      <span className="grid size-9 place-items-center rounded-lg bg-muted text-foreground">
        <span className="grid grid-cols-2 gap-0.5" aria-hidden>
          <span className="h-4 w-2 rounded-[2px] bg-foreground/25" />
          <span className="h-4 w-2 rounded-[2px] bg-foreground" />
        </span>
      </span>
      <span className="text-sm font-semibold">Open beside the agent</span>
      <span className="max-w-44 text-xs leading-relaxed text-muted-foreground">
        Drop here to create a working pane on the right.
      </span>
    </div>
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

function storedBrowserTitles(layout: LayoutState): Record<string, string> {
  return Object.fromEntries(
    codeBrowserIds(layout).map((browserId) => [
      browserId,
      storedBrowserTitle(browserId),
    ]),
  );
}

/**
 * Shells one workspace may hold at once, matching the server's own cap. The
 * plus menu stops offering another rather than letting the create fail.
 */
const MAX_WORKSPACE_TERMINALS = 8;

/**
 * Track one element's width, in CSS pixels.
 *
 * The width stays `null` until an observer reports, so a caller can tell
 * "not measured yet" from "measured and narrow" and avoid deciding on a zero
 * it read before layout ran. The callback ref re-attaches whenever the
 * element behind it changes, which is what keeps the reading live across the
 * split going up and coming down.
 */
function useMeasuredWidth(): {
  paneRef: (element: HTMLElement | null) => void;
  width: number | null;
} {
  const [width, setWidth] = useState<number | null>(null);
  const observerRef = useRef<ResizeObserver | null>(null);

  useEffect(() => () => observerRef.current?.disconnect(), []);

  const paneRef = useCallback((element: HTMLElement | null) => {
    observerRef.current?.disconnect();
    observerRef.current = null;
    if (!element || typeof ResizeObserver === "undefined") return;
    setWidth(element.getBoundingClientRect().width);
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    observerRef.current = observer;
  }, []);

  return { paneRef, width };
}

/**
 * Give every open shell a tab label, keeping the ones already assigned.
 *
 * Several shells in a strip all reading "Terminal" would be untellable apart,
 * so each takes the lowest number no other open shell is using. A number is
 * released when its tab closes, which is also when its shell ends.
 */
function nameTerminals(
  current: Readonly<Record<string, string>>,
  terminalIds: readonly string[],
): Record<string, string> {
  const kept: Record<string, string> = {};
  for (const id of terminalIds) {
    const existing = current[id];
    if (existing) kept[id] = existing;
  }
  const taken = new Set(Object.values(kept));
  for (const id of terminalIds) {
    if (kept[id]) continue;
    let ordinal = 1;
    while (taken.has(`Terminal ${ordinal}`)) ordinal += 1;
    kept[id] = `Terminal ${ordinal}`;
    taken.add(kept[id]);
  }
  const unchanged =
    Object.keys(kept).length === Object.keys(current).length &&
    Object.entries(kept).every(([id, label]) => current[id] === label);
  return unchanged ? (current as Record<string, string>) : kept;
}

/** Native child webviews must yield whenever a portaled app surface overlaps them. */

function shortcutHint(id: ShellShortcutAction, command: boolean): string {
  const def = SHELL_SHORTCUTS.find((item) => item.id === id);
  return def ? shortcutKeycaps(def, command).join("") : "";
}

/**
 * The side region beside the conversation.
 *
 * Every panel it used to draw has become a center tab, so a link naming one
 * lands here and says so rather than rendering an empty frame.
 */
function renderCodePanel() {
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
  const attention = live ?? fallback;
  // The lifecycle indicator owns live motion in this header. Keep the
  // attention mark for states that carry separate information.
  if (attention?.state.type === "working") return null;
  return <AttentionBadge compact attention={attention} />;
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
  const noun = pending === 1 ? "approval" : "approvals";
  return (
    <button
      type="button"
      data-testid="pending-approval-badge"
      // The count alone names a state, not a control. This button scrolls the
      // transcript to the first parked approval, so its name says so.
      aria-label={`Jump to ${pending} pending ${noun}`}
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
        {pending} {noun}
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
  onForkFromTurn,
  subagentCallId,
  subagentSummary,
  onBackFromSubagent,
  composerOverride,
}: {
  session: CodeSessionSnapshot;
  workspaceId: string;
  client: ApiClient;
  catalogModels: ModelInfo[];
  defaultModelKey: string | null;
  disabled: boolean;
  /** Scope the review sidebar to one turn's changes, from a turn's diffstat. */
  onOpenTurnDiff?: (turnId: string) => void;
  /** Fork this conversation at the end of one turn, from its seam row. */
  onForkFromTurn?: (turnId: string) => void;
  /** The spanning Task call to inspect inside this still-mounted session. */
  subagentCallId?: string;
  /** Current bounded rail summary, when the Task is still in the digest. */
  subagentSummary?: CodeSubagentSummary;
  onBackFromSubagent?: () => void;
  /**
   * Replace the composer. A watch task's transcript is read-along: the sweep
   * drives its turns, so the seat where the user would type carries the watch
   * controls instead.
   */
  composerOverride?: ReactNode;
}) {
  const follow = useTranscriptFollow();
  const store = useRegisteredCodeSession(session.id, client);
  const firstTurnRecovery = useFirstTurnRecovery(client, session.id);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const hydrated = store((state) => state.hydrated);
  const animateStreaming = store((state) => state.animateStreaming);
  const connectionState = store((state) => state.connectionState);
  const lastUsage = store((state) => state.lastUsage);
  // The reducer's own applied-event cursor is the activity signal the stall
  // timer wants: every delta, tool result, and boundary advances it.
  const lastSeq = store((state) => state.lastSeq);
  const transcriptSubagent = useMemo(
    () =>
      subagentCallId
        ? subagentSummaryFromTranscript(items, subagentCallId)
        : null,
    [items, subagentCallId],
  );
  const selectedSubagent = subagentCallId
    ? (subagentSummary ?? transcriptSubagent)
    : null;
  const transcriptItems = useMemo(
    () =>
      subagentCallId
        ? subagentTranscriptItems(items, subagentCallId)
        : mainAgentTranscriptItems(items),
    [items, subagentCallId],
  );
  const transcriptBusy = subagentCallId
    ? selectedSubagent?.status === "running"
    : busy;
  const streamStalled = useStreamStalled(transcriptBusy, lastSeq);
  const lifecycle = store((state) => state.lifecycle) ?? session.lifecycle;
  const [approvals, setApprovals] = useState<
    Record<string, CodeApprovalSnapshot>
  >({});
  const [decidingId, setDecidingId] = useState<string | null>(null);
  const [approvalError, setApprovalError] = useState<string | undefined>();
  const sessionQueue = useCodeQueueApi(client, session.id);
  // No `?? []` fallback here: a fresh array is a new snapshot every render,
  // and zustand v5 loops on referentially unstable snapshots.
  const cachedModels = useCodeCatalogStore(
    (state) => state.modelsByHarness[session.harness_kind],
  );
  const rememberHarnessModels = useCodeCatalogStore(
    (state) => state.rememberHarnessModels,
  );
  // The ladder a code session runs on belongs to the engine, not to whichever
  // catalog the model row came from.
  const engineEfforts =
    useCodeCatalogStore(
      (state) => state.effortsByHarness[session.harness_kind],
    ) ?? EMPTY_EFFORTS;
  const modelOptions = useMemo(() => {
    const gateway = gatewayCodeModels(
      catalogModels,
      session.harness_kind,
      defaultModelKey,
    );
    const listed =
      requiresHarnessModelIds(session.harness_kind) &&
      cachedModels === undefined
        ? []
        : preferredCodeModels(
            session.harness_kind,
            cachedModels ?? [],
            gateway,
          );
    if (
      !session.model ||
      listed.some((option) => option.id === session.model)
    ) {
      return listed;
    }
    // Historical or engine-default sessions can name a model that is hidden
    // from today's catalog. Keep that truthful current model visible instead
    // of silently labeling the session as whichever row is now default.
    return [
      ...harnessCodeModels(
        [{ id: session.model, label: session.model }],
        session.harness_kind,
      ),
      ...listed,
    ];
  }, [
    cachedModels,
    catalogModels,
    defaultModelKey,
    session.harness_kind,
    session.model,
  ]);
  const inferred = modelOptions.find((option) => option.default)?.id;
  const [model, setModel] = useState(session.model ?? inferred);
  // The recap is derived after a turn completes and published on the digest
  // channel rather than the journal, so the transcript reads it from here
  // instead of from an item the reducer built.
  const sessionDigest = useSessionDigest(workspaceId, session.id);
  type SessionSettings = {
    permissionMode: PermissionMode;
    reasoningEffort: ReasoningEffort | null;
    fastMode: boolean;
  };
  const settingsFromSession = useCallback(
    (snapshot: CodeSessionSnapshot): SessionSettings => ({
      permissionMode: snapshot.permission_mode,
      reasoningEffort: snapshot.reasoning_effort ?? null,
      fastMode: snapshot.fast_mode,
    }),
    [],
  );
  const initialSettings: SessionSettings = {
    permissionMode: session.permission_mode,
    reasoningEffort: session.reasoning_effort ?? null,
    fastMode: session.fast_mode,
  };
  // One confirmed baseline plus ordered optimistic patches keeps a full
  // response from an older write from erasing a choice that is still queued.
  const [settings, setSettings] = useState(initialSettings);
  const settingsRef = useRef(initialSettings);
  const confirmedSettingsRef = useRef(initialSettings);
  const pendingSettingsWritesRef = useRef(
    new Map<number, Partial<SessionSettings>>(),
  );
  const settingsWriteQueueRef = useRef<Promise<void>>(Promise.resolve());
  const settingsWriteGenerationRef = useRef(0);
  const settingsScopeRef = useRef(session.id);
  const [settingsPending, setSettingsPending] = useState(false);
  const pendingReasoningEffortRef = useRef<{
    value: ReasoningEffort | null;
  } | null>(null);
  const reconcileSettings = useCallback(() => {
    let next = { ...confirmedSettingsRef.current };
    for (const patch of pendingSettingsWritesRef.current.values()) {
      next = { ...next, ...patch };
    }
    const pendingReasoningEffort = pendingReasoningEffortRef.current;
    if (pendingReasoningEffort) {
      next.reasoningEffort = pendingReasoningEffort.value;
    }
    settingsRef.current = next;
    setSettings(next);
  }, []);

  function queueSettingsWrite(
    patch: Partial<SessionSettings>,
    write: () => Promise<CodeSessionSnapshot>,
    failureMessage: string,
  ) {
    const scope = session.id;
    const generation = ++settingsWriteGenerationRef.current;
    pendingSettingsWritesRef.current.set(generation, patch);
    reconcileSettings();
    setSettingsPending(true);

    const result = settingsWriteQueueRef.current.then(() => {
      if (settingsScopeRef.current !== scope) return null;
      return write();
    });
    settingsWriteQueueRef.current = result.then(
      () => undefined,
      () => undefined,
    );
    void result.then(
      (updated) => {
        if (!updated || settingsScopeRef.current !== scope) return;
        confirmedSettingsRef.current = settingsFromSession(updated);
        pendingSettingsWritesRef.current.delete(generation);
        reconcileSettings();
        setSettingsPending(pendingSettingsWritesRef.current.size > 0);
      },
      (err) => {
        if (settingsScopeRef.current !== scope) return;
        pendingSettingsWritesRef.current.delete(generation);
        reconcileSettings();
        setSettingsPending(pendingSettingsWritesRef.current.size > 0);
        toast.error(friendlyErrorMessage(err, failureMessage));
      },
    );
  }

  useEffect(() => {
    setModel(session.model ?? inferred);
  }, [inferred, session.model]);

  useEffect(() => {
    if (settingsScopeRef.current !== session.id) {
      settingsScopeRef.current = session.id;
      settingsWriteGenerationRef.current += 1;
      pendingSettingsWritesRef.current.clear();
      settingsWriteQueueRef.current = Promise.resolve();
      pendingReasoningEffortRef.current = null;
      setSettingsPending(false);
    }
    // A refreshed row can still carry the stored effort while a mid-turn
    // choice waits for its first submission. Reconciliation keeps that choice
    // and any queued writes on top of the confirmed row.
    confirmedSettingsRef.current = settingsFromSession(session);
    reconcileSettings();
  }, [
    reconcileSettings,
    session.id,
    session.permission_mode,
    session.reasoning_effort,
    session.fast_mode,
    settingsFromSession,
  ]);

  useEffect(() => {
    // An empty list is a finished fetch: this engine advertised no models.
    // Treating [] as "not yet loaded" remembers a new [] forever.
    //
    // The fetch runs even when a gateway catalog already supplies the rows,
    // because this route is also where the engine's effort ladder comes from
    // and a gateway row carries the chat catalog's instead.
    if (cachedModels !== undefined) return;
    let cancelled = false;
    void client.listCodeHarnessModels(session.harness_kind).then(
      (listed) => {
        if (cancelled) return;
        rememberHarnessModels(
          session.harness_kind,
          harnessCodeModels(listed.models, session.harness_kind),
          listed.reasoning_efforts,
        );
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [cachedModels, client, rememberHarnessModels, session.harness_kind]);
  const doctorEntry = useCodeCatalogStore(
    (state) =>
      state.doctor?.harnesses.find(
        (entry) => entry.kind === session.harness_kind,
      ) ?? null,
  );
  // Doctor caps decide what this engine's picker offers; without a doctor
  // row yet, show everything and let the server refuse.
  const availableModes: PermissionMode[] = doctorEntry
    ? createPermissionModes(doctorEntry.caps)
    : ["plan", "ask", "auto", "allow"];
  const steeringSupported = doctorEntry?.caps.mid_turn_steering === "supported";
  const turnRunning = busy || lifecycle === "running";
  const composerHistory = useMemo(
    () =>
      items
        .flatMap((item) =>
          item.kind === "user" && item.text.trim() ? [item.text] : [],
        )
        .reverse(),
    [items],
  );

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

  // This pane re-renders on every streamed delta, so a callback written inline
  // in the transcript's props would be a new identity each time and would
  // re-render every row in the transcript with it.
  const decideApproval = useCallback(
    async (
      approvalId: string,
      decision: "approve" | "deny",
      feedback?: string,
    ) => {
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
    },
    [client],
  );

  function send(
    message: string,
    attachments?: readonly { blob_id: string; media_type: string }[],
  ) {
    const pendingReasoningEffort = pendingReasoningEffortRef.current;
    const recoveryAtSend = firstTurnRecovery;
    // Sending is a deliberate return to the tail: whatever the reader was
    // reading, they now want to watch their own turn run.
    follow.armFollow();
    follow.requestSmoothFollow();
    // Outcome and refusal both belong to the composer: it says whether the
    // message ran or queued, and it holds the draft when the server refuses.
    // A queued outcome needs no state here — the tray polls the durable queue
    // and shows the row.
    return submitAcceptedTurn(store.getState().update, () =>
      pendingReasoningEffort
        ? client.submitCodeTurn(
            session.id,
            message,
            model ?? undefined,
            attachments,
            pendingReasoningEffort.value,
          )
        : client.submitCodeTurn(
            session.id,
            message,
            model ?? undefined,
            attachments,
          ),
    ).then((outcome) => {
      if (pendingReasoningEffortRef.current === pendingReasoningEffort) {
        pendingReasoningEffortRef.current = null;
      }
      if (recoveryAtSend?.status === "failed") {
        clearFirstTurnRecovery(client, session.id, recoveryAtSend.id);
      }
      return outcome;
    });
  }

  function changePermissionMode(mode: PermissionMode) {
    queueSettingsWrite(
      { permissionMode: mode },
      () => client.setCodeSessionPermissionMode(session.id, mode),
      "Could not change the mode",
    );
  }

  function changeReasoningEffort(effort: ReasoningEffort | null) {
    // A running turn keeps the effort it started with. The selected level
    // rides on the next submission, where the server also makes it sticky.
    if (turnRunning) {
      pendingReasoningEffortRef.current = { value: effort };
      settingsRef.current = { ...settingsRef.current, reasoningEffort: effort };
      setSettings(settingsRef.current);
      return;
    }
    pendingReasoningEffortRef.current = null;
    queueSettingsWrite(
      { reasoningEffort: effort },
      () => client.setCodeSessionReasoningEffort(session.id, effort),
      "Could not change the reasoning",
    );
  }

  function changeFastMode(fastMode: boolean) {
    queueSettingsWrite(
      { fastMode },
      () => client.setCodeSessionFastMode(session.id, fastMode),
      "Could not change fast mode",
    );
  }

  async function steer(message: string) {
    const expectedTurnId = store.getState().activeTurnId;
    if (!expectedTurnId) {
      throw new Error("The active turn changed. Try Redirect again.");
    }
    await client.steerCodeSession(session.id, expectedTurnId, message);
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
      {subagentCallId && (
        <SubagentContextBar
          name={selectedSubagent?.name ?? "Subagent unavailable"}
          status={selectedSubagent?.status ?? "unavailable"}
          onBack={onBackFromSubagent}
        />
      )}
      <div className={cn("message-view", follow.fadeClass)}>
        {connectionState === "reconnecting" && (
          <p
            role="status"
            className="text-info-foreground pointer-events-none absolute inset-x-0 top-2 z-[1] text-center text-xs [animation:code-reveal_140ms_ease-out] motion-reduce:animate-none"
          >
            Reconnecting to the session…
          </p>
        )}
        <CodeTranscript
          items={transcriptItems}
          sessionId={session.id}
          hydrated={hydrated}
          busy={transcriptBusy}
          streamStalled={streamStalled}
          animateStreaming={animateStreaming}
          approvals={approvals}
          decidingId={decidingId}
          approvalError={approvalError}
          onOpenTurnDiff={onOpenTurnDiff}
          onForkFromTurn={subagentCallId ? undefined : onForkFromTurn}
          onReveal={follow.pauseFollow}
          scrollRef={follow.scrollRef}
          contentRef={follow.contentRef}
          onScroll={follow.onScroll}
          onDecide={decideApproval}
          recap={sessionDigest?.recap}
          emptyState={
            subagentCallId
              ? subagentEmptyState(selectedSubagent?.status)
              : undefined
          }
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
      {composerOverride}
      {lifecycle !== "ended" && !composerOverride && !subagentCallId && (
        <>
          <div className="shrink-0 px-[clamp(0.5rem,4%,5rem)]">
            <QueueTray
              queue={sessionQueue}
              active={turnRunning}
              onStop={interrupt}
            />
          </div>
          <CodeComposer
            running={turnRunning}
            disabled={disabled || firstTurnRecovery?.status === "sending"}
            permissionMode={settings.permissionMode}
            availableModes={availableModes}
            reasoningEffort={settings.reasoningEffort}
            fastMode={settings.fastMode}
            settingsPending={settingsPending}
            engineEfforts={engineEfforts}
            harness={session.harness_kind}
            model={model ?? undefined}
            modelOptions={modelOptions}
            modelLoading={
              requiresHarnessModelIds(session.harness_kind) &&
              cachedModels === undefined
            }
            promptScope={workspaceId}
            sessionId={session.id}
            history={composerHistory}
            slashCommands={doctorEntry?.commands}
            searchPaths={(query) =>
              client
                .listCodeWorkspaceTree(workspaceId, { query })
                .then((tree) => tree.paths)
            }
            workspaceFiles={
              firstTurnRecovery?.forkSource
                ? {
                    items: [forkTranscriptFile(firstTurnRecovery.forkSource)],
                    onRemove: () =>
                      updateFirstTurnRecovery(
                        client,
                        session.id,
                        firstTurnRecovery.id,
                        (current) => ({ ...current, forkSource: null }),
                      ),
                  }
                : undefined
            }
            recovery={
              firstTurnRecovery
                ? {
                    id: firstTurnRecovery.id,
                    draft: firstTurnRecovery.draft,
                  }
                : undefined
            }
            onModelChange={setModel}
            onModeChange={
              doctorEntry?.relaunch_composes_permission_mode === false &&
              session.harness_resume_ref
                ? undefined
                : changePermissionMode
            }
            onEffortChange={
              doctorEntry?.caps.reasoning_levels === "unsupported"
                ? undefined
                : changeReasoningEffort
            }
            onFastModeChange={changeFastMode}
            contextUsage={
              lastUsage
                ? {
                    // The engine's own reading of the prompt still resident
                    // after its last model call. The four counts below are the
                    // turn's spend across every call, which on a long turn runs
                    // to several times this.
                    contextTokens: lastUsage.context_tokens,
                    spend: {
                      input: lastUsage.input_tokens,
                      output: lastUsage.output_tokens,
                      cacheRead: lastUsage.cache_read_input_tokens,
                      cacheWrite: lastUsage.cache_creation_input_tokens,
                    },
                    contextWindow: catalogModels.find(
                      (entry) => entry.id === model || entry.key === model,
                    )?.context_window,
                    modelName:
                      modelOptions.find((option) => option.id === model)
                        ?.label ??
                      model ??
                      undefined,
                  }
                : null
            }
            onSend={send}
            onSteer={steeringSupported ? steer : undefined}
            onInterrupt={interrupt}
          />
          {firstTurnRecovery && (
            <p
              role={firstTurnRecovery.status === "failed" ? "alert" : "status"}
              className={cn(
                "mx-auto w-full max-w-3xl px-2 pt-1 text-xs",
                firstTurnRecovery.status === "failed"
                  ? "text-critical-foreground"
                  : "text-muted-foreground",
              )}
            >
              {firstTurnRecovery.message}
            </p>
          )}
        </>
      )}
    </div>
  );
}

function useRegisteredCodeSession(sessionId: string, client: ApiClient) {
  const storeRef = useRef<ReturnType<
    typeof acquireCodeSessionFromClient
  > | null>(null);
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

type SubagentViewStatus = CodeSubagentStatus | "unavailable";

function subagentSummaryFromTranscript(
  items: readonly CodeTranscriptItem[],
  callId: string,
): CodeSubagentSummary | null {
  const task = items.find(
    (item) =>
      item.kind === "tool" &&
      item.parentCallId === null &&
      item.callId === callId &&
      item.name === "Task",
  );
  if (!task || task.kind !== "tool") return null;
  return {
    call_id: callId,
    name: toolDetailSubject(task.detail) || task.name,
    status:
      task.status === "running"
        ? "running"
        : task.status === "succeeded"
          ? "done"
          : "failed",
  };
}

function toolDetailSubject(
  detail: Extract<CodeTranscriptItem, { kind: "tool" }>["detail"],
): string;
function toolDetailSubject(
  detail: Extract<CodeTranscriptItem, { kind: "tool" }>["detail"],
): string {
  switch (detail.kind) {
    case "command":
      return detail.cmd;
    case "file_read":
    case "file_edit":
      return detail.path;
    case "search":
      return detail.query;
    case "other":
      return detail.summary;
  }
}

function subagentEmptyState(status: CodeSubagentStatus | undefined): {
  title: string;
  description: string;
} {
  switch (status) {
    case "running":
      return {
        title: "Waiting for this subagent",
        description:
          "It is still running, but it has not produced attributed transcript output yet.",
      };
    case "done":
      return {
        title: "No captured subagent output",
        description:
          "This subagent completed without leaving attributed assistant or tool activity.",
      };
    case "failed":
      return {
        title: "No captured subagent output",
        description:
          "This subagent ended before attributed assistant or tool activity was captured.",
      };
    default:
      return {
        title: "Subagent unavailable",
        description:
          "This link no longer matches a captured Task in the parent session.",
      };
  }
}

function SubagentContextBar({
  name,
  status,
  onBack,
}: {
  name: string;
  status: SubagentViewStatus;
  onBack?: () => void;
}) {
  const label =
    status === "running"
      ? "Running"
      : status === "done"
        ? "Completed"
        : status === "failed"
          ? "Failed"
          : "Unavailable";
  const variant =
    status === "running"
      ? "info"
      : status === "done"
        ? "success"
        : status === "failed"
          ? "critical"
          : "outline";
  return (
    <div
      className="border-border-subtle bg-background/85 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-center gap-2 rounded-lg border px-3 py-2 shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_4%,transparent)]"
      data-testid="subagent-context-bar"
    >
      <span className="grid size-7 shrink-0 place-items-center text-muted-foreground">
        <Bot className="size-3.5" aria-hidden />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate text-xs font-semibold" title={name}>
            {name}
          </span>
          <Badge variant={variant} size="sm" className="shrink-0">
            {label}
          </Badge>
        </div>
        <p className="text-muted-foreground text-xs">Read-only subagent view</p>
      </div>
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        Back to main agent
      </Button>
    </div>
  );
}

/**
 * The watch task's seat in the transcript view: the sweep drives this
 * session's turns, so instead of a composer the reader gets what the watch
 * is doing and the two decisions that are theirs — stop it, or go back.
 */
function WatchTaskBar({
  client,
  workspaceId,
  watch,
  onBack,
  onStopped,
}: {
  client: Pick<ApiClient, "stopCodeWatch">;
  workspaceId: string;
  watch: CodeWatchSnapshot | undefined;
  onBack: () => void;
  onStopped: () => void;
}) {
  const [stopping, setStopping] = useState(false);
  const active =
    watch !== undefined &&
    (watch.state === "watching" ||
      watch.state === "fixing" ||
      watch.state === "blocked");
  const label = !watch
    ? "This watch task has finished."
    : watch.state === "fixing"
      ? `Fixing PR #${watch.pr_number}${watch.cycles > 0 ? ` · fix turn ${watch.cycles}` : ""}`
      : watch.state === "blocked"
        ? `Watch blocked${watch.detail ? `: ${watch.detail}` : ""}`
        : watch.state === "watching"
          ? `Watching PR #${watch.pr_number}${watch.detail ? ` · ${watch.detail}` : ""}`
          : `Watch ${watch.state}${watch.detail ? `: ${watch.detail}` : ""}`;
  return (
    <div
      className="border-border-subtle bg-background/80 mx-auto mb-3 flex w-full max-w-3xl items-center gap-2 rounded-md border px-3 py-2 text-xs"
      data-testid="watch-task-bar"
    >
      <CircleDotDashed
        className={cn(
          "size-3.5 shrink-0",
          watch?.state === "blocked"
            ? STATUS_MARK.warning
            : STATUS_MARK.pending,
        )}
        aria-hidden
      />
      <span className="min-w-0 flex-1 truncate" title={watch?.detail}>
        {label}
      </span>
      {active && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={stopping}
          onClick={() => {
            setStopping(true);
            client
              .stopCodeWatch(workspaceId)
              .then(() => onStopped())
              .catch((err) => {
                toast.error(
                  friendlyErrorMessage(err, "Could not stop the watch"),
                );
              })
              .finally(() => setStopping(false));
          }}
        >
          {stopping ? "Stopping…" : "Stop watching"}
        </Button>
      )}
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        Back to main task
      </Button>
    </div>
  );
}

/**
 * A tab label for one of a workspace's agents.
 *
 * The first agent is the one the workspace was started with, so it keeps the
 * name the rest of the surface uses. The others are named by engine, numbered
 * only when the same engine runs more than once.
 */
function conversationTabLabel(
  session: CodeSessionSnapshot,
  index: number,
  sessions: readonly CodeSessionSnapshot[],
): string {
  if (index === 0) return "Main agent";
  const label = HARNESS_LABELS[session.harness_kind];
  const same = sessions.filter(
    (entry, at) => at > 0 && entry.harness_kind === session.harness_kind,
  );
  if (same.length < 2) return label;
  return `${label} ${same.indexOf(session) + 1}`;
}
