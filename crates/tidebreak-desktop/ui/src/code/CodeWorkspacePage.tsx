import { Button } from "@/components/ui/button";
import {
  centerEditorTabId,
  CenterTabIcon,
  centerTabParts,
  CHAT_PANEL_ID,
  CodeCenterTabs,
  conversationTabId,
  EDITOR_PANEL_ID,
  SPLIT_EDITOR_PANEL_ID,
} from "./CodeCenterTabs";
import {
  closeAllEditorTabs,
  closeCodeChromeTab,
  type CodeEditorRegion,
  closeEditorTab,
  closeEditorTabsToRight,
  closeOtherEditorTabs,
  focusCodeChromeTab,
  focusConversation,
  focusEditorTab,
  mergeEditorSplit,
  moveEditorTab,
  openCodeEditor,
  reorderEditorTab,
  splitCodeChromeLayout,
} from "./codeChrome";
import type { PermissionMode } from "../api/types";
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
import { DndContext, DragOverlay, useSensor, useSensors } from "@dnd-kit/core";
import { ErrorBoundary } from "@/ErrorBoundary";
import type { LayoutState, PanelContent } from "@/panel/panelTypes";
import {
  renderCodePanel,
  useCodeShortcutHints,
  useMeasuredWidth,
} from "./workspace/layout";
import { MarkdownLinkProvider } from "@/MessageMarkdown";
import { PanelLayout } from "@/panel/PanelLayout";
import { MemoryProposalChip } from "./MemoryProposalChip";
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
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
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
import { canOpenInExternalEditor } from "./codeWorktreeHost";
import { cn } from "@/lib/utils";
import { findEditorPanel, offersSplitDrop } from "./editorDrag";
import { fenceReasonText } from "./labels";
import { forkTranscriptFile } from "./fork";
import { isPutAway, sessionActivityLabel } from "./workspaceCards";
import { toast } from "sonner";
import { useApp } from "@/AppContext";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeContentRevision } from "./useLiveContent";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore, useSessionDigest } from "./CodeUpdatesStore";
import { useBrowserTabs } from "./workspace/useBrowserTabs";
import { useCodeWorkspacePr } from "./useCodeWorkspacePr";
import { useEditorTabs } from "./workspace/useEditorTabs";
import { useTerminalTabs } from "./workspace/useTerminalTabs";
import { useWorkspaceSessions } from "./workspace/useWorkspaceSessions";
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
  const catalog = useCodeCatalogStore();
  const { run, dialogs } = useWorkspaceCardCommands();
  const layout = useLayoutState();
  const { setLayout } = usePanelNav();
  const layoutRef = useRef(layout);
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
  const archivePending = useCodeUiStore((state) => state.archivePending);
  const shortcutHints = useCodeShortcutHints();
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
  const [createMode, setCreateMode] = useState<PermissionMode | null>(null);

  layoutRef.current = layout;

  function setWorkspaceLayout(next: LayoutState) {
    setLayout(next);
  }

  const {
    workspace,
    repo,
    session,
    conversationTabs,
    activeConversationId,
    draftAgent,
    startingNewAgent,
    starting,
    forkSource,
    clearForkSource,
    error,
    retry,
    startSession,
    reap,
    selectConversation,
    newConversation,
    forkConversation,
    closeConversation,
    openWorkspaceTask,
    openWorkspaceSubagent,
  } = useWorkspaceSessions({
    workspaceId,
    client,
    models,
    defaultModelKey,
    taskParam,
    navigate,
    focusConversationPane: () =>
      setWorkspaceLayout(focusConversation(layoutRef.current)),
  });
  const digest = useSessionDigest(workspaceId, session?.id ?? null);
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
  const workspaceStartup = useCodeUiStore(
    (state) => state.workspaceStartups[workspaceId] ?? null,
  );

  useEffect(() => {
    setViewedWorkspace(workspaceId);
    return () => setViewedWorkspace(null);
  }, [setViewedWorkspace, workspaceId]);

  useEffect(() => {
    return () => {
      useCodeUiStore.getState().setInspectorScope(null);
      useCodeUiStore.getState().finishComposerAction(workspaceId);
    };
  }, [workspaceId]);

  // Browsers close before shells: a native webview is worth nothing off
  // screen, while a shell outlives the tab that opened it.
  const {
    browserTitles,
    browserInitialUrls,
    openBrowser,
    setBrowserTitle,
    canNewBrowser,
  } = useBrowserTabs({ workspaceId, layout, setLayout: setWorkspaceLayout });
  const {
    terminalLabels,
    openTerminal,
    toggleTerminal,
    adoptTerminal,
    hasTerminal,
    canNewTerminal,
  } = useTerminalTabs({
    workspaceId,
    client,
    layout,
    setLayout: setWorkspaceLayout,
  });
  const {
    fileReveal,
    openFile,
    openTurnDiff,
    openFileDiff,
    quickOpenRequest,
    quickOpenTarget,
    newTabMenuRequest,
    newTabMenuRegion,
    requestNewTab,
    draggedTabId,
    setDraggedTabId,
    finishTabDrag,
    copyEditorPath,
    splitFocused,
  } = useEditorTabs({ layout, setLayout: setWorkspaceLayout });

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
        quickActions: repo?.quick_actions ?? [],
        setupFailed: workspace.status === "setup_failed",
      })
    : [];

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
              onTitleChange={(title) => setBrowserTitle(panel.browserId, title)}
            />
          </Suspense>
        ) : panel.type === "terminal" ? (
          <TerminalPane
            client={client}
            workspaceId={workspaceId}
            terminalId={panel.terminalId}
            onAttach={(terminalId) =>
              adoptTerminal(panel.terminalId, terminalId)
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
                            onRemove: clearForkSource,
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
                  onFileIssue={
                    workspace
                      ? () =>
                          run("uneff-me", {
                            workspace,
                            title: title ?? workspace.title,
                            session,
                          })
                      : undefined
                  }
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
              <MemoryProposalChip count={digest?.memory_proposal_count ?? 0} />
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
          <Button type="button" size="sm" onClick={retry}>
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
