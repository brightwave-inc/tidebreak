import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type RefObject,
} from "react";

import { ChevronRight, File, Folder, FolderOpen } from "lucide-react";
import type { ApiClient } from "../api/client";
import { SearchInput } from "@/components/SearchInput";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import {
  ancestorPaths,
  buildFileTree,
  filterPaths,
  flattenVisibleTree,
  treeIndentPx,
  type FileTreeNode,
} from "./fileTree";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { useLiveResource } from "./useLiveContent";

const TREE_PAGE = 5000;

/**
 * Nested worktree explorer. Search is Cmd+F (Ctrl+F): file names and
 * include/exclude globs. Diffs stay on the Source tab.
 */
export function FilesPanel({
  client,
  workspaceId,
  selected,
  onOpenFile,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceTree">;
  workspaceId: string;
  selected?: string;
  onOpenFile: (file: string) => void;
  /** Bumped by the session journal when the worktree may have moved. */
  contentRevision?: number;
}) {
  const [query, setQuery] = useState("");
  const [include, setInclude] = useState("");
  const [exclude, setExclude] = useState("");
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [openDirs, setOpenDirs] = useState<Set<string>>(() => new Set());
  const [closedTop, setClosedTop] = useState<Set<string>>(() => new Set());
  const [searchHits, setSearchHits] = useState<string[] | null>(null);
  const [searchTruncated, setSearchTruncated] = useState(false);
  const [searching, setSearching] = useState(false);
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const rowRefs = useRef(new Map<string, HTMLLIElement>());

  const loadTree = useCallback(
    () => client.listCodeWorkspaceTree(workspaceId, { limit: TREE_PAGE }),
    [client, workspaceId],
  );
  const {
    data: tree,
    error,
    refreshing,
  } = useLiveResource({
    key: workspaceId,
    revision: contentRevision,
    load: loadTree,
    errorMessage: "Could not load files",
  });

  useEffect(() => {
    const needle = query.trim();
    setSearchHits(null);
    setSearchTruncated(false);
    if (!needle) {
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(() => {
      void client
        .listCodeWorkspaceTree(workspaceId, { query: needle, limit: TREE_PAGE })
        .then((hits) => {
          if (cancelled) return;
          setSearchHits(hits.paths);
          setSearchTruncated(hits.truncated);
          setSearching(false);
        })
        .catch(() => {
          if (!cancelled) setSearching(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [client, workspaceId, query, contentRevision]);

  const visiblePaths = useMemo(() => {
    return filterPaths(searchHits ?? tree?.paths ?? [], include, exclude);
  }, [searchHits, tree, include, exclude]);

  const nodes = useMemo(() => buildFileTree(visiblePaths), [visiblePaths]);

  const forcedOpen = useMemo(() => {
    if (!query.trim() && !include.trim() && !exclude.trim()) return new Set<string>();
    const dirs = new Set<string>();
    for (const path of visiblePaths) {
      for (const parent of ancestorPaths(path)) dirs.add(parent);
    }
    return dirs;
  }, [query, include, exclude, visiblePaths]);

  function isOpen(path: string): boolean {
    if (forcedOpen.has(path)) return true;
    if (!path.includes("/")) return !closedTop.has(path);
    return openDirs.has(path);
  }

  // The drawn rows, in document order: what the arrow keys walk. `isOpen` is a
  // fresh closure every render, so the state it reads is the dep set.
  const rows = useMemo(
    () => flattenVisibleTree(nodes, isOpen),
    [nodes, forcedOpen, closedTop, openDirs],
  );
  // Tab reaches the tree once; the arrows move inside it. The tab stop follows
  // the reader, and falls back to the first row whenever the row they left is
  // no longer drawn (a search, a collapsed parent, a refreshed worktree).
  const tabStop =
    rows.find((row) => row.node.path === focusedPath)?.node.path ??
    rows[0]?.node.path ??
    null;

  function toggleDir(path: string) {
    if (!path.includes("/")) {
      setClosedTop((current) => {
        const next = new Set(current);
        if (next.has(path)) next.delete(path);
        else next.add(path);
        return next;
      });
      return;
    }
    setOpenDirs((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  useEffect(() => {
    function onFind(event: KeyboardEvent) {
      if (event.key !== "f" && event.key !== "F") return;
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const pane = root.current;
      if (!pane || pane.closest("[data-state='inactive']")) return;
      event.preventDefault();
      setFiltersOpen(true);
      searchInput.current?.focus();
      searchInput.current?.select();
    }
    window.addEventListener("keydown", onFind);
    return () => window.removeEventListener("keydown", onFind);
  }, []);

  const ready = tree !== null;
  const truncated = Boolean(tree?.truncated || searchTruncated);
  const empty = ready && nodes.length === 0 && !error;
  const busy = refreshing || searching;

  function focusRow(path: string | undefined) {
    if (!path) return;
    setFocusedPath(path);
    rowRefs.current.get(path)?.focus();
  }

  function activate(node: FileTreeNode) {
    if (node.kind === "dir") toggleDir(node.path);
    else onOpenFile(node.path);
  }

  /**
   * The tree pattern's keys.
   *
   * Right and Left are the two that make a tree a tree: on a closed folder
   * Right opens it and on an open one it steps in, and Left mirrors that by
   * closing or climbing out. Everything else is list movement.
   */
  function onTreeKeyDown(event: ReactKeyboardEvent<HTMLUListElement>) {
    const index = rows.findIndex((row) => row.node.path === tabStop);
    const row = rows[index];
    if (!row) return;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusRow(rows[index + 1]?.node.path);
        return;
      case "ArrowUp":
        event.preventDefault();
        focusRow(rows[index - 1]?.node.path);
        return;
      case "ArrowRight":
        event.preventDefault();
        if (row.node.kind !== "dir") return;
        if (!row.expanded) toggleDir(row.node.path);
        else focusRow(rows[index + 1]?.node.path);
        return;
      case "ArrowLeft":
        event.preventDefault();
        if (row.node.kind === "dir" && row.expanded) toggleDir(row.node.path);
        else focusRow(row.parent ?? undefined);
        return;
      case "Home":
        event.preventDefault();
        focusRow(rows[0]?.node.path);
        return;
      case "End":
        event.preventDefault();
        focusRow(rows[rows.length - 1]?.node.path);
        return;
      case "Enter":
      case " ":
        event.preventDefault();
        activate(row.node);
        return;
      default:
    }
  }

  return (
    <div
      ref={root}
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      data-testid="files-explorer"
    >
      <div className="flex items-center justify-between gap-2 px-3 pt-3">
        <h2 className="text-sm font-medium">Files</h2>
        <span className="grid size-3.5 shrink-0 place-items-center">
          {busy && <Spinner className="size-3.5" aria-label="Refreshing" />}
        </span>
      </div>
      <div className="flex flex-col gap-2 px-3 pt-2">
        <SearchInput
          size="sm"
          value={query}
          onValueChange={setQuery}
          placeholder="Search files"
          aria-label="Search files"
          inputRef={searchInput}
        />
        {filtersOpen || include || exclude ? (
          <>
            <Input
              value={include}
              onChange={(event) => setInclude(event.target.value)}
              placeholder="files to include"
              aria-label="Files to include"
              className="h-8 text-xs"
            />
            <Input
              value={exclude}
              onChange={(event) => setExclude(event.target.value)}
              placeholder="files to exclude"
              aria-label="Files to exclude"
              className="h-8 text-xs"
            />
          </>
        ) : (
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:text-foreground cursor-pointer self-start rounded-sm text-[11px]",
              FOCUS_RING,
              HOVER_TINT,
            )}
            onClick={() => setFiltersOpen(true)}
          >
            Include / exclude
          </button>
        )}
      </div>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {truncated && (
        <p className="text-muted-foreground px-3 py-2 text-xs">
          File list was truncated. Narrow the search to see the rest.
        </p>
      )}
      {!ready && !error && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/5" />
        </div>
      )}
      {empty ? (
        <FilesEmpty
          query={query.trim()}
          filtered={Boolean(include.trim() || exclude.trim())}
          onClear={() => {
            setQuery("");
            setInclude("");
            setExclude("");
          }}
        />
      ) : ready ? (
        <ul
          className="min-h-0 flex-1 overflow-y-auto px-1 pb-4 pt-2"
          role="tree"
          aria-label="Workspace files"
          onKeyDown={onTreeKeyDown}
        >
          {nodes.map((node) => (
            <TreeRow
              key={node.path}
              node={node}
              depth={0}
              selected={selected}
              tabStop={tabStop}
              rowRefs={rowRefs}
              isOpen={isOpen}
              onFocusRow={setFocusedPath}
              onActivate={activate}
            />
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/**
 * Nothing to list, in the two ways that can happen.
 *
 * A rail this narrow has no room for a full empty block, so the treatment is
 * one quiet line — but a filtered miss says what was searched for and offers
 * the way out, because "no matching files" beside a box the reader typed in
 * two minutes ago leaves them guessing which of three filters is the culprit.
 */
function FilesEmpty({
  query,
  filtered,
  onClear,
}: {
  query: string;
  filtered: boolean;
  onClear: () => void;
}) {
  if (!query && !filtered) {
    return (
      <p className="text-muted-foreground px-3 py-6 text-sm">
        This worktree has no tracked files yet.
      </p>
    );
  }
  return (
    <div className="flex flex-col items-start gap-1 px-3 py-6">
      <p className="text-muted-foreground line-clamp-2 min-w-0 max-w-full text-sm">
        No files match{" "}
        {query ? (
          <span className="font-mono break-all">{query}</span>
        ) : (
          "these filters"
        )}
        .
      </p>
      <button
        type="button"
        className={cn(
          "text-muted-foreground hover:text-foreground cursor-pointer rounded-sm text-[11px]",
          FOCUS_RING,
          HOVER_TINT,
        )}
        onClick={onClear}
      >
        Clear search and filters
      </button>
    </div>
  );
}

/**
 * One row of the explorer tree.
 *
 * The row itself is the `treeitem`, not a button inside one: the tree pattern
 * puts focus and the arrow keys on the item, and a nested button would take
 * both and announce itself as a button in a tree. `aria-label` restates the
 * name a sighted reader sees because a treeitem otherwise draws its name from
 * everything it contains — for an open folder, that is the whole subtree.
 */
function TreeRow({
  node,
  depth,
  selected,
  tabStop,
  rowRefs,
  isOpen,
  onFocusRow,
  onActivate,
}: {
  node: FileTreeNode;
  depth: number;
  selected?: string;
  tabStop: string | null;
  rowRefs: RefObject<Map<string, HTMLLIElement>>;
  isOpen: (path: string) => boolean;
  onFocusRow: (path: string) => void;
  onActivate: (node: FileTreeNode) => void;
}) {
  const open = node.kind === "dir" && isOpen(node.path);
  const current = node.kind === "file" && selected === node.path;
  const Icon =
    node.kind === "dir" ? (open ? FolderOpen : Folder) : File;

  return (
    <li
      role="treeitem"
      aria-label={node.name}
      aria-level={depth + 1}
      aria-expanded={node.kind === "dir" ? open : undefined}
      aria-current={current ? true : undefined}
      tabIndex={tabStop === node.path ? 0 : -1}
      ref={(element) => {
        if (element) rowRefs.current.set(node.path, element);
        else rowRefs.current.delete(node.path);
      }}
      className="group/row focus-visible:outline-none"
      onFocus={(event) => {
        if (event.target === event.currentTarget) onFocusRow(node.path);
      }}
      onClick={(event) => {
        // A click inside a nested row would otherwise reach every folder above
        // it on the way out and collapse each one.
        event.stopPropagation();
        onFocusRow(node.path);
        onActivate(node);
      }}
    >
      <div
        style={{ paddingLeft: treeIndentPx(depth) }}
        className={cn(
          "ring-offset-background flex w-full cursor-pointer items-center gap-1 rounded-sm py-0.5 pr-2 text-left text-xs",
          "group-focus-visible/row:ring-ring group-focus-visible/row:ring-2 group-focus-visible/row:ring-offset-0",
          HOVER_TINT,
          current && "bg-muted/60",
          !current && "hover:bg-muted/40",
        )}
      >
        {node.kind === "dir" ? (
          <ChevronRight
            className={cn(
              "text-muted-foreground size-3 shrink-0 transition-transform duration-[140ms] ease-out motion-reduce:transition-none",
              open && "rotate-90",
            )}
            aria-hidden
          />
        ) : (
          <span className="size-3 shrink-0" aria-hidden />
        )}
        <Icon className="text-muted-foreground size-3.5 shrink-0" aria-hidden />
        <span className="min-w-0 truncate" title={node.path}>
          {node.name}
        </span>
      </div>
      {open && node.children && (
        <ul role="group">
          {node.children.map((child) => (
            <TreeRow
              key={child.path}
              node={child}
              depth={depth + 1}
              selected={selected}
              tabStop={tabStop}
              rowRefs={rowRefs}
              isOpen={isOpen}
              onFocusRow={onFocusRow}
              onActivate={onActivate}
            />
          ))}
        </ul>
      )}
    </li>
  );
}
