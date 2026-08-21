import { useCallback, useId, useMemo, useState } from "react";
import { ChevronRight, FileCode2 } from "lucide-react";

import type { ApiClient } from "../api/client";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
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
          <MiddleTruncate
            text={scopeCaption}
            className="text-muted-foreground font-mono text-[11px]"
          />
        </div>
        <div className="flex items-center gap-2">
          {/* A fixed slot, so a refresh does not nudge the diffstat sideways. */}
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
          {payload && <DiffstatBadge stat={payload.stat} />}
          {file && onOpenFile && (
            <button
              type="button"
              className={cn(
                "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-[11px]",
                FOCUS_RING_TIGHT,
                HOVER_TINT,
              )}
              onClick={() => onOpenFile(file)}
            >
              <FileCode2 className="size-3" aria-hidden />
              Open file
            </button>
          )}
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
        {file && groups.length === 1 ? (
          <DiffBody group={groups[0]} />
        ) : (
          groups.map((group) => (
            <FileDiffSection
              key={group.path}
              group={group}
              onOpenFile={onOpenFile}
            />
          ))
        )}
        {payload && groups.length === 0 && !error && (
          <p className="text-muted-foreground px-3 py-6 text-sm">
            {emptyDiffText(file, turnId, turnLabel)}
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * What an empty diff means, which depends entirely on what it was scoped to.
 *
 * The three cases are three different facts — this file is unchanged, this
 * turn wrote nothing, the whole worktree matches its base — and a reader
 * scoping the panel to a turn is asking exactly the question the middle one
 * answers. One line of shared copy for all three ("No diff.") answers none.
 */
function emptyDiffText(
  file?: string,
  turnId?: string,
  turnLabel?: string,
): string {
  if (file) return "No changes in this file.";
  if (turnId) return `${turnLabel ?? "This turn"} changed no files.`;
  return "The worktree matches its base branch.";
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
  const bodyId = useId();
  const { insertions, deletions } = fileDiffstat(group.lines);

  return (
    <section className="border-b last:border-b-0">
      {/*
        The disclosure and "Open" are two controls, not one nested in the
        other: a button inside a button is neither reachable nor announceable,
        and the disclosure's name swallowed the word "Open" along with the
        counts. They sit side by side and read as one row.
      */}
      <header className="bg-background sticky top-0 z-10 flex items-center gap-1.5 pr-3">
        {/*
          The heading wraps the disclosure rather than sitting inside it: a
          heading is how a reader jumps between files, and a button is how they
          open one. Nesting either in the other loses one of the two.
        */}
        <h3 className="min-w-0 flex-1">
          <button
            type="button"
            className={cn(
              "hover:bg-muted/40 flex w-full min-w-0 cursor-pointer items-center gap-1.5 px-3 py-1.5 text-left",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            aria-expanded={expanded}
            aria-controls={expanded ? bodyId : undefined}
            onClick={() => setExpanded((current) => !current)}
          >
            <ChevronRight
              className={cn(
                "text-muted-foreground size-3 shrink-0 transition-transform duration-[140ms] ease-out motion-reduce:transition-none",
                expanded && "rotate-90",
              )}
              aria-hidden="true"
            />
            <MiddleTruncate
              text={group.path}
              className="text-muted-foreground min-w-0 flex-1 font-mono text-xs"
            />
          </button>
        </h3>
        {onOpenFile && (
          <button
            type="button"
            // The visible word is enough beside its own file name; a reader
            // tabbing or listing controls gets one "Open" per file and needs
            // the path to tell them apart.
            aria-label={`Open ${group.path}`}
            className={cn(
              "text-muted-foreground hover:text-foreground shrink-0 cursor-pointer rounded-sm text-[11px] underline-offset-2 hover:underline",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            onClick={() => onOpenFile(group.path)}
          >
            Open
          </button>
        )}
        <span className="shrink-0 font-mono text-[11px] tabular-nums">
          {/*
            `--success` and `--critical` are mark colours: they clear 3:1
            against either background, which an icon needs and a numeral this
            small does not. The `-foreground` inks clear 9:1 in both themes.
          */}
          <span className="text-success-foreground">+{insertions}</span>{" "}
          <span className="text-critical-foreground">−{deletions}</span>
        </span>
      </header>
      {expanded ? (
        <DiffBody group={group} id={bodyId} />
      ) : large ? (
        <button
          type="button"
          aria-label={`Show diff for ${group.path}`}
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

function DiffBody({ group, id }: { group: DiffFileGroup; id?: string }) {
  return (
    <pre
      id={id}
      className="overflow-x-auto py-1 font-mono text-[13px] leading-5"
    >
      {group.lines.map((line, index) => (
        <DiffLineRow key={`${group.path}:${index}`} line={line} />
      ))}
    </pre>
  );
}

function DiffLineRow({ line }: { line: DiffLine }) {
  if (line.kind === "meta" && isNoisyDiffMeta(line.text)) return null;

  return (
    // The tint carries "added" or "removed"; the ink carries readability. Tinted
    // ink on a tinted row is what made added lines a 3.2:1 pale green in the
    // light theme while reading fine in the dark one.
    <span
      className={cn(
        "flex min-h-5 min-w-max border-l-2 border-transparent",
        line.kind === "add" &&
          "border-success-border bg-success-background/55 text-success-foreground",
        line.kind === "del" &&
          "border-critical-border bg-critical-background/55 text-critical-foreground",
        line.kind === "context" && "text-foreground/90",
        line.kind === "hunk" &&
          "border-info-border/60 bg-info-background/45 text-info-foreground my-1 border-y border-l-0",
        line.kind === "meta" &&
          "text-muted-foreground bg-muted/20 border-l-0 text-[11px]",
      )}
    >
      <span
        className="text-muted-foreground/80 bg-background/35 w-[5.25ch] shrink-0 select-none border-r px-1 text-right text-[11px] tabular-nums"
        data-diff-gutter="old"
      >
        {line.oldNo ?? ""}
      </span>
      <span
        className="text-muted-foreground/80 bg-background/35 w-[5.25ch] shrink-0 select-none border-r px-1 text-right text-[11px] tabular-nums"
        data-diff-gutter="new"
      >
        {line.newNo ?? ""}
      </span>
      <span className="px-1 whitespace-pre">{line.text || " "}</span>
    </span>
  );
}

function isNoisyDiffMeta(text: string): boolean {
  return (
    text.startsWith("index ") ||
    text.startsWith("--- ") ||
    text.startsWith("+++ ")
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

  return groups.filter(
    (group) => group.lines.length > 0 || groups.length === 1,
  );
}
