import { type ReactNode, useCallback, useId, useMemo, useState } from "react";

import {
  ChevronRight,
  Folder,
  FolderOpen,
  MoreHorizontal,
  Trash2,
  Undo2,
} from "lucide-react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange, CodeWorkspaceFiles } from "../api/types";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { CodeFileIcon } from "./CodeFileIcon";
import { DiffstatBadge } from "./TurnReviewCard";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { type LiveResource, useLiveResource } from "./useLiveContent";
import { WorkspaceRevisionChip } from "./WorkspaceRevisionChip";
import { STATUS_TEXT } from "./statusTone";
import { FILE_KIND, type RevertRequest } from "./worktreeUndo";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";

/**
 * What a changed-file row offers besides opening its diff: revert the file,
 * and discard its uncommitted changes. The host asks first and has the
 * server apply it.
 */
export type ChangeRowActions = {
  onRevertFile: (request: RevertRequest) => unknown;
  /**
   * Offered only on files with uncommitted changes. `expectedTree` is the
   * list's `worktree_tree`, so the server refuses a file that changed since.
   */
  onDiscard?: (file: CodeFileChange, expectedTree?: string) => unknown;
  /**
   * Why nothing can change the worktree right now, such as a running turn.
   * The actions stay in the menu, turned off, with this sentence under them.
   */
  unavailableReason?: string;
};

/**
 * Compact source-control index. It deliberately fetches the bounded changed
 * file list rather than the unified patch: the sidebar answers what changed;
 * the center pane answers how.
 */
export function DiffOverview({
  client,
  workspaceId,
  turnId,
  turnLabel,
  selected,
  contentRevision = 0,
  onOpenFile,
  actions,
  commit,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  selected?: string;
  contentRevision?: number;
  onOpenFile: (path: string) => void;
  actions?: ChangeRowActions;
  /** The commit box, when the list shows the workspace rather than a turn. */
  commit?: (files: CodeWorkspaceFiles | null) => ReactNode;
}) {
  const resource = useChangedFilesResource({
    client,
    workspaceId,
    turnId,
    contentRevision,
  });

  return (
    <DiffOverviewContent
      resource={resource}
      turnId={turnId}
      turnLabel={turnLabel}
      selected={selected}
      onOpenFile={onOpenFile}
      actions={actions}
      commit={commit}
    />
  );
}

export function useChangedFilesResource({
  client,
  workspaceId,
  turnId,
  contentRevision = 0,
  enabled = true,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  contentRevision?: number;
  enabled?: boolean;
}): LiveResource<CodeWorkspaceFiles> {
  const load = useCallback(
    () => client.listCodeWorkspaceFiles(workspaceId, turnId),
    [client, workspaceId, turnId],
  );
  return useLiveResource({
    key: `${workspaceId}:${turnId ?? "workspace"}`,
    revision: contentRevision,
    load,
    enabled,
    errorMessage: "Could not load changed files",
  });
}

