import { lazy, Suspense, type ReactNode } from "react";
import type { ApiClient } from "../../api/client";
import type {
  CodeWorkspaceFiles,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../../api/types";
import type { PanelContent } from "@/panel/panelTypes";
import { Skeleton } from "@/components/ui/skeleton";
import {
  centerEditorTabId,
  EDITOR_PANEL_ID,
  SPLIT_EDITOR_PANEL_ID,
} from "../CodeCenterTabs";
import type { CodeEditorRegion } from "../codeChrome";
import { WorkspaceDeliveryPrTab } from "../CodeInspector";
import { canOpenInExternalEditor } from "../codeWorktreeHost";
import { DiffOverview, type ChangeRowActions } from "../DiffOverview";
import { DiffPanel, type DiffRevertActions } from "../DiffPanel";
import type { DiffReviewerContext } from "../review/ReviewChanges";
import type { CodeWorkspacePrResource } from "../useCodeWorkspacePr";
import { openWorkspaceFileInEditor } from "../workspaceActions";
import { isRemoteWorktreePath } from "../workspaceRemote";

const FileViewer = lazy(async () => {
  const module = await import("../FileViewer");
  return { default: module.FileViewer };
});

const CodeBrowserTab = lazy(async () => {
  const module = await import("../browser/CodeBrowserTab");
  return { default: module.CodeBrowserTab };
});

const TerminalPane = lazy(async () => {
  const module = await import("../TerminalPane");
  return { default: module.TerminalPane };
});

/**
 * What one editor tab of a workspace shows: a file, a diff, a browser, a
 * terminal, Source control, or the pull request. The primary group and the
 * split group each render one for their active tab.
 */
export function WorkspaceEditorPanel({
  panel,
  region,
  index,
  client,
  workspaceId,
  workspace,
  hostAccess,
  contentRevision,
  fileReveal,
  diffRevert,
  reviewer,
  changeActions,
  renderCommitBox,
  stepFileDiff,
  openFile,
  openFileDiff,
  browserInitialUrls,
  workspaceOverlayOpen,
  setBrowserTitle,
  adoptTerminal,
  prResource,
  pr,
}: {
  panel: PanelContent;
  region: CodeEditorRegion;
  index: number;
  client: ApiClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
  hostAccess: boolean;
  contentRevision: number;
  fileReveal: { path: string; line: number; revision: number } | null;
  diffRevert: DiffRevertActions | undefined;
  reviewer: DiffReviewerContext | undefined;
  changeActions: ChangeRowActions | undefined;
  renderCommitBox:
    | ((files: CodeWorkspaceFiles | null) => ReactNode)
    | undefined;
  stepFileDiff: (from: string, to: string, turnId?: string) => void;
  openFile: (
    path: string,
    line?: number,
    preferredRegion?: CodeEditorRegion,
  ) => void;
  openFileDiff: (path: string) => void;
  browserInitialUrls: Record<string, string>;
  workspaceOverlayOpen: boolean;
  setBrowserTitle: (browserId: string, title: string) => void;
  adoptTerminal: (previousId: string | undefined, terminalId: string) => void;
  prResource: CodeWorkspacePrResource;
  pr: PullRequestDigest | undefined;
}) {
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
            readOnlyReason={
              isRemoteWorktreePath(workspace?.worktree_path)
                ? "Sandbox files are read-only"
                : undefined
            }
            revealLine={
              fileReveal?.path === panel.path ? fileReveal.line : undefined
            }
            revealRevision={fileReveal?.revision}
            onOpenInEditor={
              hostAccess && canOpenInExternalEditor()
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
          revert={diffRevert}
          reviewer={reviewer}
          onStepFile={(next) => {
            if (panel.path) stepFileDiff(panel.path, next, panel.turnId);
          }}
          onOpenFile={(path) => openFile(path, undefined, region)}
          onOpenInEditor={
            hostAccess && canOpenInExternalEditor()
              ? (path) =>
                  openWorkspaceFileInEditor({
                    workspaceId,
                    relativePath: path,
                  })
              : undefined
          }
        />
      ) : (panel.type === "browser" || panel.type === "terminal") &&
        !hostAccess ? (
        <p className="p-4 text-muted-foreground">
          Host tools are available only to the workspace owner.
        </p>
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
        <Suspense fallback={<Skeleton className="h-full w-full" />}>
          <TerminalPane
            client={client}
            workspaceId={workspaceId}
            terminalId={panel.terminalId}
            onAttach={(terminalId) =>
              adoptTerminal(panel.terminalId, terminalId)
            }
            hideHeader
          />
        </Suspense>
      ) : panel.type === "source_control" ? (
        <div
          className="flex min-h-0 flex-1 flex-col overflow-hidden"
          data-testid="source-control-panel"
        >
          <DiffOverview
            client={client}
            workspaceId={workspaceId}
            contentRevision={contentRevision}
            actions={changeActions}
            commit={renderCommitBox}
            onOpenFile={(path) => openFileDiff(path)}
          />
        </div>
      ) : panel.type === "pr" ? (
        <div
          className="flex min-h-0 flex-1 flex-col overflow-hidden"
          data-testid="pr-details-panel"
        >
          <WorkspaceDeliveryPrTab
            workspaceOnly={!hostAccess}
            allowMerge={!isRemoteWorktreePath(workspace?.worktree_path)}
            client={client}
            workspaceId={workspaceId}
            pr={prResource.data === null ? pr : prResource.data.pr}
            branch={workspace?.branch_name}
            prResource={prResource}
          />
        </div>
      ) : null}
    </div>
  );
}
