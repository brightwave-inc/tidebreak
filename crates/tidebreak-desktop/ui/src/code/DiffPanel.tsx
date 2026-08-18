import { useCallback, useMemo, useState } from "react";
import { ChevronRight } from "lucide-react";

import type { ApiClient } from "../api/client";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { DiffstatBadge } from "./TurnReviewCard";
import { useLiveResource } from "./useLiveContent";

/** Files longer than this start collapsed behind "Show diff". */
export const DIFF_COLLAPSE_LINE_THRESHOLD = 400;

export type DiffLineKind = "add" | "del" | "context" | "hunk" | "meta";

export type DiffLine = {
  kind: DiffLineKind;
  oldNo: number | null;
  newNo: number | null;
  text: string;
};

export type DiffFileGroup = {
  path: string;
  lines: DiffLine[];
};

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/**
 * Server-produced unified diff, grouped per file and tinted with the
 * semantic status tokens. No client-side highlighting: fragments misparse,
 * fight the add/del tints, and cost a pass we do not need.
 */
export function DiffPanel({
  client,
  workspaceId,
  turnId,
  turnLabel,
  file,
  contentRevision = 0,
  onOpenFile,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceDiff">;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  file?: string;
  contentRevision?: number;
  onOpenFile?: (path: string) => void;
}) {
  const load = useCallback(
    () => client.getCodeWorkspaceDiff(workspaceId, { turn: turnId, file }),
    [client, workspaceId, turnId, file],
  );
  const {
    data: payload,
    error,
    refreshing,
  } = useLiveResource({
    key: `${workspaceId}${turnId ?? ""}${file ?? ""}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not load the diff",
  });

  const groups = useMemo(
    () => (payload ? groupUnifiedDiff(payload.diff) : []),
    [payload],
  );

  const scopeCaption = file
    ? file
    : turnId
      ? (turnLabel ?? "This turn")
      : "Workspace vs base";

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
        <div className="min-w-0">
          <h2 className="text-sm font-medium">Diff</h2>
          <p
            className="text-muted-foreground truncate font-mono text-[11px]"
            title={scopeCaption}
          >
            {scopeCaption}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {refreshing && <Spinner className="size-3.5" aria-label="Refreshing" />}
          {payload && <DiffstatBadge stat={payload.stat} />}
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {payload?.truncated && (
        <p className="text-muted-foreground border-b px-3 py-2 text-xs">
          This diff was truncated. Open a single file for the rest.
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {!payload && !error && (
          <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
            <Skeleton className="h-3 w-1/3" />
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        )}
        {groups.map((group) => (
          <FileDiffSection
            key={group.path}
            group={group}
            onOpenFile={onOpenFile}
          />
        ))}
        {payload && groups.length === 0 && !error && (
          <p className="text-muted-foreground px-3 py-6 text-sm">No diff.</p>
        )}
      </div>
    </div>
  );
}

function FileDiffSection({
  group,
  onOpenFile,
}: {
  group: DiffFileGroup;
  onOpenFile?: (path: string) => void;
}) {
  const large = group.lines.length > DIFF_COLLAPSE_LINE_THRESHOLD;
  const [expanded, setExpanded] = useState(!large);
  const { insertions, deletions } = fileDiffstat(group.lines);

  return (
    <section className="border-b last:border-b-0">
      <header className="bg-background sticky top-0 z-10">
        <button
          type="button"
          className={cn(
            "hover:bg-muted/40 flex w-full cursor-pointer items-center gap-1.5 px-3 py-1.5 text-left",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          aria-expanded={expanded}
          onClick={() => setExpanded((current) => !current)}
        >
          <ChevronRight
            className={cn(
              "text-muted-foreground size-3 shrink-0 transition-transform duration-[140ms] ease-out motion-reduce:transition-none",
              expanded && "rotate-90",
            )}
            aria-hidden="true"
          />
          <h3
            className="text-muted-foreground min-w-0 flex-1 truncate font-mono text-xs"
            title={group.path}
          >
            {group.path}
          </h3>
          {onOpenFile && (
            <span
              role="link"
              tabIndex={0}
              className={cn(
                "text-muted-foreground hover:text-foreground shrink-0 cursor-pointer rounded-sm text-[11px] underline-offset-2 hover:underline",
                FOCUS_RING_TIGHT,
                HOVER_TINT,
              )}
              onClick={(event) => {
                event.stopPropagation();
                onOpenFile(group.path);
              }}
              onKeyDown={(event) => {
                if (event.key !== "Enter" && event.key !== " ") return;
                event.preventDefault();
                event.stopPropagation();
                onOpenFile(group.path);
              }}
            >
              Open
            </span>
          )}
          <span className="shrink-0 font-mono text-[11px] tabular-nums">
            <span className="text-success">+{insertions}</span>{" "}
            <span className="text-critical">−{deletions}</span>
          </span>
        </button>
      </header>
      {expanded ? (
        <pre className="overflow-x-auto py-1 font-mono text-xs leading-5">
          {group.lines.map((line, index) => (
            <DiffLineRow
              key={`${group.path}:${index}`}
              line={line}
            />
          ))}
        </pre>
      ) : large ? (
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:text-foreground cursor-pointer rounded-sm px-3 py-2 text-xs",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          onClick={() => setExpanded(true)}
        >
          Show diff
        </button>
      ) : null}
    </section>
  );
}

function DiffLineRow({ line }: { line: DiffLine }) {
  return (
    <span
      className={cn(
        "flex min-h-4 min-w-max",
        line.kind === "add" && "text-success bg-success/10",
        line.kind === "del" && "text-critical bg-critical/10",
        (line.kind === "hunk" || line.kind === "meta") && "text-muted-foreground",
      )}
    >
      <span
        className="text-muted-foreground w-[3.25ch] shrink-0 select-none text-right text-[11px] tabular-nums"
        data-diff-gutter="old"
      >
        {line.oldNo ?? ""}
      </span>
      <span
        className="text-muted-foreground w-[3.25ch] shrink-0 select-none pr-2 text-right text-[11px] tabular-nums"
        data-diff-gutter="new"
      >
        {line.newNo ?? ""}
      </span>
      <span className="px-1 whitespace-pre">{line.text || " "}</span>
    </span>
  );
}

function fileDiffstat(lines: readonly DiffLine[]): {
  insertions: number;
  deletions: number;
} {
  let insertions = 0;
  let deletions = 0;
  for (const line of lines) {
    if (line.kind === "add") insertions += 1;
    else if (line.kind === "del") deletions += 1;
  }
  return { insertions, deletions };
}

/**
 * Split a unified diff into per-file groups of structured lines.
 *
 * Hunk headers drive the old/new counters. Rename and other git headers stay
 * `meta` so they never steal a gutter number from the patch they describe.
 */
export function groupUnifiedDiff(diff: string): DiffFileGroup[] {
  const groups: DiffFileGroup[] = [];
  let current: DiffFileGroup | null = null;
  let oldCursor = 0;
  let newCursor = 0;
  let inHunk = false;

  function ensureGroup(path: string): DiffFileGroup {
    if (current) return current;
    current = { path, lines: [] };
    groups.push(current);
    return current;
  }

  for (const line of diff.split("\n")) {
    const file = /^diff --git a\/(.+?) b\/(.+)$/.exec(line);
    if (file) {
      current = { path: file[2] ?? file[1] ?? "file", lines: [] };
      groups.push(current);
      oldCursor = 0;
      newCursor = 0;
      inHunk = false;
      continue;
    }

    if (line.length === 0) continue;

    const group = ensureGroup("file");
    const hunk = HUNK_HEADER.exec(line);
    if (hunk) {
      oldCursor = Number(hunk[1]);
      newCursor = Number(hunk[3]);
      inHunk = true;
      group.lines.push({ kind: "hunk", oldNo: null, newNo: null, text: line });
      continue;
    }

    if (line.startsWith("+") && !line.startsWith("+++")) {
      group.lines.push({
        kind: "add",
        oldNo: null,
        newNo: inHunk ? newCursor : null,
        text: line,
      });
      if (inHunk) newCursor += 1;
      continue;
    }
    if (line.startsWith("-") && !line.startsWith("---")) {
      group.lines.push({
        kind: "del",
        oldNo: inHunk ? oldCursor : null,
        newNo: null,
        text: line,
      });
      if (inHunk) oldCursor += 1;
      continue;
    }
    if (inHunk && line.startsWith(" ")) {
      group.lines.push({
        kind: "context",
        oldNo: oldCursor,
        newNo: newCursor,
        text: line,
      });
      oldCursor += 1;
      newCursor += 1;
      continue;
    }

    group.lines.push({ kind: "meta", oldNo: null, newNo: null, text: line });
  }

  return groups.filter((group) => group.lines.length > 0 || groups.length === 1);
}