export function DiffOverviewContent({
  resource,
  turnId,
  turnLabel,
  selected,
  onOpenFile,
  actions,
  commit,
}: {
  resource: Pick<
    LiveResource<CodeWorkspaceFiles>,
    "data" | "error" | "refreshing"
  > &
    Partial<Pick<LiveResource<CodeWorkspaceFiles>, "refresh" | "archived">>;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  selected?: string;
  onOpenFile: (path: string) => void;
  actions?: ChangeRowActions;
  /**
   * The commit box. A turn's changes are history, so it only sits above the
   * workspace's own list.
   */
  commit?: (files: CodeWorkspaceFiles | null) => ReactNode;
}) {
  const { data: payload, error, archived, refreshing, refresh } = resource;

  const scopeCaption = turnId
    ? (turnLabel ?? "This turn")
    : "Workspace vs base";
  const tree = useMemo(
    () => buildChangeTree(payload?.files ?? []),
    [payload?.files],
  );
  const rowActions = actions
    ? { ...actions, turnId, worktreeTree: payload?.worktree_tree }
    : undefined;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {!turnId && commit?.(payload)}
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 px-3 pb-2 pt-3">
        <div className="min-w-0 flex-[1_1_7rem]">
          <div className="flex items-baseline gap-1.5">
            <h2 className="text-sm font-medium">Changes</h2>
            {payload && (
              <span className="text-muted-foreground font-mono text-xs tabular-nums">
                {payload.files.length}
              </span>
            )}
          </div>
          <p className="text-muted-foreground truncate font-mono text-xs">
            {scopeCaption}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <WorkspaceRevisionChip
            revision={payload?.revision}
            revisionRef={payload?.revision_ref}
            savedAt={payload?.revision_saved_at}
          />
          {payload && <DiffstatBadge stat={payload.stat} />}
          {/* A fixed trailing slot, so a refresh moves nothing and an idle
              slot never opens a gap between the chip and its neighbors. */}
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
        </div>
      </header>
      {error && (
        <Notice
          tone={archived ? "neutral" : "critical"}
          docked="top"
          className="shrink-0"
          action={
            refresh &&
            !archived && (
              <NoticeRetryButton
                pending={refreshing}
                onClick={() => void refresh()}
              />
            )
          }
        >
          {error}
        </Notice>
      )}
      {payload?.truncated && (
        <p className="text-muted-foreground border-y px-3 py-2 text-xs">
          The changed-file list was truncated.
        </p>
      )}
      {!payload && !error && <ChangesSkeleton />}
      {payload && payload.files.length > 0 && (
        <ul
          className="min-h-0 flex-1 overflow-y-auto px-1 pb-4 pt-1"
          aria-label="Changed files"
          tabIndex={0}
        >
          {tree.map((node) => (
            <ChangeTreeRow
              key={node.path}
              node={node}
              depth={0}
              selected={selected}
              onOpenFile={onOpenFile}
              actions={rowActions}
            />
          ))}
        </ul>
      )}
      {payload && payload.files.length === 0 && !error && (
        <p className="text-muted-foreground px-3 py-6 text-sm">
          {emptyChangesText(turnId, turnLabel)}
        </p>
      )}
    </div>
  );
}

type RowActions = ChangeRowActions & {
  turnId?: string;
  /** The snapshot the list was read from, for a discard to check against. */
  worktreeTree?: string;
};

type ChangeTreeNode = ChangeDirectoryNode | ChangeFileNode;

type ChangeDirectoryNode = {
  kind: "dir";
  name: string;
  path: string;
  count: number;
  children: ChangeTreeNode[];
};

type ChangeFileNode = {
  kind: "file";
  name: string;
  path: string;
  file: CodeFileChange;
};

/** Build and compact a Git-style changed-file tree from relative paths. */
export function buildChangeTree(
  files: readonly CodeFileChange[],
): ChangeTreeNode[] {
  const root: ChangeTreeNode[] = [];
  for (const file of files) {
    const parts = file.path.split("/").filter(Boolean);
    if (parts.length === 0) continue;
    let siblings = root;
    let prefix = "";
    for (let index = 0; index < parts.length; index += 1) {
      const name = parts[index]!;
      prefix = prefix ? `${prefix}/${name}` : name;
      const isFile = index === parts.length - 1;
      if (isFile) {
        siblings.push({ kind: "file", name, path: prefix, file });
        continue;
      }
      let directory = siblings.find(
        (node): node is ChangeDirectoryNode =>
          node.kind === "dir" && node.name === name,
      );
      if (!directory) {
        directory = { kind: "dir", name, path: prefix, count: 0, children: [] };
        siblings.push(directory);
      }
      directory.count += 1;
      siblings = directory.children;
    }
  }
  sortChangeTree(root);
  return root.map(compactDirectoryChain);
}

/**
 * Changed paths in the order the Changes list shows them: folders before
 * files at each level, each group by name. Stepping from one file's diff to
 * the next follows this order, so the keys walk the list the reader sees.
 */
