import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { File, Search } from "lucide-react";

import type { ApiClient } from "@/api/client";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { FOCUS_RING, HOVER_TINT } from "./interactive";

const QUICK_OPEN_LIMIT = 5000;
const VISIBLE_RESULTS = 12;

/** Centered, keyboard-first filename quick open for Cmd/Ctrl+P. */
export function CodeQuickOpen({
  client,
  workspaceId,
  contentRevision,
  onOpenFile,
  openRequest = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceTree">;
  workspaceId: string;
  contentRevision: number;
  onOpenFile: (path: string) => void;
  /** Increment to open the picker from a visible New tab control. */
  openRequest?: number;
}) {
  const [open, setOpen] = useState(false);
  const [openWorkspaceId, setOpenWorkspaceId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [paths, setPaths] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const loadedKeyRef = useRef<string | null>(null);
  const treeKey = `${workspaceId}:${contentRevision}`;

  const reveal = useCallback(() => {
    setQuery("");
    setActiveIndex(0);
    setOpenWorkspaceId(workspaceId);
    setOpen(true);
  }, [workspaceId]);

  useEffect(() => {
    function onQuickOpen(event: KeyboardEvent) {
      if (event.key.toLowerCase() !== "p") return;
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      event.preventDefault();
      reveal();
    }
    window.addEventListener("keydown", onQuickOpen);
    return () => window.removeEventListener("keydown", onQuickOpen);
  }, [reveal]);

  useEffect(() => {
    if (openRequest > 0) reveal();
  }, [openRequest, reveal]);

  useEffect(() => {
    loadedKeyRef.current = null;
    setPaths([]);
    setLoading(false);
    setError(null);
    setQuery("");
    setActiveIndex(0);
    setOpenWorkspaceId(null);
    setOpen(false);
  }, [workspaceId]);

  useEffect(() => {
    if (
      !open ||
      openWorkspaceId !== workspaceId ||
      loadedKeyRef.current === treeKey
    ) {
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .listCodeWorkspaceTree(workspaceId, { limit: QUICK_OPEN_LIMIT })
      .then((tree) => {
        if (cancelled) return;
        setPaths(tree.paths);
        loadedKeyRef.current = treeKey;
        setLoading(false);
      })
      .catch((caught) => {
        if (cancelled) return;
        setError(friendlyErrorMessage(caught, "Could not load workspace files"));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId, open, openWorkspaceId, treeKey]);

  const results = useMemo(
    () => rankQuickOpenPaths(paths, query).slice(0, VISIBLE_RESULTS),
    [paths, query],
  );

  useEffect(() => {
    setActiveIndex((current) => Math.min(current, Math.max(0, results.length - 1)));
  }, [results.length]);

  function choose(path: string | undefined) {
    if (!path) return;
    setOpen(false);
    onOpenFile(path);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) setOpenWorkspaceId(null);
      }}
    >
      <DialogContent
        withCloseButton={false}
        className="top-1/2 max-w-2xl gap-0 overflow-hidden rounded-xl p-0 shadow-2xl"
        overlayClassName="bg-black/45 backdrop-blur-[1px]"
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          inputRef.current?.focus();
        }}
      >
        <DialogTitle className="sr-only">Open file</DialogTitle>
        <DialogDescription className="sr-only">
          Search workspace filenames and open a file.
        </DialogDescription>
        <div className="flex items-center gap-2 border-b px-3">
          <Search className="text-muted-foreground size-4 shrink-0" aria-hidden />
          <Input
            ref={inputRef}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActiveIndex(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setActiveIndex((index) =>
                  results.length === 0 ? 0 : (index + 1) % results.length,
                );
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActiveIndex((index) =>
                  results.length === 0
                    ? 0
                    : (index - 1 + results.length) % results.length,
                );
              } else if (event.key === "Enter") {
                event.preventDefault();
                choose(results[activeIndex]);
              }
            }}
            placeholder="Search files by name"
            aria-label="Search files by name"
            autoComplete="off"
            spellCheck={false}
            className="h-12 border-0 bg-transparent px-0 font-mono text-sm shadow-none focus-visible:ring-0"
          />
          {loading && <Spinner className="size-4" aria-label="Loading files" />}
          <kbd className="text-muted-foreground shrink-0 rounded border px-1.5 py-0.5 font-sans text-[10px]">
            esc
          </kbd>
        </div>
        <div className="max-h-[min(55vh,30rem)] min-h-20 overflow-y-auto p-1.5">
          {error ? (
            <p className="text-critical px-3 py-5 text-sm">{error}</p>
          ) : !loading && results.length === 0 ? (
            <p className="text-muted-foreground px-3 py-5 text-sm">
              No filenames match {query ? <span className="font-mono">{query}</span> : "this workspace"}.
            </p>
          ) : (
            results.map((path, index) => {
              const name = fileName(path);
              const parent = parentPath(path);
              return (
                <button
                  key={path}
                  type="button"
                  className={cn(
                    "flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-2 text-left",
                    FOCUS_RING,
                    HOVER_TINT,
                    index === activeIndex && "bg-accent text-accent-foreground",
                  )}
                  aria-label={path}
                  onMouseMove={() => setActiveIndex(index)}
                  onClick={() => choose(path)}
                >
                  <File className="text-muted-foreground size-4 shrink-0" aria-hidden />
                  <span className="min-w-0 flex-1 truncate font-mono text-sm">{name}</span>
                  {parent && (
                    <span className="text-muted-foreground min-w-0 max-w-[55%] truncate font-mono text-[11px]">
                      {parent}
                    </span>
                  )}
                </button>
              );
            })
          )}
        </div>
        <div className="text-muted-foreground flex items-center justify-end gap-3 border-t px-3 py-1.5 text-[10px]">
          <span>↑↓ navigate</span>
          <span>↵ open</span>
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function rankQuickOpenPaths(
  paths: readonly string[],
  query: string,
): string[] {
  const tokens = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return [...paths].sort(pathSort);
  const pathQuery = query.includes("/") || query.includes("\\");
  return paths
    .map((path) => {
      const target = pathQuery ? path.replace(/\\/g, "/") : fileName(path);
      const score = tokens.reduce((total, token) => {
        const next = fuzzyTokenScore(target.toLocaleLowerCase(), token);
        return total < 0 || next < 0 ? -1 : total + next;
      }, 0);
      return { path, score };
    })
    .filter((entry) => entry.score >= 0)
    .sort((left, right) => right.score - left.score || pathSort(left.path, right.path))
    .map((entry) => entry.path);
}

function fuzzyTokenScore(target: string, token: string): number {
  if (target === token) return 2000;
  const contiguous = target.indexOf(token);
  if (contiguous >= 0) {
    return 1200 - contiguous * 8 - (target.length - token.length);
  }
  let cursor = 0;
  let score = 0;
  let previous = -2;
  for (const char of token) {
    const index = target.indexOf(char, cursor);
    if (index < 0) return -1;
    score += index === previous + 1 ? 24 : Math.max(2, 14 - index);
    if (index === 0 || /[-_.]/.test(target[index - 1] ?? "")) score += 16;
    previous = index;
    cursor = index + 1;
  }
  return score - target.length;
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function parentPath(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const separator = normalized.lastIndexOf("/");
  return separator < 0 ? "" : normalized.slice(0, separator);
}

function pathSort(left: string, right: string): number {
  return fileName(left).localeCompare(fileName(right)) || left.localeCompare(right);
}
