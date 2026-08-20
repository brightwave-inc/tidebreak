import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";

import { ChevronRight, Folder, FolderOpen } from "lucide-react";
import type { ApiClient } from "../api/client";
import type { CodeWorkspaceSearchMatch } from "../api/types";
import { SearchInput } from "@/components/SearchInput";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { CodeFileIcon } from "./CodeFileIcon";
import { useCodeUiStore } from "./CodeUiStore";
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
 * Nested worktree explorer plus content search. Search is Cmd+F (Ctrl+F): the
 * query matches file contents and include/exclude fields use VS Code-style
 * globs. Filename-only quick open lives at Cmd+P in the workspace center.
 */
export function FilesPanel({
  client,
  workspaceId,
  selected,
  onOpenFile,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceTree" | "searchCodeWorkspace">;
  workspaceId: string;
  selected?: string;
  onOpenFile: (file: string, line?: number) => void;
  /** Bumped by the session journal when the worktree may have moved. */
  contentRevision?: number;
}) {
  const [query, setQuery] = useState("");
  const [include, setInclude] = useState("");
  const [exclude, setExclude] = useState("");
  const [openDirs, setOpenDirs] = useState<Set<string>>(() => new Set());
  const [closedTop, setClosedTop] = useState<Set<string>>(() => new Set());
  const [searchHits, setSearchHits] = useState<CodeWorkspaceSearchMatch[] | null>(
    null,
  );
  const [searchTruncated, setSearchTruncated] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const filesSearchPending = useCodeUiStore(
    (state) => state.filesSearchPending,
  );
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
    setSearchError(null);
    if (!needle) {
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(() => {
      void client
        .searchCodeWorkspace(workspaceId, {
          query: needle,
          include: include.trim() || undefined,
          exclude: exclude.trim() || undefined,
          limit: 200,
        })
        .then((hits) => {
          if (cancelled) return;
          setSearchHits(hits.matches);
          setSearchTruncated(hits.truncated);
          setSearching(false);
        })
        .catch((caught) => {
          if (cancelled) return;
          setSearchError(friendlyErrorMessage(caught, "Could not search files"));
          setSearching(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [client, workspaceId, query, include, exclude, contentRevision]);

  const visiblePaths = useMemo(() => {
    return filterPaths(tree?.paths ?? [], include, exclude);
  }, [tree, include, exclude]);

  const nodes = useMemo(() => buildFileTree(visiblePaths), [visiblePaths]);

  const forcedOpen = useMemo(() => {
    if (!include.trim() && !exclude.trim()) return new Set<string>();
    const dirs = new Set<string>();
    for (const path of visiblePaths) {
      for (const parent of ancestorPaths(path)) dirs.add(parent);
    }
    return dirs;
  }, [include, exclude, visiblePaths]);

  const searchGroups = useMemo(
    () => groupSearchMatches(searchHits ?? []),
    [searchHits],
  );

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

  // The find chord is raised as a flag by the shell keymap, above the route
  // that owns this panel. Taking it here rather than watching for the key
  // means search still answers Cmd+F when the review rail was closed: the
  // store opens the rail, this panel mounts, and the ask is still waiting.
  useEffect(() => {
    if (!filesSearchPending) return;
    if (!useCodeUiStore.getState().takeFilesSearch()) return;
    searchInput.current?.focus();
    searchInput.current?.select();
  }, [filesSearchPending]);

  const searchMode = Boolean(query.trim());
  const ready = tree !== null;
  const truncated = searchMode ? searchTruncated : Boolean(tree?.truncated);
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
      <div className="flex flex-col gap-1.5 px-3 pb-2 pt-2.5">
        <SearchInput
          size="sm"
          value={query}
          onValueChange={setQuery}
          placeholder="Search file contents"
          aria-label="Search file contents"
          inputRef={searchInput}
        />
        <Input
          value={include}
          onChange={(event) => setInclude(event.target.value)}
          placeholder="Files to include"
          aria-label="Files to include"
          className="h-8 text-xs"
        />
        <Input
          value={exclude}
          onChange={(event) => setExclude(event.target.value)}
          placeholder="Files to exclude"
          aria-label="Files to exclude"
          className="h-8 text-xs"
        />
      </div>
      {!searchMode && error && (
        <p className="text-critical px-3 py-2 text-sm">{error}</p>
      )}
      {searchMode && searchError && (
        <p className="text-critical px-3 py-2 text-sm">{searchError}</p>
      )}
      {truncated && (
        <p className="text-muted-foreground px-3 py-2 text-xs">
          {searchMode
            ? "Search results were truncated. Narrow the query or file filters."
            : "File list was truncated. Add a file filter to narrow it."}
        </p>
      )}
      {searchMode && searchHits === null && !searchError ? (
        <SearchResultsSkeleton />
      ) : !searchMode && !ready && !error ? (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/5" />
        </div>
      ) : searchMode ? (
        searchGroups.length > 0 ? (
          <SearchResults
            groups={searchGroups}
            query={query.trim()}
            onOpenFile={onOpenFile}
          />
        ) : !searchError ? (
          <ContentSearchEmpty
            query={query.trim()}
            onClear={() => setQuery("")}
          />
        ) : null
      ) : empty ? (
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

type SearchGroup = {
  path: string;
  matches: CodeWorkspaceSearchMatch[];
};

export function groupSearchMatches(
  matches: readonly CodeWorkspaceSearchMatch[],
): SearchGroup[] {
  const groups = new Map<string, CodeWorkspaceSearchMatch[]>();
  for (const matched of matches) {
    const rows = groups.get(matched.path);
    if (rows) rows.push(matched);
    else groups.set(matched.path, [matched]);
  }
  return [...groups].map(([path, rows]) => ({ path, matches: rows }));
}

function SearchResults({
  groups,
  query,
  onOpenFile,
}: {
  groups: readonly SearchGroup[];
  query: string;
  onOpenFile: (path: string, line?: number) => void;
}) {
  return (
    <div
      className="min-h-0 flex-1 overflow-y-auto px-1 pb-4 pt-2"
      role="list"
      aria-label="File content matches"
    >
      {groups.map((group) => (
        <section key={group.path} className="mb-2" aria-label={group.path}>
          <div className="text-muted-foreground flex items-center gap-1.5 px-2 py-1.5 text-[11px]">
            <CodeFileIcon path={group.path} />
            <span className="min-w-0 flex-1 truncate font-mono" title={group.path}>
              {group.path}
            </span>
            <span className="shrink-0 tabular-nums">{group.matches.length}</span>
          </div>
          <div className="space-y-0.5">
            {group.matches.map((matched) => (
              <button
                key={`${matched.line_number}:${matched.line}`}
                type="button"
                className={cn(
                  "hover:bg-muted/50 flex w-full cursor-pointer items-start gap-2 rounded-sm px-2 py-1 text-left",
                  FOCUS_RING,
                  HOVER_TINT,
                )}
                aria-label={`${group.path}, line ${matched.line_number}`}
                onClick={() => onOpenFile(group.path, matched.line_number)}
              >
                <span className="text-muted-foreground w-7 shrink-0 text-right font-mono text-[10px] leading-5 tabular-nums">
                  {matched.line_number}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-[11px] leading-5">
                  <LiteralMatch text={matched.line} query={query} />
                </span>
              </button>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

function LiteralMatch({ text, query }: { text: string; query: string }) {
  if (!query) return text;
  const lower = text.toLocaleLowerCase();
  const needle = query.toLocaleLowerCase();
  const parts: ReactNode[] = [];
  let cursor = 0;
  let index = lower.indexOf(needle);
  while (index >= 0) {
    if (index > cursor) parts.push(text.slice(cursor, index));
    parts.push(
      <mark
        key={`${index}:${cursor}`}
        className="bg-warning-background text-warning-foreground rounded-[2px] px-0.5"
      >
        {text.slice(index, index + query.length)}
      </mark>,
    );
    cursor = index + query.length;
    index = lower.indexOf(needle, cursor);
  }
  if (cursor < text.length) parts.push(text.slice(cursor));
  return parts.length > 0 ? parts : text;
}

function SearchResultsSkeleton() {
  return (
    <div className="flex flex-col gap-2 px-3 py-3" aria-label="Searching files">
      <Skeleton className="h-3 w-2/3" />
      <Skeleton className="h-4 w-full" />
      <Skeleton className="h-4 w-4/5" />
    </div>
  );
}

function ContentSearchEmpty({
  query,
  onClear,
}: {
  query: string;
  onClear: () => void;
}) {
  return (
    <div className="flex flex-col items-start gap-1 px-3 py-6">
      <p className="text-muted-foreground line-clamp-2 text-sm">
        No text matches <span className="font-mono break-all">{query}</span>.
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
        Clear search
      </button>
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
  const DirectoryIcon = open ? FolderOpen : Folder;

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
          "ring-offset-background flex min-h-7 w-full cursor-pointer items-center gap-1.5 rounded-md py-1 pr-2 text-left text-[12.5px]",
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
        {node.kind === "dir" ? (
          <DirectoryIcon
            className="text-muted-foreground size-3.5 shrink-0"
            aria-hidden
          />
        ) : (
          <CodeFileIcon path={node.path} />
        )}
        <span
          className={cn(
            "min-w-0 truncate",
            node.kind === "dir" && "font-medium",
          )}
          title={node.path}
        >
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