export function changedFileOrder(files: readonly CodeFileChange[]): string[] {
  const order: string[] = [];
  const walk = (nodes: readonly ChangeTreeNode[]) => {
    for (const node of nodes) {
      if (node.kind === "file") order.push(node.path);
      else walk(node.children);
    }
  };
  walk(buildChangeTree(files));
  return order;
}

function sortChangeTree(nodes: ChangeTreeNode[]): void {
  nodes.sort((left, right) => {
    if (left.kind !== right.kind) return left.kind === "dir" ? -1 : 1;
    return left.name.localeCompare(right.name);
  });
  for (const node of nodes) {
    if (node.kind === "dir") sortChangeTree(node.children);
  }
}

function compactDirectoryChain(node: ChangeTreeNode): ChangeTreeNode {
  if (node.kind === "file") return node;
  let name = node.name;
  let path = node.path;
  let children = node.children.map(compactDirectoryChain);
  while (children.length === 1 && children[0]?.kind === "dir") {
    const child = children[0];
    name = `${name}/${child.name}`;
    path = child.path;
    children = child.children;
  }
  return { ...node, name, path, children };
}

function ChangeTreeRow({
  node,
  depth,
  selected,
  onOpenFile,
  actions,
}: {
  node: ChangeTreeNode;
  depth: number;
  selected?: string;
  onOpenFile: (path: string) => void;
  actions?: RowActions;
}) {
  if (node.kind === "file") {
    return (
      <ChangeFileRow
        node={node}
        depth={depth}
        selected={selected === node.path}
        onOpenFile={onOpenFile}
        actions={actions}
      />
    );
  }
  return (
    <ChangeDirectoryRow
      node={node}
      depth={depth}
      selected={selected}
      onOpenFile={onOpenFile}
      actions={actions}
    />
  );
}

