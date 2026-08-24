import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { File } from "lucide-react";

import type { ApiClient } from "@/api/client";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { fuzzyTokenScore, queryTokens } from "@/fuzzy";
import { friendlyErrorMessage } from "@/lib/utils";

const QUICK_OPEN_LIMIT = 5000;
const VISIBLE_RESULTS = 12;

/**
 * Centered, keyboard-first filename quick open.
 *
 * The chords that reach it — Cmd+T and Cmd+P — are in the shell keymap rather
 * than a listener here, so they appear in the shortcuts dialog and respect the
 * guard that keeps every shell chord out of an open dialog.
 */
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
  /** Increment to open the picker: a New tab control, or the shell keymap. */
  openRequest?: number;
}) {
  const [open, setOpen] = useState(false);
  const [openWorkspaceId, setOpenWorkspaceId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [paths, setPaths] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loadedKeyRef = useRef<string | null>(null);
  const treeKey = `${workspaceId}:${contentRevision}`;

  const reveal = useCallback(() => {
    setQuery("");
    setOpenWorkspaceId(workspaceId);
    setOpen(true);
  }, [workspaceId]);

  useEffect(() => {
    if (openRequest > 0) reveal();
  }, [openRequest, reveal]);

  useEffect(() => {
    loadedKeyRef.current = null;
    setPaths([]);
    setLoading(false);
    setError(null);
    setQuery("");
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
        setError(
          friendlyErrorMessage(caught, "Could not load workspace files"),
        );
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
      >
        <DialogTitle className="sr-only">Open file</DialogTitle>
        <DialogDescription className="sr-only">
          Search workspace filenames and open a file.
        </DialogDescription>
        <Command
          shouldFilter={false}
          label="Search files by name"
          className="rounded-none bg-transparent"
        >
          <div className="relative">
            <CommandInput
              value={query}
              onValueChange={setQuery}
              placeholder="Search files by name"
              autoComplete="off"
              spellCheck={false}
              className="h-12 font-mono"
            />
            {loading && (
              <Spinner
                className="absolute top-1/2 right-12 size-4 -translate-y-1/2"
                aria-label="Loading files"
              />
            )}
            <kbd className="text-muted-foreground absolute top-1/2 right-3 -translate-y-1/2 shrink-0 rounded border px-1.5 py-0.5 font-sans text-2xs">
              esc
            </kbd>
          </div>
          <CommandList className="max-h-[min(55vh,30rem)] min-h-20 p-1.5">
            {error ? (
              <p className="text-critical px-3 py-5 text-sm">{error}</p>
            ) : (
              <>
                <CommandEmpty className="text-muted-foreground px-3 py-5 text-sm">
                  No filenames match{" "}
                  {query ? (
                    <span className="font-mono">{query}</span>
                  ) : (
                    "this workspace"
                  )}
                  .
                </CommandEmpty>
                <CommandGroup className="p-0">
                  {results.map((path) => {
                    const name = fileName(path);
                    const parent = parentPath(path);
                    return (
                      <CommandItem
                        key={path}
                        value={path}
                        aria-label={path}
                        onSelect={() => choose(path)}
                        className="gap-2 px-2.5 py-2 font-mono"
                      >
                        <File
                          className="text-muted-foreground size-4 shrink-0"
                          aria-hidden
                        />
                        <span className="min-w-0 flex-1 truncate text-sm">
                          {name}
                        </span>
                        {parent && (
                          <span className="text-muted-foreground min-w-0 max-w-[55%] truncate text-xs">
                            {parent}
                          </span>
                        )}
                      </CommandItem>
                    );
                  })}
                </CommandGroup>
              </>
            )}
          </CommandList>
        </Command>
        <div className="text-muted-foreground flex items-center justify-end gap-3 border-t px-3 py-1.5 text-2xs">
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
  const tokens = queryTokens(query);
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
    .sort(
      (left, right) =>
        right.score - left.score || pathSort(left.path, right.path),
    )
    .map((entry) => entry.path);
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
  return (
    fileName(left).localeCompare(fileName(right)) || left.localeCompare(right)
  );
}
