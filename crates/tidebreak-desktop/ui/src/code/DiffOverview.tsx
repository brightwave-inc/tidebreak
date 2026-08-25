import { useCallback, useMemo, useState } from "react";

import { ChevronRight, Folder, FolderOpen } from "lucide-react";

import type { ApiClient } from "../api/client";
import type {
  CodeFileChange,
  CodeWorkspaceFiles,
  FileChangeKind,
} from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { CodeFileIcon } from "./CodeFileIcon";
import { DiffstatBadge } from "./TurnReviewCard";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { type LiveResource, useLiveResource } from "./useLiveContent";

const FILE_KIND: Record<
  FileChangeKind,
  { letter: string; label: string; className: string }
> = {
  added: {
    letter: "A",
    label: "Added",
    className: "text-success-foreground",
  },
  modified: {
    letter: "M",
    label: "Modified",
    className: "text-warning-foreground",
  },
  deleted: {
    letter: "D",
    label: "Deleted",
    className: "text-critical-foreground",
  },
  renamed: {
    letter: "R",
    label: "Renamed",
    className: "text-info-foreground",
  },
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
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  selected?: string;
  contentRevision?: number;
  onOpenFile: (path: string) => void;
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
    />
  );
}

export function useChangedFilesResource({
  client,
  workspaceId,
  turnId,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  contentRevision?: number;
}): LiveResource<CodeWorkspaceFiles> {
  const load = useCallback(
    () => client.listCodeWorkspaceFiles(workspaceId, turnId),
    [client, workspaceId, turnId],
  );
  return useLiveResource({
    key: `${workspaceId}:${turnId ?? "workspace"}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not load changed files",
  });
}

export function DiffOverviewContent({
  resource,
  turnId,
  turnLabel,
  selected,
  onOpenFile,
}: {
  resource: Pick<
    LiveResource<CodeWorkspaceFiles>,
    "data" | "error" | "refreshing"
  >;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  selected?: string;
  onOpenFile: (path: string) => void;
}) {
  const { data: payload, error, refreshing } = resource;

  const scopeCaption = turnId
    ? (turnLabel ?? "This turn")
    : "Workspace vs base";
  const tree = useMemo(
    () => buildChangeTree(payload?.files ?? []),
    [payload?.files],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between gap-2 px-3 pb-2 pt-3">
        <div className="min-w-0">
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
        <div className="flex items-center gap-2">
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
          {payload && <DiffstatBadge stat={payload.stat} />}
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
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
        >
          {tree.map((node) => (
            <ChangeTreeRow
              key={node.path}
              node={node}
              depth={0}
              selected={selected}
              onOpenFile={onOpenFile}
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
}: {
  node: ChangeTreeNode;
  depth: number;
  selected?: string;
  onOpenFile: (path: string) => void;
}) {
  if (node.kind === "file") {
    return (
      <ChangeFileRow
        node={node}
        depth={depth}
        selected={selected === node.path}
        onOpenFile={onOpenFile}
      />
    );
  }
  return (
    <ChangeDirectoryRow
      node={node}
      depth={depth}
      selected={selected}
      onOpenFile={onOpenFile}
    />
  );
}

function ChangeDirectoryRow({
  node,
  depth,
  selected,
  onOpenFile,
}: {
  node: ChangeDirectoryNode;
  depth: number;
  selected?: string;
  onOpenFile: (path: string) => void;
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
}: {
  node: ChangeFileNode;
  depth: number;
  selected: boolean;
  onOpenFile: (path: string) => void;
}) {
  const file = node.file;
  const kind = FILE_KIND[file.kind];

  return (
    <li>
      <button
        type="button"
        className={cn(
          "group flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1.5 pr-2 text-left",
          FOCUS_RING,
          HOVER_TINT,
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
            <span className="text-success-foreground">+{file.insertions}</span>
          )}
          {file.deletions > 0 && (
            <span className="text-critical-foreground">−{file.deletions}</span>
          )}
          <span
            className={cn("w-2.5 text-right font-semibold", kind.className)}
          >
            {kind.letter}
          </span>
        </span>
      </button>
    </li>
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
