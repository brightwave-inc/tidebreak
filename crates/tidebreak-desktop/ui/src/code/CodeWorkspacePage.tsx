import { Button } from "@/components/ui/button";
import {
  centerEditorTabId,
  CenterTabIcon,
  centerTabParts,
  CHAT_PANEL_ID,
  CodeCenterTabs,
  type CodeConversationTab,
  conversationTabId,
  EDITOR_PANEL_ID,
  SPLIT_EDITOR_PANEL_ID,
} from "./CodeCenterTabs";
import {
  adoptCodeTerminalId,
  closeAllEditorTabs,
  closeCodeChromeTab,
  closeEditorTab,
  closeEditorTabsToRight,
  closeOtherEditorTabs,
  codeBrowserIds,
  type CodeEditorRegion,
  codeTerminalIds,
  findCodeTerminalTab,
  focusCodeChromeTab,
  focusConversation,
  focusedEditorPosition,
  focusEditorTab,
  mergeEditorSplit,
  moveEditorTab,
  openCodeEditor,
  removedCodeBrowserIds,
  removedCodeTerminalIds,
  reorderEditorTab,
  splitCodeChromeLayout,
} from "./codeChrome";
import type {
  CodeForkTranscript,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  PermissionMode,
  ReasoningEffort,
} from "../api/types";
import { CodeInspector, WorkspaceDeliveryPrTab } from "./CodeInspector";
import { CodeQuickOpen } from "./CodeQuickOpen";
import { CodeSessionPane } from "./workspace/CodeSessionPane";
import { CodeSidebar } from "./CodeSidebar";
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
import { DiffOverview } from "./DiffOverview";
import { DiffPanel } from "./DiffPanel";
import {
  DndContext,
  type DragEndEvent,
  DragOverlay,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import { ErrorBoundary } from "@/ErrorBoundary";
import {
  clearFirstTurnRecovery,
  type FirstTurnRecovery,
  writeFirstTurnRecovery,
} from "./workspace/firstTurnRecovery";
import type { LayoutState, PanelContent } from "@/panel/panelTypes";
import {
  browserTitlesForLayout,
  conversationTabLabel,
  MAX_WORKSPACE_TERMINALS,
  nameTerminals,
  renderCodePanel,
  useCodeShortcutHints,
  useMeasuredWidth,
} from "./workspace/layout";
import { MarkdownLinkProvider } from "@/MessageMarkdown";
import { PanelLayout } from "@/panel/PanelLayout";
import {
  PendingApprovalBadge,
  SessionAttentionBadge,
} from "./workspace/badges";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import { RouteFrame } from "@/RouteFrame";
import { SessionLifecycleIndicator } from "./SessionLifecycleIndicator";
import { SessionPermissionIndicator } from "./SessionPermissionIndicator";
import { Skeleton } from "@/components/ui/skeleton";
import {
  SplitDropZone,
  tabDropTarget,
  TabPointerSensor,
} from "./workspace/tabDrag";
import {
  StartSessionPrompt,
  WorkspaceSessionStartingState,
} from "./StartSessionPrompt";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { TerminalPane } from "./TerminalPane";
import { WatchTaskBar } from "./workspace/subagents";
import { WorkspaceHeader } from "./WorkspaceHeader";
import {
  openWorkspaceFileInEditor,
  useWorkspaceCardCommands,
  workspaceHeaderCommands,
  WorkspaceOverflowMenu,
} from "./workspaceActions";
import { WorkspaceWorkflowControl } from "./WorkspaceWorkflowControl";
import { attachedRemotely } from "@/host";
import { attentionMarkForDigest } from "./statusTone";
import { canOpenInExternalEditor } from "./codeWorktreeHost";
import { closeCodeBrowser } from "./browser/browserHost";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { copyPlainText } from "@/ClipboardCopyButton";
import { dropEditorTab, findEditorPanel, offersSplitDrop } from "./editorDrag";
import {
  fenceReasonText,
  gatewayCodeModels,
  preferredCodeModels,
  requiresHarnessModelIds,
} from "./labels";
import { forkFraming, forkTranscriptFile } from "./fork";
import { hasLocalHostAuthority } from "../host";
import { isPutAway, sessionActivityLabel } from "./workspaceCards";
import { liveCodeSessions } from "./parsers";
import { publishCodeImage } from "../attachments";
import { seedBrowserSession } from "./browser/browserPersistence";
import { tidebreakProductRepo } from "./uneffMe";
import { toast } from "sonner";
import { uploadImageAttachment } from "../ImageAttachments";
import { useApp } from "@/AppContext";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeContentRevision } from "./useLiveContent";
import { useCodeUiStore } from "./CodeUiStore";
import {
  useCodeUpdatesStore,
  useConversationDigests,
  useSessionDigest,
} from "./CodeUpdatesStore";
import { useCodeWorkspacePr } from "./useCodeWorkspacePr";
import { useDefaultLayout, useGroupRef } from "react-resizable-panels";
import { useLayoutState, usePanelNav } from "@/panel/usePanelNav";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { usePortalOverlayOpen } from "@/lib/usePortalOverlayOpen";

