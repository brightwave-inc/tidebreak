import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  type FileTreeNode,
} from "./fileTree";
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
  const searchInput = useRef<HTMLInputElement>(null);
  const root = useRef<HTMLDivElement>(null);

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
            className="text-muted-foreground hover:text-foreground self-start text-[11px]"
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
        <p className="text-muted-foreground px-3 py-6 text-sm">
          {query.trim() || include.trim() || exclude.trim()
            ? "No matching files."
            : "No files."}
        </p>
      ) : ready ? (
        <ul
          className="min-h-0 flex-1 overflow-y-auto px-1 pb-4 pt-2"
          role="tree"
          aria-label="Workspace files"
        >
          {nodes.map((node) => (
            <TreeRow
              key={node.path}
              node={node}
              depth={0}
              selected={selected}
              isOpen={isOpen}
              onToggle={toggleDir}
              onOpenFile={onOpenFile}
            />
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function TreeRow({
  node,
  depth,
  selected,
  isOpen,
  onToggle,
  onOpenFile,
}: {
  node: FileTreeNode;
  depth: number;
  selected?: string;
  isOpen: (path: string) => boolean;
  onToggle: (path: string) => void;
  onOpenFile: (file: string) => void;
}) {
  const open = node.kind === "dir" && isOpen(node.path);
  const current = node.kind === "file" && selected === node.path;
  const Icon =
    node.kind === "dir" ? (open ? FolderOpen : Folder) : File;

  return (
    <li role="treeitem" aria-expanded={node.kind === "dir" ? open : undefined}>
      <button
        type="button"
        aria-current={current ? true : undefined}
        style={{ paddingLeft: 8 + depth * 12 }}
        className={cn(
          "flex w-full items-center gap-1 rounded-sm py-0.5 pr-2 text-left text-xs",
          current && "bg-muted/60",
          !current && "hover:bg-muted/40",
        )}
        onClick={() => {
          if (node.kind === "dir") onToggle(node.path);
          else onOpenFile(node.path);
        }}
      >
        {node.kind === "dir" ? (
          <ChevronRight
            className={cn(
              "text-muted-foreground size-3 shrink-0 transition-transform",
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
      </button>
      {open && node.children && (
        <ul role="group">
          {node.children.map((child) => (
            <TreeRow
              key={child.path}
              node={child}
              depth={depth + 1}
              selected={selected}
              isOpen={isOpen}
              onToggle={onToggle}
              onOpenFile={onOpenFile}
            />
          ))}
        </ul>
      )}
    </li>
  );
}
