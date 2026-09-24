import { type ReactNode, useCallback, useId, useMemo, useState } from "react";
import { ChevronRight, FileCode2, Undo2 } from "lucide-react";

import type { ApiClient } from "../api/client";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { OpenInEditorButton } from "./OpenInEditorButton";
import { DiffstatBadge } from "./TurnReviewCard";
import { useLiveResource } from "./useLiveContent";
import { HEADER_CAPTION, WorkspaceRevisionChip } from "./WorkspaceRevisionChip";
import type { DiffFileGroup, DiffHunk, DiffLine } from "./unifiedDiff";
import { diffHunks, fileChangeOf, groupUnifiedDiff } from "./unifiedDiff";
import type { RevertRequest } from "./worktreeUndo";

/** Files longer than this start collapsed behind "Show diff". */
export const DIFF_COLLAPSE_LINE_THRESHOLD = 400;

export type {
  DiffFileGroup,
  DiffLine,
  DiffLineKind,
} from "./unifiedDiff";
export { groupUnifiedDiff } from "./unifiedDiff";

/**
 * Reverting from the diff: the whole file, or one hunk. The host asks first
 * and has the server apply it; a handler resolves `true` once it landed.
 */
export type DiffRevertActions = {
  onRevertFile: (request: RevertRequest) => Promise<boolean>;
  onRevertHunk: (request: RevertRequest, hunk: DiffHunk) => Promise<boolean>;
  /**
   * Why nothing can be reverted right now, such as a turn running. The
   * controls stay in place, turned off, with this sentence as their title.
   */
  unavailableReason?: string;
};

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
  onOpenInEditor,
  revert,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceDiff">;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  file?: string;
  contentRevision?: number;
  onOpenFile?: (path: string) => void;
  /** Hand the scoped file to the reader's own editor. */
  onOpenInEditor?: (path: string) => void;
  /** Revert a file or a hunk. Absent where the worktree is not ours to change. */
  revert?: DiffRevertActions;
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
  const reverts = useRevertTracker(revert, turnId);

  const scopeCaption = file
    ? file
    : turnId
      ? (turnLabel ?? "This turn")
      : "Workspace vs base";

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {/*
        The caption takes the row until it would drop below its basis; then
        the controls wrap under it instead of squeezing the path beside the
        revision chip. `FileViewer` uses the same basis so the two headers
        wrap at the same width.
      */}
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 border-b px-3 py-2">
        <div className={HEADER_CAPTION}>
          <h2 className="text-sm font-medium">Diff</h2>
          <MiddleTruncate
            text={scopeCaption}
            className="text-muted-foreground font-mono text-xs"
          />
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <WorkspaceRevisionChip
            revision={payload?.revision}
            revisionRef={payload?.revision_ref}
            savedAt={payload?.revision_saved_at}
          />
          {payload && <DiffstatBadge stat={payload.stat} />}
          {file && reverts && groups.length > 0 && (
            <RevertFileButton
              group={groups[0]}
              path={file}
              reverts={reverts}
              placement="header"
            />
          )}
          {file && onOpenFile && (
            <button
              type="button"
              className={cn(HEADER_ACTION, FOCUS_RING_TIGHT, HOVER_TINT)}
              onClick={() => onOpenFile(file)}
            >
              <FileCode2 className="size-3" aria-hidden />
              Open file
            </button>
          )}
          {file && onOpenInEditor && (
            <OpenInEditorButton onClick={() => onOpenInEditor(file)} />
          )}
          {/* A fixed trailing slot, so a refresh moves nothing and an idle
              slot never opens a gap between the chip and its neighbors. */}
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {payload?.truncated && (
        <p className="text-muted-foreground border-b px-3 py-2 text-xs">
          This diff was truncated. Open a single file for the rest.
        </p>
      )}
      <div
        className="min-h-0 flex-1 overflow-y-auto"
        tabIndex={0}
        aria-label="Diff"
      >
        {!payload && !error && (
          <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
            <Skeleton className="h-3 w-1/3" />
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        )}
        {file && groups.length === 1 ? (
          <DiffBody group={groups[0]} reverts={reverts} />
        ) : (
          groups.map((group) => (
            <FileDiffSection
              key={group.path}
              group={group}
              onOpenFile={onOpenFile}
              reverts={reverts}
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

/** A header action: a quiet icon and word, like "Open file". */
const HEADER_ACTION =
  "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-xs whitespace-nowrap disabled:cursor-not-allowed disabled:opacity-60";

/**
 * The revert controls' shared state for one open diff.
 *
 * A turn's diff is history: reverting its hunk changes the worktree, not the
 * checkpoints the diff compares, so the hunk stays on screen. It is marked
 * reverted instead, so the reader sees their action landed and is not
 * offered a second revert that can only fail. The workspace diff re-reads
 * the worktree, and a reverted hunk simply leaves it.
 */
type RevertTracker = {
  actions: DiffRevertActions;
  turnId?: string;
  reverted: ReadonlySet<string>;
  pending: string | null;
  run: (key: string, action: () => Promise<boolean>) => void;
};

function useRevertTracker(
  actions: DiffRevertActions | undefined,
  turnId: string | undefined,
): RevertTracker | null {
  const [reverted, setReverted] = useState<ReadonlySet<string>>(new Set());
  const [pending, setPending] = useState<string | null>(null);
  const run = useCallback((key: string, action: () => Promise<boolean>) => {
    setPending(key);
    void action()
      .then((landed) => {
        if (landed) setReverted((current) => new Set(current).add(key));
      })
      .finally(() => setPending(null));
  }, []);
  if (!actions) return null;
  return { actions, turnId, reverted, pending, run };
}

function revertKey(path: string, hunk?: number): string {
  return hunk === undefined ? path : `${path}#${hunk}`;
}

/**
 * Revert one whole file. In the panel header it reads like "Open file"
 * beside it; in a file's own row it is one quiet word, like "Open".
 */
function RevertFileButton({
  group,
  path,
  reverts,
  placement,
}: {
  /** The file's section of the diff, which says what kind of change it is. */
  group: DiffFileGroup;
  path: string;
  reverts: RevertTracker;
  placement: "header" | "row";
}) {
  const key = revertKey(path);
  if (reverts.turnId && reverts.reverted.has(key)) {
    return <RevertedLabel />;
  }
  const busy = reverts.pending === key;
  const unavailableReason = reverts.actions.unavailableReason;
  const onClick = () =>
    reverts.run(key, () =>
      reverts.actions.onRevertFile({
        path,
        turnId: reverts.turnId,
        ...fileChangeOf(group),
      }),
    );
  if (placement === "header") {
    return (
      <button
        type="button"
        aria-label={`Revert ${path}`}
        title={unavailableReason}
        disabled={busy || unavailableReason !== undefined}
        className={cn(HEADER_ACTION, FOCUS_RING_TIGHT, HOVER_TINT)}
        onClick={onClick}
      >
        <Undo2 className="size-3" aria-hidden />
        {busy ? "Reverting…" : "Revert file"}
      </button>
    );
  }
  return (
    <RevertButton
      label="Revert"
      ariaLabel={`Revert ${path}`}
      busy={busy}
      unavailableReason={unavailableReason}
      onClick={onClick}
    />
  );
}

function RevertButton({
  label,
  ariaLabel,
  busy,
  unavailableReason,
  onClick,
}: {
  label: string;
  ariaLabel: string;
  busy: boolean;
  unavailableReason?: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={ariaLabel}
      title={unavailableReason}
      disabled={busy || unavailableReason !== undefined}
      className={cn(
        "text-muted-foreground hover:text-foreground shrink-0 cursor-pointer rounded-sm px-1 font-sans text-xs whitespace-nowrap underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:no-underline",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      {busy ? "Reverting…" : label}
    </button>
  );
}

function RevertedLabel() {
  return (
    <span className="text-success-foreground shrink-0 px-1 font-sans text-xs whitespace-nowrap">
      Reverted
    </span>
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
  reverts,
}: {
  group: DiffFileGroup;
  onOpenFile?: (path: string) => void;
  reverts: RevertTracker | null;
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
        {reverts && (
          <RevertFileButton
            group={group}
            path={group.path}
            reverts={reverts}
            placement="row"
          />
        )}
        {onOpenFile && (
          <button
            type="button"
            // The visible word is enough beside its own file name; a reader
            // tabbing or listing controls gets one "Open" per file and needs
            // the path to tell them apart.
            aria-label={`Open ${group.path}`}
            className={cn(
              "text-muted-foreground hover:text-foreground shrink-0 cursor-pointer rounded-sm text-xs underline-offset-2 hover:underline",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            onClick={() => onOpenFile(group.path)}
          >
            Open
          </button>
        )}
        <span className="shrink-0 font-mono text-xs tabular-nums">
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
        <DiffBody group={group} id={bodyId} reverts={reverts} />
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

function DiffBody({
  group,
  id,
  reverts,
}: {
  group: DiffFileGroup;
  id?: string;
  reverts?: RevertTracker | null;
}) {
  // Only a hunk shown whole can be reverted as shown; the last hunk of a
  // diff cut at its size cap is not.
  const revertable = reverts !== null && reverts !== undefined;
  const hunks = useMemo(
    () =>
      revertable
        ? new Map(
            diffHunks(group)
              .filter((hunk) => hunk.complete)
              .map((hunk) => [hunk.line, hunk]),
          )
        : null,
    [group, revertable],
  );
  const fileReverted =
    reverts?.turnId !== undefined &&
    reverts.reverted.has(revertKey(group.path));
  return (
    <pre id={id} className="overflow-x-auto py-1 font-mono text-md leading-5">
      {group.lines.map((line, index) => {
        const hunk = hunks?.get(index);
        return (
          <DiffLineRow
            key={`${group.path}:${index}`}
            line={line}
            action={
              hunk && reverts ? (
                <HunkRevert
                  group={group}
                  hunk={hunk}
                  reverts={reverts}
                  fileReverted={fileReverted}
                />
              ) : undefined
            }
          />
        );
      })}
    </pre>
  );
}

function HunkRevert({
  group,
  hunk,
  reverts,
  fileReverted,
}: {
  group: DiffFileGroup;
  hunk: DiffHunk;
  reverts: RevertTracker;
  fileReverted: boolean;
}) {
  const path = group.path;
  const key = revertKey(path, hunk.index);
  if (reverts.turnId && (fileReverted || reverts.reverted.has(key))) {
    return <RevertedLabel />;
  }
  const lines =
    hunk.newCount > 1
      ? `lines ${hunk.newStart} to ${hunk.newStart + hunk.newCount - 1}`
      : `line ${Math.max(hunk.newStart, 1)}`;
  return (
    <RevertButton
      label="Revert"
      ariaLabel={`Revert the change at ${lines} of ${path}`}
      busy={reverts.pending === key}
      unavailableReason={reverts.actions.unavailableReason}
      onClick={() =>
        reverts.run(key, () =>
          reverts.actions.onRevertHunk(
            { path, turnId: reverts.turnId, ...fileChangeOf(group) },
            hunk,
          ),
        )
      }
    />
  );
}

function DiffLineRow({
  line,
  action,
}: {
  line: DiffLine;
  /**
   * A control that belongs to this line; only hunk headers carry one. It
   * sits in the gutter, which a hunk header leaves empty, so a long header
   * line never pushes it out of view.
   */
  action?: ReactNode;
}) {
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
          "border-info-border/60 bg-info-background/45 text-info-foreground my-1 items-center border-y border-l-0",
        line.kind === "meta" &&
          "text-muted-foreground bg-muted/20 border-l-0 text-xs",
      )}
    >
      {action ? (
        <span
          className="bg-background/35 flex w-[10.5ch] shrink-0 select-none items-center justify-end self-stretch border-r text-xs"
          data-diff-gutter="action"
        >
          {action}
        </span>
      ) : (
        <>
          <span
            className="text-muted-foreground bg-background/35 w-[5.25ch] shrink-0 select-none border-r px-1 text-right text-xs tabular-nums"
            data-diff-gutter="old"
          >
            {line.oldNo ?? ""}
          </span>
          <span
            className="text-muted-foreground bg-background/35 w-[5.25ch] shrink-0 select-none border-r px-1 text-right text-xs tabular-nums"
            data-diff-gutter="new"
          >
            {line.newNo ?? ""}
          </span>
        </>
      )}
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