const FileViewer = lazy(async () => {
  const module = await import("./FileViewer");
  return { default: module.FileViewer };
});

const CodeBrowserTab = lazy(async () => {
  const module = await import("./browser/CodeBrowserTab");
  return { default: module.CodeBrowserTab };
});

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
  const [closedConversationIds, setClosedConversationIds] = useState<
    Set<string>
  >(() => new Set());

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
    () => browserTitlesForLayout(layout),
  );
  const [browserInitialUrls, setBrowserInitialUrls] = useState<
    Record<string, string>
  >({});
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
  const workspaceStartup = useCodeUiStore(
    (state) => state.workspaceStartups[workspaceId] ?? null,
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
        const title = current[browserId] ?? "Browser";
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

  // `?task=` names the session to show: a sibling agent, a watch child
  // opened from the rail, or an archived conversation from search. The param
  // is a request, not a fact — a link outlives the agent it points at, so it
  // holds only while that agent is still listed.
  const namedTask = useMemo(() => {
    if (!taskParam) return null;
    return sessions.find((entry) => entry.id === taskParam) ?? null;
  }, [sessions, taskParam]);

  // A param that names no listed session is stale. Drop it so the fallback
  // below runs and the URL stops naming an agent that is not there. Replace
  // rather than push, so Back does not lead to the same dead link. An ended
  // session is still listed, so archive search can open its transcript.
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

      const heldImages = useCodeUiStore
        .getState()
        .takeComposerImages(workspaceId);

      const recovery: FirstTurnRecovery = {
        id: `${created.id}:${request}`,
        sessionId: created.id,
        draft,
        forkSource: startedWithFork,
        message: "Sending your first message…",
        status: "sending",
      };
      if (!isCurrent()) {
        if (heldImages && heldImages.length > 0) {
          useCodeUiStore
            .getState()
            .offerComposerPrompt(workspaceId, draft, heldImages);
        }
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
        const attachments = heldImages?.length
          ? await Promise.all(
              heldImages.map(async (file) => {
                const published = hasLocalHostAuthority()
                  ? await publishCodeImage(created.id, file)
                  : await uploadImageAttachment(
                      startedWithClient,
                      created.id,
                      file,
                      {
                        onProgress: () => undefined,
                        signal: new AbortController().signal,
                        path: (id) =>
                          `/code/sessions/${encodeURIComponent(id)}/attachments/images`,
                      },
                    );
                return {
                  blob_id: published.attachmentId,
                  media_type: published.mediaType,
                };
              }),
            )
          : [];
        if (attachments.length > 0) {
          await startedWithClient.submitCodeTurn(
            created.id,
            message,
            undefined,
            attachments,
          );
        } else {
          await startedWithClient.submitCodeTurn(created.id, message);
        }
        clearFirstTurnRecovery(startedWithClient, created.id, recovery.id);
      } catch (err) {
        if (heldImages && heldImages.length > 0) {
          useCodeUiStore
            .getState()
            .offerComposerPrompt(workspaceId, draft, heldImages);
        }
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
    seedBrowserSession({
      browserId,
      workspaceId,
      initialUrl: url,
    });
    if (url) {
      setBrowserInitialUrls((current) => ({
        ...current,
        [browserId]: url,
      }));
    }
    setBrowserTitles((current) => ({
      ...current,
      [browserId]: "Browser",
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

  /** Close a conversation tab without ending its agent or workspace. */
  function closeConversation(sessionId: string | null) {
    if (sessionId === null) {
      setDraftAgent(false);
      setForkSource(null);
    } else {
      const index = conversations.findIndex((entry) => entry.id === sessionId);
      if (index <= 0) return;
      setClosedConversationIds((current) => {
        const next = new Set(current);
        next.add(sessionId);
        return next;
      });
      if (sessionId !== activeSessionId) return;
    }
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
    const tabs: CodeConversationTab[] = conversations
      .filter(
        (entry, index) => index === 0 || !closedConversationIds.has(entry.id),
      )
      .map((entry) => {
        const index = conversations.findIndex(
          (conversation) => conversation.id === entry.id,
        );
        const digest = conversationDigests[entry.id];
        return {
          id: entry.id,
          label: conversationTabLabel(entry, index, conversations),
          harness: entry.harness_kind,
          attention: attentionMarkForDigest(digest),
          closable: index > 0,
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
  }, [closedConversationIds, conversationDigests, conversations, draftAgent]);

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
              initialUrl={browserInitialUrls[panel.browserId]}
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
            <WorkspaceDeliveryPrTab
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
              {workspaceStartup &&
                workspace?.status === "active" &&
                !draftAgent && (
                  <WorkspaceSessionStartingState startup={workspaceStartup} />
                )}
              {!workspaceStartup &&
                startingNewAgent &&
                workspace?.status === "active" && (
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
              {!workspaceStartup && session && !draftAgent && (
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
              onOpenPr={() =>
                setWorkspaceLayout(
                  openCodeEditor(
                    layoutRef.current,
                    { type: "pr" },
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
          session && !workspaceStartup ? (
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