function ChangeDirectoryRow({
  node,
  depth,
  selected,
  onOpenFile,
  actions,
}: {
  node: ChangeDirectoryNode;
  depth: number;
  selected?: string;
  onOpenFile: (path: string) => void;
  actions?: RowActions;
}) {
  const [open, setOpen] = useState(true);
  const DirectoryIcon = open ? FolderOpen : Folder;
  return (
    <li>
      <button
        type="button"
        className={cn(
          "text-muted-foreground flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1 pr-2 text-left text-xs hover:bg-muted/35 hover:text-foreground",
          FOCUS_RING,
          HOVER_TINT,
        )}
        style={{ paddingLeft: 6 + depth * 14 }}
        aria-expanded={open}
        aria-label={`${open ? "Collapse" : "Expand"} ${node.path}`}
        title={node.path}
        onClick={() => setOpen((current) => !current)}
      >
        <ChevronRight
          className={cn(
            "size-3 shrink-0 transition-transform duration-150 motion-reduce:transition-none",
            open && "rotate-90",
          )}
          aria-hidden
        />
        <DirectoryIcon className="size-3.5 shrink-0" aria-hidden />
        <span className="min-w-0 flex-1 truncate font-mono">{node.name}</span>
        <span className="shrink-0 font-mono text-2xs tabular-nums">
          {node.count}
        </span>
      </button>
      {open && (
        <ul>
          {node.children.map((child) => (
            <ChangeTreeRow
              key={child.path}
              node={child}
              depth={depth + 1}
              selected={selected}
              onOpenFile={onOpenFile}
              actions={actions}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

function ChangeFileRow({
  node,
  depth,
  selected,
  onOpenFile,
  actions,
}: {
  node: ChangeFileNode;
  depth: number;
  selected: boolean;
  onOpenFile: (path: string) => void;
  actions?: RowActions;
}) {
  const file = node.file;
  const kind = FILE_KIND[file.kind];

  return (
    <li className="group/row relative">
      <button
        type="button"
        className={cn(
          "flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1.5 pr-2 text-left",
          FOCUS_RING,
          HOVER_TINT,
          actions && "pr-8",
          selected ? "bg-muted/70" : "hover:bg-muted/45",
        )}
        style={{ paddingLeft: 23 + depth * 14 }}
        aria-label={`${kind.label} ${file.path}, ${file.insertions} insertions, ${file.deletions} deletions`}
        aria-current={selected ? "page" : undefined}
        title={
          file.previous_path
            ? `${file.path}\nRenamed from ${file.previous_path}`
            : file.path
        }
        onClick={() => onOpenFile(file.path)}
      >
        <CodeFileIcon path={file.path} className={kind.className} />
        <span className="min-w-0 flex-1 truncate text-md">{node.name}</span>
        <span className="flex shrink-0 items-center gap-1.5 font-mono text-2xs tabular-nums">
          {file.insertions > 0 && (
            <span className={STATUS_TEXT.ready}>+{file.insertions}</span>
          )}
          {file.deletions > 0 && (
            <span className={STATUS_TEXT.critical}>−{file.deletions}</span>
          )}
          <span
            className={cn("w-2.5 text-right font-semibold", kind.className)}
          >
            {kind.letter}
          </span>
        </span>
      </button>
      {actions && <ChangeFileMenu file={file} actions={actions} />}
    </li>
  );
}

/**
 * A file's actions, behind a trigger that shows while the row is hovered or
 * holds focus. Revert goes back to the diff's own base; discard goes back to
 * the last commit, so the two read differently whenever the branch has
 * commits of its own.
 */
function ChangeFileMenu({
  file,
  actions,
}: {
  file: CodeFileChange;
  actions: RowActions;
}) {
  const unavailable = actions.unavailableReason !== undefined;
  const reasonId = useId();
  const request: RevertRequest = {
    path: file.path,
    turnId: actions.turnId,
    kind: file.kind,
    previousPath: file.previous_path,
  };
  const discard = actions.onDiscard && file.uncommitted && !actions.turnId;
  // A turned-off item stays focusable, so a keyboard or screen reader user
  // reaches it and hears why it is off.
  const unavailableItem = unavailable
    ? {
        "aria-disabled": true,
        "aria-describedby": reasonId,
        className: "opacity-60 cursor-not-allowed",
      }
    : {};
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`Actions for ${file.path}`}
          className={cn(
            "text-muted-foreground hover:bg-muted hover:text-foreground absolute top-1/2 right-1.5 grid size-6 -translate-y-1/2 cursor-pointer place-items-center rounded-md opacity-0 group-focus-within/row:opacity-100 group-hover/row:opacity-100 data-[state=open]:opacity-100 pointer-coarse:opacity-100",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
        >
          <MoreHorizontal className="size-3.5" aria-hidden="true" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        collisionPadding={12}
        className="w-max max-w-80"
      >
        <DropdownMenuItem
          {...unavailableItem}
          onSelect={(event) => {
            if (unavailable) {
              event.preventDefault();
              return;
            }
            void actions.onRevertFile(request);
          }}
        >
          <Undo2 />
          {actions.turnId
            ? "Revert this turn's changes"
            : "Revert to base branch"}
        </DropdownMenuItem>
        {discard && (
          <DropdownMenuItem
            {...unavailableItem}
            onSelect={(event) => {
              if (unavailable) {
                event.preventDefault();
                return;
              }
              void actions.onDiscard?.(file, actions.worktreeTree);
            }}
          >
            <Trash2 />
            Discard uncommitted changes
          </DropdownMenuItem>
        )}
        {actions.unavailableReason && (
          <p
            id={reasonId}
            className="text-muted-foreground px-2 pb-1.5 pl-10 text-xs"
          >
            {actions.unavailableReason}
          </p>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function emptyChangesText(turnId?: string, turnLabel?: string): string {
  if (turnId) return `${turnLabel ?? "This turn"} changed no files.`;
  return "The worktree matches its base branch.";
}

function ChangesSkeleton() {
  return (
    <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-8 w-5/6" />
      <Skeleton className="h-8 w-11/12" />
    </div>
  );
}
