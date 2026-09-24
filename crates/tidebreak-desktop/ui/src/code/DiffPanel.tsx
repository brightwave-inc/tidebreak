import {
  type KeyboardEvent,
  useCallback,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { ChevronRight, FileCode2, Undo2 } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import { useConfirm } from "@/components/ConfirmDialog";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { changedFileOrder } from "./DiffOverview";
import { CommentCard, CommentComposer } from "./diff/DiffComments";
import { diffFileKey, stepDiffFile } from "./diff/diffKeys";
import { useDiffPreferences } from "./diff/diffPreferences";
import { DiffView, type HunkAction } from "./diff/DiffView";
import { DiffViewOptions } from "./diff/DiffViewOptions";
import { usePendingReviewStore } from "./diff/pendingReview";
import {
  commentLinesLabel,
  spansOf,
  type ReviewComment,
} from "./diff/reviewComments";
import {
  useWorkspaceDiffReview,
  type WorkspaceDiffReview,
} from "./diff/useWorkspaceDiffReview";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { OpenInEditorButton } from "./OpenInEditorButton";
import {
  DiffReviewStatus,
  ReviewChangesControl,
  type DiffReviewerContext,
} from "./review/ReviewChanges";
import { DiffstatBadge } from "./TurnReviewCard";
import { useLiveResource } from "./useLiveContent";
import { HEADER_CAPTION, WorkspaceRevisionChip } from "./WorkspaceRevisionChip";
import type { DiffFileGroup, DiffHunk, DiffLine } from "./unifiedDiff";
import { diffHunks, fileChangeOf, groupUnifiedDiff } from "./unifiedDiff";
import type { RevertRequest } from "./worktreeUndo";
import { Notice, NoticeRetryButton } from "@/components/ui/notice";

/** Files longer than this start collapsed behind "Show diff". */
export const DIFF_COLLAPSE_LINE_THRESHOLD = 400;

const NO_PATHS: ReadonlySet<string> = new Set();

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

type DiffPanelClient = Pick<ApiClient, "getCodeWorkspaceDiff"> &
  Partial<Pick<ApiClient, "listCodeWorkspaceFiles">>;

/**
 * A workspace's diff: the worktree against its base, one turn's changes, or
 * one file of either, drawn by the shared `DiffView`.
 *
 * Line comments written here wait in the workspace's pending review and go
 * to the agent with the next message. J and K move between files; on one
 * file's diff they open the next or previous changed file in its place.
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
  onStepFile,
  revert,
  comments = true,
  reviewer,
}: {
  client: DiffPanelClient;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  file?: string;
  contentRevision?: number;
  onOpenFile?: (path: string) => void;
  /** Hand the scoped file to the reader's own editor. */
  onOpenInEditor?: (path: string) => void;
  /**
   * Show another changed file's diff in place of this one. With it, J and K
   * on a one-file diff walk the changed files in the order Changes lists them.
   */
  onStepFile?: (path: string) => void;
  /** Revert a file or a hunk. Absent where the worktree is not ours to change. */
  revert?: DiffRevertActions;
  /** Take line comments. Off where nobody here can send them to an agent. */
  comments?: boolean;
  /**
   * Offer Review changes: another engine reviews the changes, read-only,
   * and its findings land here as comments. Absent where no conversation
   * could take them.
   */
  reviewer?: DiffReviewerContext;
}) {
  const load = useCallback(
    () => client.getCodeWorkspaceDiff(workspaceId, { turn: turnId, file }),
    [client, workspaceId, turnId, file],
  );
  const {
    data: payload,
    error,
    refreshing,
    refresh,
  } = useLiveResource({
    key: `${workspaceId}${turnId ?? ""}${file ?? ""}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not load the diff",
  });

  // Keyed on the text: a refresh that finds the same diff keeps the same
  // groups, so nothing below draws again.
  const diffText = payload?.diff;
  const groups = useMemo(
    () => (diffText === undefined ? [] : groupUnifiedDiff(diffText)),
    [diffText],
  );
  const reverts = useRevertTracker(revert, turnId);
  const layout = useDiffPreferences((state) => state.layout);
  const [ignoreWhitespace, setIgnoreWhitespace] = useState(false);
  const { confirm, dialog } = useConfirm();
  const deleteComment = useCallback(
    async (id: string) => {
      const confirmed = await confirm({
        title: "Delete this comment?",
        description: "It will not go to the agent, and its text is lost.",
        confirmLabel: "Delete",
        destructive: true,
      });
      if (confirmed) usePendingReviewStore.getState().remove(workspaceId, id);
    },
    [confirm, workspaceId],
  );
  const onDeleteComment = useCallback(
    (id: string) => void deleteComment(id),
    [deleteComment],
  );
  // A reviewer's finding goes without asking: its text is the reviewer's,
  // not the reader's, and the toast puts it back.
  const onDismissFinding = useCallback(
    (id: string) => {
      const store = usePendingReviewStore.getState();
      const finding = store.byWorkspace[workspaceId]?.find(
        (comment) => comment.id === id,
      );
      if (!finding) return;
      store.remove(workspaceId, id);
      toast("Finding dismissed", {
        action: {
          label: "Undo",
          onClick: () =>
            usePendingReviewStore.getState().restore(workspaceId, [finding]),
        },
      });
    },
    [workspaceId],
  );
  // A diff cut at its size cap cannot tell a line past the cut, or a file,
  // from one that changed, so it leaves the comments' places as they were.
  const whole = Boolean(payload && !payload.truncated);
  const renamed = useMemo(() => {
    const shown = new Set(groups.map((group) => group.path));
    const moves = new Map<string, string>();
    for (const group of groups) {
      const previous = fileChangeOf(group).previousPath;
      if (previous && !shown.has(previous)) moves.set(previous, group.path);
    }
    return moves;
  }, [groups]);
  // Files with a new comment being written in them, whose views stay while
  // it is, even when the file leaves the diff.
  const [writingIn, setWritingIn] = useState<ReadonlySet<string>>(NO_PATHS);
  const onWriting = useCallback((path: string, writing: boolean) => {
    setWritingIn((current) => {
      if (current.has(path) === writing) return current;
      const next = new Set(current);
      if (writing) next.add(path);
      else next.delete(path);
      return next;
    });
  }, []);
  const review = useWorkspaceDiffReview({
    workspaceId: comments ? workspaceId : undefined,
    turnId,
    onDelete: onDeleteComment,
    onDismiss: onDismissFinding,
    relocate: whole,
    renamed,
    onWriting,
  });
  // Files the diff no longer shows that still have comments, or a comment
  // being written: the agent reverted them, say. They stay at the top with
  // their comments, outdated, instead of vanishing with the file.
  const goneKey = useMemo(() => {
    if (!review || !whole) return "";
    const shown = new Set(groups.map((group) => group.path));
    const left = new Set(
      [...review.paths, ...writingIn].filter(
        (path) => !shown.has(path) && (!file || path === file),
      ),
    );
    return JSON.stringify([...left].sort());
  }, [review, whole, groups, writingIn, file]);
  const gone = useMemo<DiffFileGroup[]>(
    () =>
      goneKey
        ? (JSON.parse(goneKey) as string[]).map((path) => ({
            path,
            lines: [],
          }))
        : [],
    [goneKey],
  );

  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const stepping = useRef(false);
  async function stepToFile(direction: 1 | -1) {
    if (!file || !onStepFile || !client.listCodeWorkspaceFiles) return;
    if (stepping.current) return;
    stepping.current = true;
    try {
      const listed = await client.listCodeWorkspaceFiles(workspaceId, turnId);
      const order = changedFileOrder(listed.files);
      const next = order[order.indexOf(file) + direction];
      if (next) onStepFile(next);
    } catch {
      // The list is a convenience here; the diff on screen stays as it is.
    } finally {
      stepping.current = false;
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    // A dialog this panel opened is portaled elsewhere in the page, but its
    // keys still bubble here through React.
    if (!event.currentTarget.contains(event.target as Node)) return;
    const action = diffFileKey(event.nativeEvent);
    if (!action) return;
    event.preventDefault();
    if (action === "toggle-whitespace") {
      setIgnoreWhitespace((current) => !current);
      return;
    }
    const direction = action === "next-file" ? 1 : -1;
    const scroller = scrollerRef.current;
    if (scroller && stepDiffFile(scroller, direction)) return;
    void stepToFile(direction);
  }

  const scopeCaption = file
    ? file
    : turnId
      ? (turnLabel ?? "This turn")
      : "Workspace vs base";

  const options = { layout, ignoreWhitespace, review, reverts };

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      onKeyDown={onKeyDown}
    >
      {dialog}
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
          <DiffViewOptions
            ignoreWhitespace={ignoreWhitespace}
            onIgnoreWhitespaceChange={setIgnoreWhitespace}
          />
          {reviewer && !file && (
            <ReviewChangesControl
              workspaceId={workspaceId}
              reviewer={reviewer}
              turn={
                turnId
                  ? { id: turnId, label: turnLabel ?? "This turn" }
                  : undefined
              }
              takesRequests
            />
          )}
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
      {reviewer && !file && (
        <DiffReviewStatus
          workspaceId={workspaceId}
          reviewer={reviewer}
          turnLabel={turnLabel}
        />
      )}
      {error && (
        <Notice
          tone="critical"
          docked="top"
          className="shrink-0"
          action={
            <NoticeRetryButton
              disabled={refreshing}
              onClick={() => void refresh()}
            />
          }
        >
          {error}
        </Notice>
      )}
      {payload?.truncated && (
        <p className="text-muted-foreground border-b px-3 py-2 text-xs">
          This diff was truncated. Open a single file for the rest.
        </p>
      )}
      <div
        ref={scrollerRef}
        className="min-h-0 flex-1 overflow-y-auto"
        tabIndex={0}
        role="region"
        aria-label="Diff"
      >
        {!payload && !error && (
          <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
            <Skeleton className="h-3 w-1/3" />
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        )}
        {payload && groups.length === 0 && !error && (
          <p className="text-muted-foreground px-3 py-6 text-sm">
            {emptyDiffText(file, turnId, turnLabel)}
          </p>
        )}
        {!file && review && review.general.length > 0 && (
          <WholeChangeComments
            workspaceId={workspaceId}
            comments={review.general}
            sending={review.sending}
            onDelete={onDeleteComment}
            onDismiss={onDismissFinding}
          />
        )}
        {/*
          A file that leaves the diff keeps its place in this list, under the
          same key, so an editor open on it keeps what was typed and is there
          again when the file comes back.
        */}
        {file && groups.length <= 1
          ? (groups[0] ?? gone[0]) && (
              <FileDiff
                group={groups[0] ?? gone[0]!}
                options={options}
                onShowWhitespace={() => setIgnoreWhitespace(false)}
              />
            )
          : [...gone, ...groups].map((group, index) => (
              <FileDiffSection
                key={group.path}
                group={group}
                gone={index < gone.length}
                onOpenFile={onOpenFile}
                options={options}
                onShowWhitespace={() => setIgnoreWhitespace(false)}
              />
            ))}
      </div>
    </div>
  );
}

/** What every file of one panel is drawn with. */
type FileDiffOptions = {
  layout: "unified" | "split";
  ignoreWhitespace: boolean;
  review: WorkspaceDiffReview | null;
  reverts: RevertTracker | null;
};

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
  return useMemo(
    () => (actions ? { actions, turnId, reverted, pending, run } : null),
    [actions, turnId, reverted, pending, run],
  );
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

/**
 * Comments on the changes as a whole, above the files: a reviewer's answer
 * that could not be read as findings, or its findings on lines the diff
 * does not show. They go with the next message like any comment once kept.
 */
function WholeChangeComments({
  workspaceId,
  comments,
  sending,
  onDelete,
  onDismiss,
}: {
  workspaceId: string;
  comments: readonly ReviewComment[];
  sending: ReadonlySet<string>;
  onDelete: (id: string) => void;
  onDismiss: (id: string) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  return (
    <section
      aria-label="Comments on the changes as a whole"
      className="border-border-subtle flex flex-col gap-1 border-b py-1"
      data-diff-whole-change=""
    >
      {comments.map((comment) =>
        editing === comment.id ? (
          <CommentComposer
            key={comment.id}
            label={wholeChangeLabel(comment)}
            initial={comment.body}
            submitLabel="Save"
            onSubmit={(body) => {
              usePendingReviewStore
                .getState()
                .edit(workspaceId, comment.id, body);
              setEditing(null);
            }}
            onCancel={() => setEditing(null)}
          />
        ) : (
          <CommentCard
            key={comment.id}
            comment={comment}
            label={wholeChangeLabel(comment)}
            sending={sending.has(comment.id)}
            onEdit={() => setEditing(comment.id)}
            onDelete={() =>
              comment.author.kind === "reviewer"
                ? onDismiss(comment.id)
                : onDelete(comment.id)
            }
            onKeep={() =>
              usePendingReviewStore.getState().keep(workspaceId, comment.id)
            }
          />
        ),
      )}
    </section>
  );
}

/** How a comment on no lines names where it sits. */
const WHOLE_CHANGE = "The changes as a whole";

/**
 * Where a comment that quotes no lines sits: the changes as a whole, or a
 * file's lines the diff does not show, such as "src/queue.ts, lines 90–92".
 */
function wholeChangeLabel(comment: ReviewComment): string {
  if (!comment.path) return WHOLE_CHANGE;
  const spans = spansOf(comment);
  return spans.lines || spans.oldLines
    ? `${comment.path}, ${commentLinesLabel(spans).toLowerCase()}`
    : comment.path;
}

function FileDiffSection({
  group,
  gone = false,
  onOpenFile,
  options,
  onShowWhitespace,
}: {
  group: DiffFileGroup;
  /**
   * The diff no longer shows this file. It stays, with no lines, for its
   * comments, which read as outdated, and for any comment being written.
   */
  gone?: boolean;
  onOpenFile?: (path: string) => void;
  options: FileDiffOptions;
  onShowWhitespace: () => void;
}) {
  const large = group.lines.length > DIFF_COLLAPSE_LINE_THRESHOLD;
  const [expanded, setExpanded] = useState(!large);
  const bodyId = useId();
  const { insertions, deletions } = fileDiffstat(group.lines);
  const reverts = gone ? null : options.reverts;

  if (gone) {
    return (
      <section
        className="border-b last:border-b-0"
        data-diff-file=""
        data-diff-file-gone=""
      >
        <header className="bg-background sticky top-0 z-10 flex items-center gap-1.5 pr-3">
          {/* Nothing to open, but J and K land here like on any file. */}
          <h3
            tabIndex={-1}
            data-diff-file-header=""
            className={cn(
              "flex min-w-0 flex-1 items-center gap-1.5 py-1.5 pl-3",
              FOCUS_RING_TIGHT,
            )}
          >
            {/* The chevron's room, so the path lines up with the others. */}
            <span className="size-3 shrink-0" aria-hidden="true" />
            <MiddleTruncate
              text={group.path}
              className="text-muted-foreground min-w-0 flex-1 font-mono text-xs"
            />
          </h3>
          <span className="text-muted-foreground shrink-0 text-xs">
            No longer in this diff
          </span>
        </header>
        <FileDiff
          group={group}
          id={bodyId}
          options={options}
          onShowWhitespace={onShowWhitespace}
        />
      </section>
    );
  }

  return (
    <section className="border-b last:border-b-0" data-diff-file="">
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
            data-diff-file-header=""
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
        <FileDiff
          group={group}
          id={bodyId}
          options={options}
          onShowWhitespace={onShowWhitespace}
        />
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

/** One file's body: the shared view, with this panel's reverts and comments. */
function FileDiff({
  group,
  id,
  options,
  onShowWhitespace,
}: {
  group: DiffFileGroup;
  id?: string;
  options: FileDiffOptions;
  onShowWhitespace: () => void;
}) {
  const { reverts } = options;
  // Only a hunk shown whole can be reverted as shown; the last hunk of a
  // diff cut at its size cap is not.
  const hunks = useMemo(
    () =>
      reverts
        ? new Map(
            diffHunks(group)
              .filter((hunk) => hunk.complete)
              .map((hunk) => [hunk.index, hunk]),
          )
        : null,
    [group, reverts],
  );
  const fileReverted =
    reverts?.turnId !== undefined &&
    reverts.reverted.has(revertKey(group.path));
  const hunkAction = useCallback<HunkAction>(
    (index, { hiddenWhitespace }) => {
      const hunk = hunks?.get(index);
      if (!hunk || !reverts) return null;
      return (
        <HunkRevert
          group={group}
          hunk={hunk}
          reverts={reverts}
          fileReverted={fileReverted}
          hiddenWhitespace={hiddenWhitespace}
        />
      );
    },
    [hunks, reverts, group, fileReverted],
  );
  return (
    <DiffView
      group={group}
      id={id}
      layout={options.layout}
      ignoreWhitespace={options.ignoreWhitespace}
      hunkAction={reverts ? hunkAction : undefined}
      review={options.review?.forPath(group.path)}
      onShowWhitespace={onShowWhitespace}
    />
  );
}

function HunkRevert({
  group,
  hunk,
  reverts,
  fileReverted,
  hiddenWhitespace,
}: {
  group: DiffFileGroup;
  hunk: DiffHunk;
  reverts: RevertTracker;
  fileReverted: boolean;
  /** Lines of the hunk the view hides because only their whitespace changed. */
  hiddenWhitespace: number;
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
          // The revert sends the hunk's exact text, hidden lines and all, so
          // the question names what hiding whitespace kept out of sight.
          reverts.actions.onRevertHunk(
            {
              path,
              turnId: reverts.turnId,
              ...fileChangeOf(group),
              ...(hiddenWhitespace > 0
                ? { hiddenWhitespaceLines: hiddenWhitespace }
                : {}),
            },
            hunk,
          ),
        )
      }
    />
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
