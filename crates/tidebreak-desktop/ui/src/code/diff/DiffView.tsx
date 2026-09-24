import {
  createContext,
  memo,
  startTransition,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from "react";

import { cn } from "@/lib/utils";
import { STATUS_TEXT } from "../statusTone";
import type { DiffFileGroup } from "../unifiedDiff";
import {
  anchorRows,
  indexRows,
  placeComment,
  type CommentAnchor,
  type CommentPlacement,
} from "./commentAnchor";
import { CommentCard, CommentComposer } from "./DiffComments";
import {
  buildDiffFileModel,
  diffRows,
  isCodeRow,
  rowAnchor,
  rowSide,
  type DiffFileModel,
  type DiffRow,
  type RowAnchor,
  type RowSide,
  type TextRange,
} from "./diffModel";
import { SPLIT_MIN_WIDTH, type DiffLayout } from "./diffPreferences";
import type { CommentRelocation } from "./pendingReview";
import {
  commentLinesLabel,
  spansOf,
  type CommentLineSpans,
  type ReviewComment,
} from "./reviewComments";
import {
  FileSyntax,
  useLanguageReady,
  type SyntaxLine,
  type SyntaxRole,
} from "./syntaxHighlight";

/**
 * One file's diff, the way both the workspace diff and a pull request's
 * diff draw it: line-number gutters, syntax color under the add and delete
 * tints, word emphasis inside changed lines, unified or side by side, and,
 * where the diff takes them, line comments.
 *
 * A long diff mounts a chunk of rows per frame rather than all at once, and
 * each chunk colors its own hunks, so opening 5,000 lines never holds the
 * main thread for one long task. `content-visibility` lets the webview skip
 * laying out chunks that are off screen.
 */

/** Rows per chunk: a screenful several times over, cheap to render in a frame. */
export const DIFF_CHUNK_ROWS = 60;
/** Chunks mounted on the first render, before the rest arrive frame by frame. */
const FIRST_CHUNKS = 1;
/**
 * One chunk a frame keeps each frame's work to a chunk's worth of rows,
 * well under a frame on a slow machine; `content-visibility` spares the
 * layout of chunks mounted off screen.
 */
const CHUNKS_PER_FRAME = 1;
/** A hunk this small is colored during render, so short diffs never flash plain. */
const SYNC_HIGHLIGHT_LINES = 400;

/** A comment as the view writes it: its lines, and what finds them again. */
export type NewDiffComment = Pick<
  ReviewComment,
  "lines" | "unquoted" | "span" | "context" | "outdated"
>;

export type DiffReview = {
  /** Pending comments on this file, in the diff this view shows. */
  comments: readonly ReviewComment[];
  /** Which of them are riding a send right now. */
  sending: ReadonlySet<string>;
  onAdd: (comment: NewDiffComment, body: string) => void;
  onEdit: (id: string, body: string) => void;
  onDelete: (id: string) => void;
  /**
   * Record where a comment's lines are now, or that they changed, so the
   * message that carries it names the lines as they are. Absent where the
   * diff shown is not the whole diff, which cannot tell a missing line from
   * a changed one.
   */
  onRelocate?: (id: string, change: CommentRelocation) => void;
};

/**
 * A control for one hunk's header row, such as Revert, or nothing. It is
 * told how many of the hunk's changed lines the view is hiding because only
 * their whitespace changed, since acting on the hunk acts on those too.
 */
export type HunkAction = (
  index: number,
  hunk: { hiddenWhitespace: number },
) => ReactNode;

export type DiffViewProps = {
  group: DiffFileGroup;
  layout: DiffLayout;
  ignoreWhitespace: boolean;
  id?: string;
  /** A control for hunk `index`'s header row, such as Revert, or nothing. */
  hunkAction?: HunkAction;
  /** Line comments. Absent, the gutters are plain numbers. */
  review?: DiffReview;
  /** Offered when hiding whitespace left nothing to show. */
  onShowWhitespace?: () => void;
};

const ROLE_CLASS: Record<SyntaxRole, string> = {
  keyword: "text-syntax-keyword",
  string: "text-syntax-string",
  comment: "text-syntax-comment",
  number: "text-syntax-number",
  title: "text-syntax-title",
  attr: "text-syntax-attr",
};

type Column = "unified" | "left" | "right";

/** A line picked for a comment, from where the mouse went down to where it is. */
type Selection = { anchor: RowAnchor; head: RowAnchor };

/**
 * The comment editor: a new comment on the lines it was opened on, or an
 * existing one rewritten. A new comment holds on to its lines' code, like a
 * saved one, so a refresh while it is open moves it with them.
 */
type Editor =
  | {
      kind: "new";
      id: string;
      anchor: CommentAnchor & { span: CommentLineSpans };
    }
  | { kind: "edit"; id: string };

/** Where an open new-comment editor sits, for the chunk that draws it. */
type EditorSlot = {
  id: string;
  display: number;
  span: CommentLineSpans;
};

type Interaction = {
  review: DiffReview | null;
  openEditor: (range: { start: number; end: number }) => void;
  closeEditor: () => void;
  submitNew: (body: string) => void;
  submitEdit: (id: string, body: string) => void;
  startEdit: (id: string) => void;
  /** What was typed into an editor, kept across the remounts a refresh causes. */
  draft: (key: string) => string | undefined;
  keepDraft: (key: string, text: string) => void;
};

/** A saved comment under its lines, with the span it covers there now. */
type PlacedComment = { comment: ReviewComment; span: CommentLineSpans };

/**
 * Where a comment sits in this view. Its lines can be under it, hidden
 * because only their whitespace changed and whitespace is hidden, or gone.
 * A hidden comment is not outdated: showing whitespace brings its lines back.
 */
type ViewPlacement =
  | Extract<CommentPlacement, { kind: "placed" }>
  | (Omit<Extract<CommentPlacement, { kind: "placed" }>, "kind"> & {
      /** `start` and `end` are rows of the diff with whitespace shown. */
      kind: "hidden";
    })
  | { kind: "outdated" };

let editorCount = 0;

const InteractionContext = createContext<Interaction | null>(null);

function anchorKey(anchor: RowAnchor): string {
  return `${anchor.side}:${anchor.line}`;
}

function useElementWidth(ref: React.RefObject<HTMLElement | null>) {
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const observer = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next !== undefined) setWidth(Math.round(next));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

/** How many of `total` chunks are mounted, growing a little every frame. */
function useProgressiveCount(total: number, resetKey: unknown): number {
  const [state, setState] = useState({ key: resetKey, count: FIRST_CHUNKS });
  const count = state.key === resetKey ? state.count : FIRST_CHUNKS;
  useEffect(() => {
    if (count >= total) return;
    // A transition, so React renders the next chunk in slices that yield
    // to input and paint instead of in one task.
    const frame = window.requestAnimationFrame(() =>
      startTransition(() =>
        setState({ key: resetKey, count: count + CHUNKS_PER_FRAME }),
      ),
    );
    return () => window.cancelAnimationFrame(frame);
  }, [count, total, resetKey]);
  return Math.min(count, total);
}

function scheduleIdle(callback: () => void): () => void {
  if (typeof window.requestIdleCallback === "function") {
    const handle = window.requestIdleCallback(callback, { timeout: 250 });
    return () => window.cancelIdleCallback(handle);
  }
  const handle = window.setTimeout(callback, 16);
  return () => window.clearTimeout(handle);
}

/**
 * Color the hunks a chunk shows, each computed once for the whole view.
 *
 * A chunk that mounts with its grammar already loaded colors its small hunks
 * during that first render, so a short diff never flashes plain. Anything
 * else waits for an idle moment, one chunk at a time: a large hunk, and
 * every chunk that was already on screen when its grammar arrived, which
 * would otherwise all highlight in the one render the arrival causes.
 *
 * Returns a number that changes only when more of the chunk's syntax is
 * known, so the memoized lines under it redraw then and only then.
 */
function useChunkSyntax(
  syntax: FileSyntax | null,
  hunks: readonly number[],
  ready: boolean,
): number {
  const [version, setVersion] = useState(0);
  const [colorsOnMount] = useState(ready);
  const mounted = useRef(false);
  if (syntax && ready) {
    for (const hunk of hunks) {
      // A hunk highlighted lately comes back at no cost, on any render.
      if (syntax.recall(hunk)) continue;
      if (
        colorsOnMount &&
        !mounted.current &&
        syntax.size(hunk) <= SYNC_HIGHLIGHT_LINES
      ) {
        syntax.compute(hunk);
      }
    }
  }
  useEffect(() => {
    mounted.current = true;
  }, []);
  const missing =
    syntax && ready ? hunks.filter((hunk) => !syntax.has(hunk)) : [];
  const missingKey = missing.join(",");
  useEffect(() => {
    if (!syntax || !ready || missingKey === "") return;
    return scheduleIdle(() => {
      for (const hunk of missingKey.split(",")) syntax.compute(Number(hunk));
      // Many chunks can finish in one idle period; as a transition their
      // redraws render in slices rather than as one long task.
      startTransition(() => setVersion((current) => current + 1));
    });
  }, [syntax, ready, missingKey]);
  return version;
}

export function DiffView({
  group,
  layout: preferredLayout,
  ignoreWhitespace,
  id,
  hunkAction,
  review,
  onShowWhitespace,
}: DiffViewProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const width = useElementWidth(rootRef);
  const layout: DiffLayout =
    preferredLayout === "split" && (width === null || width >= SPLIT_MIN_WIDTH)
      ? "split"
      : "unified";
  const model = useMemo(
    () => buildDiffFileModel(group, { ignoreWhitespace }),
    [group, ignoreWhitespace],
  );
  const syntax = useMemo(() => FileSyntax.for(group), [group]);
  const languageReady = useLanguageReady(syntax?.language ?? null);

  /** Row index by the line it stands for, so anchors outlive a refresh. */
  const rowByAnchor = useMemo(() => {
    const map = new Map<string, number>();
    model.rows.forEach((row, index) => {
      const anchor = rowAnchor(row);
      if (anchor) map.set(anchorKey(anchor), index);
    });
    return map;
  }, [model]);
  const resolve = useCallback(
    (anchor: RowAnchor) => rowByAnchor.get(anchorKey(anchor)) ?? null,
    [rowByAnchor],
  );

  const [selection, setSelectionState] = useState<Selection | null>(null);
  const selectionRef = useRef<Selection | null>(null);
  const setSelection = useCallback(
    (
      next:
        | Selection
        | null
        | ((current: Selection | null) => Selection | null),
    ) => {
      const value =
        typeof next === "function" ? next(selectionRef.current) : next;
      selectionRef.current = value;
      setSelectionState(value);
    },
    [],
  );
  const [editor, setEditor] = useState<Editor | null>(null);
  const [active, setActive] = useState<{ row: number; column: Column } | null>(
    null,
  );
  const dragAnchor = useRef<RowAnchor | null>(null);
  /** Text typed into editors, by editor, so a remount never loses it. */
  const drafts = useRef(new Map<string, string>());

  const rowIndex = useMemo(() => indexRows(model.rows), [model.rows]);
  // With whitespace hidden, the rows it leaves out, to tell a comment whose
  // lines are only hidden from one whose lines are gone.
  const shownRows = useMemo(
    () => (ignoreWhitespace ? diffRows(group) : null),
    [group, ignoreWhitespace],
  );
  const shownIndex = useMemo(
    () => (shownRows ? indexRows(shownRows) : null),
    [shownRows],
  );
  const locate = useCallback(
    (anchor: CommentAnchor): ViewPlacement => {
      const here = placeComment(model.rows, anchor, rowIndex);
      if (here.kind === "placed" || !shownRows || !shownIndex) return here;
      const shown = placeComment(shownRows, anchor, shownIndex);
      return shown.kind === "placed" ? { ...shown, kind: "hidden" } : here;
    },
    [model.rows, rowIndex, shownRows, shownIndex],
  );
  const editorPlacement = useMemo<ViewPlacement | null>(
    () => (editor?.kind === "new" ? locate(editor.anchor) : null),
    [editor, locate],
  );

  const selected = useMemo(() => {
    if (selection) {
      const anchor = resolve(selection.anchor);
      const head = resolve(selection.head);
      if (anchor === null || head === null) return null;
      return clampToHunk(model.rows, anchor, head);
    }
    // An open editor marks the lines it is about, wherever they are now.
    return editorPlacement?.kind === "placed"
      ? { start: editorPlacement.start, end: editorPlacement.end }
      : null;
  }, [selection, resolve, model.rows, editorPlacement]);

  // Which display row each comment, and the editor, sits under.
  const displayOf = useMemo(() => {
    if (layout === "unified") return (row: number) => row;
    const map = new Map<number, number>();
    model.split.forEach((entry, index) => {
      if (entry.kind === "full") map.set(entry.row, index);
      else {
        if (entry.left !== null) map.set(entry.left, index);
        if (entry.right !== null) map.set(entry.right, index);
      }
    });
    return (row: number) => map.get(row) ?? 0;
  }, [layout, model.split]);

  // Each comment finds its lines by their code, so a line the agent added
  // above one never slides it onto the wrong line.
  const placements = useMemo(() => {
    const found = new Map<string, ViewPlacement>();
    for (const comment of review?.comments ?? []) {
      found.set(comment.id, locate(comment));
    }
    return found;
  }, [review?.comments, locate]);

  const commentPlacement = useMemo(() => {
    const at = new Map<number, PlacedComment[]>();
    const hidden: PlacedComment[] = [];
    const outdated: ReviewComment[] = [];
    for (const comment of review?.comments ?? []) {
      const placement = placements.get(comment.id);
      if (!placement || placement.kind === "outdated") {
        outdated.push(comment);
        continue;
      }
      if (placement.kind === "hidden") {
        hidden.push({ comment, span: placement.span });
        continue;
      }
      const display = displayOf(placement.end);
      at.set(display, [
        ...(at.get(display) ?? []),
        { comment, span: placement.span },
      ]);
    }
    return { at, hidden, outdated };
  }, [review?.comments, placements, displayOf]);

  // Tell the review where each comment's lines are now, so the message that
  // carries it names them as they are, or says they changed. Each view says
  // so once per answer: two views that briefly disagree, one showing a diff
  // a refresh has not reached yet, must not overwrite each other in a loop.
  const onRelocate = review?.onRelocate;
  const reviewComments = review?.comments;
  const reported = useRef(new Map<string, string>());
  useEffect(() => {
    if (!onRelocate || !reviewComments) return;
    for (const comment of reviewComments) {
      const placement = placements.get(comment.id);
      if (!placement) continue;
      const answer =
        placement.kind === "outdated"
          ? "outdated"
          : `${placement.lines.map((line) => `${line.oldNo}/${line.newNo}`).join(",")}|${placement.span.lines}/${placement.span.oldLines}`;
      if (reported.current.get(comment.id) === answer) continue;
      reported.current.set(comment.id, answer);
      if (placement.kind === "outdated") {
        if (!comment.outdated) onRelocate(comment.id, { outdated: true });
        continue;
      }
      const moved = placement.lines.some(
        (line, index) =>
          line.oldNo !== comment.lines[index]?.oldNo ||
          line.newNo !== comment.lines[index]?.newNo,
      );
      const spanMoved =
        comment.unquoted !== undefined &&
        (comment.span?.lines !== placement.span.lines ||
          comment.span?.oldLines !== placement.span.oldLines);
      if (moved || spanMoved || comment.outdated) {
        onRelocate(comment.id, {
          lines: placement.lines,
          ...(comment.unquoted !== undefined ? { span: placement.span } : {}),
          outdated: false,
        });
      }
    }
  }, [placements, onRelocate, reviewComments]);

  const editorSlot = useMemo<EditorSlot | null>(() => {
    if (!editor || editor.kind !== "new") return null;
    if (editorPlacement?.kind !== "placed") return null;
    return {
      id: editor.id,
      display: displayOf(editorPlacement.end),
      span: editorPlacement.span,
    };
  }, [editor, editorPlacement, displayOf]);
  // The lines an open editor was about changed under it, or hiding
  // whitespace left them out: it moves to the top with the lines as they
  // were, and keeps what was typed.
  const editorAway =
    editor?.kind === "new" &&
    (editorPlacement?.kind === "outdated" || editorPlacement?.kind === "hidden")
      ? editorPlacement.kind
      : null;

  const reviewRef = useRef(review);
  reviewRef.current = review;
  const rowsRef = useRef(model.rows);
  rowsRef.current = model.rows;
  const shownRowsRef = useRef(shownRows);
  shownRowsRef.current = shownRows;
  const locateRef = useRef(locate);
  locateRef.current = locate;
  const editorRef = useRef(editor);
  editorRef.current = editor;
  const editorPlacementRef = useRef(editorPlacement);
  editorPlacementRef.current = editorPlacement;
  const rowByAnchorRef = useRef(rowByAnchor);
  rowByAnchorRef.current = rowByAnchor;

  /** Put focus, and the tab stop, back on a line once the editor closes. */
  const focusRow = useCallback((row: number) => {
    window.requestAnimationFrame(() => {
      const target = rootRef.current?.querySelector<HTMLElement>(
        `[data-diff-target][data-row="${row}"]`,
      );
      if (!target) return;
      setActive({ row, column: target.dataset.column as Column });
      target.focus({ preventScroll: true });
    });
  }, []);

  const interaction = useMemo<Interaction>(
    () => ({
      get review() {
        return reviewRef.current ?? null;
      },
      openEditor: ({ start, end }) => {
        const rows = rowsRef.current;
        if (!isCodeRow(rows[start]!) || !isCodeRow(rows[end]!)) return;
        editorCount += 1;
        setSelection(null);
        setEditor({
          kind: "new",
          id: `editor-${editorCount}`,
          anchor: anchorRows(rows, start, end),
        });
      },
      closeEditor: () => {
        const current = editorRef.current;
        const placement = editorPlacementRef.current;
        setEditor(null);
        setSelection(null);
        if (!current) return;
        drafts.current.delete(
          current.kind === "new" ? current.id : `edit:${current.id}`,
        );
        if (current.kind === "new" && placement?.kind === "placed") {
          focusRow(placement.end);
        }
      },
      submitNew: (body) => {
        const current = editorRef.current;
        const target = reviewRef.current;
        if (!current || current.kind !== "new" || !target) return;
        // Where the lines are at the moment of saving, not where they were
        // when the editor opened.
        const placement = locateRef.current(current.anchor);
        const rows =
          placement.kind === "hidden"
            ? (shownRowsRef.current ?? rowsRef.current)
            : rowsRef.current;
        if (placement.kind !== "outdated") {
          const { span, ...anchor } = anchorRows(
            rows,
            placement.start,
            placement.end,
          );
          target.onAdd(
            { ...anchor, ...(anchor.unquoted ? { span } : {}) },
            body,
          );
          if (placement.kind === "placed") focusRow(placement.end);
        } else {
          const { span, ...anchor } = current.anchor;
          target.onAdd(
            {
              ...anchor,
              ...(anchor.unquoted ? { span } : {}),
              outdated: true,
            },
            body,
          );
        }
        drafts.current.delete(current.id);
        setEditor(null);
        setSelection(null);
      },
      submitEdit: (commentId, body) => {
        reviewRef.current?.onEdit(commentId, body);
        drafts.current.delete(`edit:${commentId}`);
        setEditor(null);
      },
      startEdit: (commentId) => setEditor({ kind: "edit", id: commentId }),
      draft: (key) => drafts.current.get(key),
      keepDraft: (key, text) => {
        drafts.current.set(key, text);
      },
    }),
    [focusRow, setSelection],
  );

  // --- Pointer: press a line number, drag across others, let go. ---------

  const targetRow = (element: EventTarget | null): number | null => {
    if (!(element instanceof Element)) return null;
    const target = element.closest<HTMLElement>("[data-diff-target]");
    if (!target || !rootRef.current?.contains(target)) return null;
    const row = Number(target.dataset.row);
    return Number.isInteger(row) ? row : null;
  };

  function onPointerDown(event: PointerEvent<HTMLDivElement>) {
    if (!review || event.button !== 0) return;
    const row = targetRow(event.target);
    if (row === null) return;
    const anchor = rowAnchor(model.rows[row]!);
    if (!anchor) return;
    // Keep the press from starting a text selection across the rows; the
    // pressed line takes focus by hand instead.
    event.preventDefault();
    (event.target as Element)
      .closest<HTMLElement>("[data-diff-target]")
      ?.focus({ preventScroll: true });
    const start =
      event.shiftKey && selectionRef.current
        ? selectionRef.current.anchor
        : anchor;
    dragAnchor.current = start;
    setSelection({ anchor: start, head: anchor });
    setActive({ row, column: columnOf(event.target) });
    const finish = () => {
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      dragAnchor.current = null;
      const current = selectionRef.current;
      if (!current) return;
      const from = rowByAnchorRef.current.get(anchorKey(current.anchor));
      const to = rowByAnchorRef.current.get(anchorKey(current.head));
      if (from !== undefined && to !== undefined) {
        interaction.openEditor(clampToHunk(rowsRef.current, from, to));
      }
    };
    const cancel = () => {
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      dragAnchor.current = null;
    };
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", cancel);
  }

  function onPointerOver(event: PointerEvent<HTMLDivElement>) {
    const anchor = dragAnchor.current;
    if (!anchor || !(event.target instanceof Element)) return;
    const line = event.target.closest<HTMLElement>("[data-row]");
    const row = line ? Number(line.dataset.row) : Number.NaN;
    if (!Number.isInteger(row)) return;
    const head = rowAnchor(model.rows[row]!);
    if (!head) return;
    setSelection((current) =>
      current && anchorKey(current.head) === anchorKey(head)
        ? current
        : { anchor, head },
    );
  }

  // --- Keyboard: walk the line numbers, extend with Shift, Enter to comment.

  function targetsIn(column: Column): HTMLElement[] {
    return [
      ...(rootRef.current?.querySelectorAll<HTMLElement>(
        `[data-diff-target][data-column="${column}"]`,
      ) ?? []),
    ];
  }

  function moveTo(target: HTMLElement | undefined, extend: boolean) {
    if (!target) return;
    const row = Number(target.dataset.row);
    const column = target.dataset.column as Column;
    target.focus();
    target.scrollIntoView?.({ block: "nearest" });
    setActive({ row, column });
    const anchor = rowAnchor(model.rows[row]!);
    if (!anchor) return;
    if (extend) {
      setSelection((current) => ({
        anchor: current?.anchor ?? anchor,
        head: anchor,
      }));
    } else {
      setSelection(null);
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!review) return;
    const element = event.target as HTMLElement;
    if (!element.matches?.("[data-diff-target]")) return;
    const row = Number(element.dataset.row);
    const column = element.dataset.column as Column;
    const targets = targetsIn(column);
    const index = targets.indexOf(element);
    const plain = !event.metaKey && !event.ctrlKey && !event.altKey;
    // Shift extends from the line that had focus when nothing was picked yet.
    const anchorHere = () => {
      if (!event.shiftKey || selectionRef.current) return;
      const anchor = rowAnchor(model.rows[row]!);
      if (anchor) setSelection({ anchor, head: anchor });
    };
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        if (!plain) return;
        event.preventDefault();
        anchorHere();
        moveTo(
          targets[index + (event.key === "ArrowDown" ? 1 : -1)],
          event.shiftKey,
        );
        return;
      }
      case "Home":
      case "End": {
        if (!plain) return;
        event.preventDefault();
        anchorHere();
        moveTo(
          event.key === "Home" ? targets[0] : targets.at(-1),
          event.shiftKey,
        );
        return;
      }
      case "ArrowLeft":
      case "ArrowRight": {
        if (column === "unified" || !plain || event.shiftKey) return;
        event.preventDefault();
        const other = targetsIn(event.key === "ArrowLeft" ? "left" : "right");
        const here = element.closest<HTMLElement>("[data-display]");
        const display = Number(here?.dataset.display);
        const same = other.find(
          (candidate) =>
            Number(
              candidate.closest<HTMLElement>("[data-display]")?.dataset.display,
            ) >= display,
        );
        moveTo(same ?? other.at(-1), false);
        return;
      }
      case "Enter":
      case " ": {
        if (!plain || event.shiftKey) return;
        event.preventDefault();
        const range =
          selected && row >= selected.start && row <= selected.end
            ? selected
            : { start: row, end: row };
        interaction.openEditor(range);
        return;
      }
      case "Escape": {
        if (!selectionRef.current) return;
        event.preventDefault();
        setSelection(null);
        return;
      }
      default:
    }
  }

  const displayCount =
    layout === "split" ? model.split.length : model.rows.length;
  const chunkCount = Math.ceil(displayCount / DIFF_CHUNK_ROWS);
  // Keyed on the file, not its content: a refresh while an agent works keeps
  // every row it had instead of mounting the diff again from the top.
  const mounted = useProgressiveCount(chunkCount, group.path);

  // The roving tab stop: the line last used, or the first line.
  const firstTarget = useMemo(
    () => model.rows.findIndex(isCodeRow),
    [model.rows],
  );
  const stopRow =
    active &&
    active.row < model.rows.length &&
    isCodeRow(model.rows[active.row]!)
      ? active.row
      : firstTarget;
  const stopColumnNow =
    stopRow >= 0
      ? stopColumn(
          layout,
          model.rows[stopRow]!,
          active?.row === stopRow ? active.column : null,
        )
      : null;
  // One object for as long as the stop stays put, so the chunk holding it
  // is not redrawn on every frame of a long diff's mounting.
  const tabStop = useMemo(
    () =>
      stopRow >= 0 && stopColumnNow
        ? { row: stopRow, column: stopColumnNow }
        : null,
    [stopRow, stopColumnNow],
  );

  const style = {
    "--diff-number": `calc(${model.gutterDigits}ch + 0.75rem)`,
    ...(width !== null ? { "--diff-viewport": `${width}px` } : {}),
  } as CSSProperties;

  return (
    <InteractionContext.Provider value={interaction}>
      <div
        ref={rootRef}
        id={id}
        data-diff-view={layout}
        // Without line comments nothing inside takes focus, so the region
        // itself does: its long lines scroll sideways from the keyboard too.
        {...(review
          ? {}
          : {
              role: "region",
              "aria-label": `Changes to ${group.path}`,
              tabIndex: 0,
            })}
        className={cn(
          "font-mono text-md leading-5",
          layout === "unified" && "overflow-x-auto",
        )}
        style={style}
        onPointerDown={onPointerDown}
        onPointerOver={onPointerOver}
        onKeyDown={onKeyDown}
      >
        {model.binary && (
          <p className="text-muted-foreground px-3 py-2 font-sans text-xs">
            Binary file not shown.
          </p>
        )}
        {model.onlyWhitespace && (
          <p className="text-muted-foreground flex flex-wrap items-center gap-x-2 px-3 py-2 font-sans text-xs">
            Only whitespace changed in this file.
            {onShowWhitespace && (
              <button
                type="button"
                className="text-foreground cursor-pointer rounded-sm underline underline-offset-2 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                onClick={onShowWhitespace}
              >
                Show whitespace changes
              </button>
            )}
          </p>
        )}
        {(commentPlacement.hidden.length > 0 || editorAway === "hidden") && (
          <div
            className="border-border-subtle border-b py-1 font-sans"
            data-diff-hidden-comments=""
          >
            <p className="text-muted-foreground px-3 pt-1 text-xs">
              Hiding whitespace leaves out the lines these comments are on.
            </p>
            {editorAway === "hidden" && editor?.kind === "new" && (
              <NewCommentSlot
                key={`editor:${editor.id}`}
                editorId={editor.id}
                span={editor.anchor.span}
                awayQuote={editor.anchor.lines}
              />
            )}
            {commentPlacement.hidden.map(({ comment, span }) => (
              <CommentSlot
                key={comment.id}
                comment={comment}
                span={span}
                editing={editor?.kind === "edit" && editor.id === comment.id}
                sending={review?.sending.has(comment.id) ?? false}
                away="hidden"
              />
            ))}
          </div>
        )}
        {(commentPlacement.outdated.length > 0 ||
          editorAway === "outdated") && (
          <div
            className="border-border-subtle border-b py-1 font-sans"
            data-diff-outdated=""
          >
            <p className="text-muted-foreground px-3 pt-1 text-xs">
              The code these comments quote has changed since they were written.
            </p>
            {editorAway === "outdated" && editor?.kind === "new" && (
              <NewCommentSlot
                key={`editor:${editor.id}`}
                editorId={editor.id}
                span={editor.anchor.span}
                awayQuote={editor.anchor.lines}
                note="These lines changed while you wrote. The comment keeps them as they were."
              />
            )}
            {commentPlacement.outdated.map((comment) => (
              <CommentSlot
                key={comment.id}
                comment={comment}
                span={spansOf(comment)}
                editing={editor?.kind === "edit" && editor.id === comment.id}
                sending={review?.sending.has(comment.id) ?? false}
                away="outdated"
              />
            ))}
          </div>
        )}
        <div className={cn(layout === "unified" && "w-max min-w-full", "py-1")}>
          {Array.from({ length: mounted }, (_, chunk) => {
            const start = chunk * DIFF_CHUNK_ROWS;
            const end = Math.min(displayCount, start + DIFF_CHUNK_ROWS);
            return (
              <DiffChunk
                key={chunk}
                model={model}
                layout={layout}
                start={start}
                end={end}
                syntax={syntax}
                syntaxReady={languageReady}
                hunkAction={hunkAction}
                commentable={review !== undefined}
                selected={intersect(selected, model, layout, start, end)}
                tabStop={
                  tabStop && inChunk(displayOf(tabStop.row), start, end)
                    ? tabStop
                    : null
                }
                comments={commentsIn(commentPlacement.at, start, end)}
                editor={
                  editorSlot && inChunk(editorSlot.display, start, end)
                    ? editorSlot
                    : null
                }
                editingId={editor?.kind === "edit" ? editor.id : null}
                sending={review?.sending}
              />
            );
          })}
        </div>
      </div>
    </InteractionContext.Provider>
  );
}

/**
 * The column a line's tab stop sits in. A layout switch keeps the line and
 * moves the stop to the column that layout draws it in.
 */
function stopColumn(
  layout: DiffLayout,
  row: DiffRow,
  previous: Column | null,
): Column {
  if (layout === "unified") return "unified";
  if (row.kind === "del") return "left";
  if (row.kind === "add") return "right";
  return previous === "left" ? "left" : "right";
}

function columnOf(target: EventTarget | null): Column {
  const element =
    target instanceof Element
      ? target.closest<HTMLElement>("[data-diff-target]")
      : null;
  return (element?.dataset.column as Column | undefined) ?? "unified";
}

function inChunk(display: number, start: number, end: number): boolean {
  return display >= start && display < end;
}

/**
 * A comment covers lines of one hunk: a range dragged past the hunk's edge
 * stops at the edge, because the lines beyond it are not next to these.
 */
function clampToHunk(
  rows: readonly DiffRow[],
  anchor: number,
  head: number,
): { start: number; end: number } {
  const hunk = rows[anchor]?.hunk;
  let to = head;
  const step = head > anchor ? -1 : 1;
  while (to !== anchor && (rows[to]?.hunk !== hunk || !isCodeRow(rows[to]!))) {
    to += step;
  }
  return { start: Math.min(anchor, to), end: Math.max(anchor, to) };
}

/** The selected rows a chunk shows, or null so an untouched chunk stays memoized. */
function intersect(
  selected: { start: number; end: number } | null,
  model: DiffFileModel,
  layout: DiffLayout,
  start: number,
  end: number,
): { start: number; end: number } | null {
  if (!selected) return null;
  if (layout === "unified") {
    return selected.end < start || selected.start >= end ? null : selected;
  }
  for (let index = start; index < end; index += 1) {
    const entry = model.split[index]!;
    const rows =
      entry.kind === "full" ? [entry.row] : [entry.left, entry.right];
    if (
      rows.some(
        (row) => row !== null && row >= selected.start && row <= selected.end,
      )
    ) {
      return selected;
    }
  }
  return null;
}

const NO_COMMENTS: ReadonlyMap<number, readonly PlacedComment[]> = new Map();

function commentsIn(
  at: ReadonlyMap<number, readonly PlacedComment[]>,
  start: number,
  end: number,
): ReadonlyMap<number, readonly PlacedComment[]> {
  let found: Map<number, readonly PlacedComment[]> | null = null;
  for (const [display, comments] of at) {
    if (display < start || display >= end) continue;
    found ??= new Map();
    found.set(display, comments);
  }
  return found ?? NO_COMMENTS;
}

type ChunkProps = {
  model: DiffFileModel;
  layout: DiffLayout;
  start: number;
  end: number;
  syntax: FileSyntax | null;
  syntaxReady: boolean;
  hunkAction?: HunkAction;
  commentable: boolean;
  selected: { start: number; end: number } | null;
  tabStop: { row: number; column: Column } | null;
  comments: ReadonlyMap<number, readonly PlacedComment[]>;
  editor: EditorSlot | null;
  editingId: string | null;
  sending: ReadonlySet<string> | undefined;
};

/**
 * A run of display rows. Memoized on its slice of the state, so picking a
 * line re-renders the chunk holding it and none of the others.
 */
const DiffChunk = memo(function DiffChunk({
  model,
  layout,
  start,
  end,
  syntax,
  syntaxReady,
  hunkAction,
  commentable,
  selected,
  tabStop,
  comments,
  editor,
  editingId,
  sending,
}: ChunkProps) {
  const hunks = useMemo(() => {
    const seen = new Set<number>();
    for (let display = start; display < end; display += 1) {
      for (const row of displayRows(model, layout, display)) {
        const hunk = model.rows[row]!.hunk;
        if (hunk >= 0) seen.add(hunk);
      }
    }
    return [...seen];
  }, [model, layout, start, end]);
  const syntaxVersion = useChunkSyntax(syntax, hunks, syntaxReady);

  const rows: ReactNode[] = [];
  for (let display = start; display < end; display += 1) {
    // Each line gets the selection and the tab stop only when they touch
    // it, so moving either redraws the lines involved and no others.
    const lineRows = displayRows(model, layout, display);
    const touches = (range: { start: number; end: number } | null) =>
      range !== null &&
      lineRows.some((row) => row >= range.start && row <= range.end);
    const lineSelected = touches(selected) ? selected : null;
    const lineStop = tabStop && lineRows.includes(tabStop.row) ? tabStop : null;
    rows.push(
      layout === "split" ? (
        <SplitLine
          key={`s${display}`}
          model={model}
          display={display}
          syntax={syntax}
          syntaxVersion={syntaxVersion}
          hunkAction={hunkAction}
          commentable={commentable}
          selected={lineSelected}
          tabStop={lineStop}
        />
      ) : (
        <UnifiedLine
          key={`u${display}`}
          model={model}
          index={display}
          syntax={syntax}
          syntaxVersion={syntaxVersion}
          hunkAction={hunkAction}
          commentable={commentable}
          selected={lineSelected}
          tabStop={lineStop}
        />
      ),
    );
    if (editor && editor.display === display) {
      // Keyed by the editor, not the row: a refresh that moves its lines
      // keeps the same editor, and what was typed in it.
      rows.push(
        <NewCommentSlot
          key={`editor:${editor.id}`}
          editorId={editor.id}
          span={editor.span}
        />,
      );
    }
    for (const { comment, span } of comments.get(display) ?? []) {
      rows.push(
        <CommentSlot
          key={comment.id}
          comment={comment}
          span={span}
          editing={editingId === comment.id}
          sending={sending?.has(comment.id) ?? false}
        />,
      );
    }
  }
  const lines = end - start;
  return (
    <div
      className="[content-visibility:auto]"
      style={{ containIntrinsicBlockSize: `auto ${lines * 20}px` }}
    >
      {rows}
    </div>
  );
});

function displayRows(
  model: DiffFileModel,
  layout: DiffLayout,
  display: number,
): number[] {
  if (layout === "unified") return [display];
  const entry = model.split[display];
  if (!entry) return [];
  if (entry.kind === "full") return [entry.row];
  return [entry.left, entry.right].filter((row): row is number => row !== null);
}

function NewCommentSlot({
  editorId,
  span,
  awayQuote,
  note,
}: {
  editorId: string;
  span: CommentLineSpans;
  /** The lines as they were, when the diff no longer shows them. */
  awayQuote?: CommentAnchor["lines"];
  note?: string;
}) {
  const interaction = useContext(InteractionContext);
  if (!interaction) return null;
  return (
    <CommentComposer
      label={commentLinesLabel(span)}
      quote={awayQuote}
      note={note}
      initial={interaction.draft(editorId) ?? ""}
      onDraftChange={(text) => interaction.keepDraft(editorId, text)}
      submitLabel="Add comment"
      onSubmit={interaction.submitNew}
      onCancel={interaction.closeEditor}
    />
  );
}

function CommentSlot({
  comment,
  span,
  editing = false,
  sending = false,
  away,
}: {
  comment: ReviewComment;
  /** The lines it covers where the diff has them now. */
  span: CommentLineSpans;
  editing?: boolean;
  sending?: boolean;
  /**
   * Why it is not under its lines: they are hidden with whitespace, or its
   * code changed. Either way it shows its quote.
   */
  away?: "hidden" | "outdated";
}) {
  const interaction = useContext(InteractionContext);
  const review = interaction?.review;
  if (!interaction || !review) return null;
  if (editing) {
    const key = `edit:${comment.id}`;
    return (
      <CommentComposer
        label={commentLinesLabel(span)}
        quote={away ? comment.lines : undefined}
        initial={interaction.draft(key) ?? comment.body}
        onDraftChange={(text) => interaction.keepDraft(key, text)}
        submitLabel="Save"
        onSubmit={(body) => interaction.submitEdit(comment.id, body)}
        onCancel={interaction.closeEditor}
      />
    );
  }
  return (
    <CommentCard
      comment={comment}
      label={commentLinesLabel(span)}
      sending={sending || review.sending.has(comment.id)}
      showQuote={away !== undefined}
      outdated={away === "outdated"}
      onEdit={() => interaction.startEdit(comment.id)}
      onDelete={() => review.onDelete(comment.id)}
    />
  );
}

/** The marker and the words a screen reader says for a row's kind. */
const KIND_MARK: Record<DiffRow["kind"], { mark: string; spoken?: string }> = {
  add: { mark: "+", spoken: "added" },
  del: { mark: "-", spoken: "removed" },
  context: { mark: " " },
  hunk: { mark: "" },
  meta: { mark: "" },
};

/** The syntax of the text one side of a row shows, from the side it came from. */
function sideSyntax(
  syntax: FileSyntax | null,
  row: DiffRow,
  shown: RowSide,
): SyntaxLine | undefined {
  if (!syntax || row.hunk < 0) return undefined;
  const hunk = syntax.get(row.hunk);
  if (!hunk) return undefined;
  return (shown.side === "old" ? hunk.old : hunk.new).get(shown.source);
}

function isSelected(
  selected: { start: number; end: number } | null,
  row: number,
): boolean {
  return selected !== null && row >= selected.start && row <= selected.end;
}

function commentLabel(row: DiffRow): string {
  if (row.kind === "del") return `Comment on deleted line ${row.oldNo}`;
  return `Comment on line ${row.newNo}`;
}

/** The line numbers: a button that starts a comment where comments are taken. */
function Gutter({
  row,
  index,
  column,
  numbers,
  commentable,
  tabStop,
  display,
}: {
  row: DiffRow;
  index: number;
  column: Column;
  numbers: Array<{ side: "old" | "new"; value: number | null }>;
  commentable: boolean;
  tabStop: boolean;
  display: number;
}) {
  const content = numbers.map(({ side, value }) => (
    <span
      key={side}
      data-diff-gutter={side}
      className="w-[var(--diff-number)] shrink-0 px-1.5 text-right"
    >
      {value ?? ""}
    </span>
  ));
  const shared =
    "diff-gutter text-muted-foreground sticky left-0 z-[1] flex shrink-0 self-stretch font-mono text-xs leading-5 tabular-nums select-none";
  if (!commentable || !isCodeRow(row)) {
    return (
      <span className={shared} data-display={display}>
        {content}
      </span>
    );
  }
  return (
    <button
      type="button"
      data-diff-target=""
      data-row={index}
      data-column={column}
      data-display={display}
      tabIndex={tabStop ? 0 : -1}
      aria-label={commentLabel(row)}
      title={commentLabel(row)}
      className={cn(
        shared,
        "hover:text-foreground cursor-pointer focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset",
      )}
    >
      {content}
    </button>
  );
}

function CodeText({
  text,
  runs,
  emphasis,
  kind,
}: {
  text: string;
  runs?: SyntaxLine;
  emphasis?: readonly TextRange[];
  kind: DiffRow["kind"];
}) {
  if (!text) return " ";
  const usable =
    runs &&
    runs.reduce((length, run) => length + run.text.length, 0) === text.length
      ? runs
      : [{ text, role: null }];
  if (!emphasis || emphasis.length === 0) {
    if (usable.length === 1 && usable[0]!.role === null) return text;
    return usable.map((run, index) =>
      run.role ? (
        <span key={index} className={ROLE_CLASS[run.role]}>
          {run.text}
        </span>
      ) : (
        run.text
      ),
    );
  }
  const word =
    kind === "add"
      ? "bg-[var(--diff-add-word)] text-inherit"
      : "bg-[var(--diff-del-word)] text-inherit";
  const pieces: ReactNode[] = [];
  let offset = 0;
  let next = 0;
  for (const run of usable) {
    let from = 0;
    while (from < run.text.length) {
      const at = offset + from;
      while (next < emphasis.length && emphasis[next]!.end <= at) next += 1;
      const range = emphasis[next];
      const inside = range !== undefined && range.start <= at;
      const boundary = inside ? range.end : (range?.start ?? Infinity);
      const to = Math.min(run.text.length, boundary - offset);
      const piece = run.text.slice(from, to);
      const key = `${at}`;
      const inked = run.role ? ROLE_CLASS[run.role] : undefined;
      pieces.push(
        inside ? (
          <mark key={key} className={cn(word, inked)}>
            {piece}
          </mark>
        ) : inked ? (
          <span key={key} className={inked}>
            {piece}
          </span>
        ) : (
          piece
        ),
      );
      from = to;
    }
    offset += run.text.length;
  }
  return pieces;
}

function CodeCell({
  row,
  text,
  runs,
  emphasis,
  wrap,
}: {
  row: DiffRow;
  text: string;
  runs?: SyntaxLine;
  emphasis?: readonly TextRange[];
  wrap: boolean;
}) {
  const { mark, spoken } = KIND_MARK[row.kind];
  return (
    <>
      <span
        aria-hidden
        className={cn(
          "w-[2ch] shrink-0 text-center select-none",
          row.kind === "add" && STATUS_TEXT.ready,
          row.kind === "del" && STATUS_TEXT.critical,
        )}
      >
        {mark}
      </span>
      {/* The tint says added or removed to the eye; this says it aloud. */}
      {spoken && <span className="sr-only">{`${spoken}: `}</span>}
      <span
        data-diff-code=""
        className={cn(
          "text-foreground min-w-0 pr-3",
          wrap
            ? "flex-1 [overflow-wrap:anywhere] whitespace-pre-wrap"
            : "whitespace-pre",
        )}
      >
        <CodeText text={text} runs={runs} emphasis={emphasis} kind={row.kind} />
      </span>
    </>
  );
}

function HunkOrMeta({
  row,
  hunkAction,
  hiddenWhitespace,
  gutterWidth,
}: {
  row: DiffRow;
  hunkAction?: HunkAction;
  hiddenWhitespace: ReadonlyMap<number, number>;
  gutterWidth: string;
}) {
  const action =
    row.kind === "hunk"
      ? hunkAction?.(row.hunk, {
          hiddenWhitespace: hiddenWhitespace.get(row.hunk) ?? 0,
        })
      : undefined;
  return (
    <>
      {/*
        At least as wide as the gutters, and wider when the action needs it:
        side by side there is one gutter per side, narrower than "Revert".
      */}
      <span
        className="diff-gutter sticky left-0 z-[1] flex shrink-0 items-center justify-end self-stretch font-sans text-xs"
        style={{ minWidth: gutterWidth }}
        data-diff-gutter={action ? "action" : undefined}
      >
        {action}
      </span>
      <span
        className={cn(
          "min-w-0 px-2 text-xs whitespace-pre",
          row.kind === "hunk" ? STATUS_TEXT.pending : "text-muted-foreground",
        )}
      >
        {row.text}
      </span>
    </>
  );
}

const UnifiedLine = memo(function UnifiedLine({
  model,
  index,
  syntax,
  hunkAction,
  commentable,
  selected,
  tabStop,
}: {
  model: DiffFileModel;
  index: number;
  syntax: FileSyntax | null;
  /** Read only to redraw once more of the syntax is known. */
  syntaxVersion: number;
  hunkAction?: HunkAction;
  commentable: boolean;
  selected: { start: number; end: number } | null;
  tabStop: { row: number; column: Column } | null;
}) {
  const row = model.rows[index]!;
  const picked = isSelected(selected, index);
  const shown = rowSide(row, "new");
  if (row.kind === "hunk" || row.kind === "meta") {
    return (
      <div
        className={cn(
          "diff-line flex min-h-5 w-full items-center",
          row.kind === "hunk" && "my-1",
        )}
        data-kind={row.kind}
        data-row={index}
      >
        <HunkOrMeta
          row={row}
          hunkAction={hunkAction}
          hiddenWhitespace={model.hiddenWhitespace}
          gutterWidth="calc(var(--diff-number) * 2)"
        />
      </div>
    );
  }
  return (
    <div
      className="diff-line flex min-h-5 w-full"
      data-kind={row.kind}
      data-row={index}
      data-selected={picked || undefined}
    >
      <Gutter
        row={row}
        index={index}
        column="unified"
        display={index}
        numbers={[
          { side: "old", value: row.oldNo },
          { side: "new", value: row.newNo },
        ]}
        commentable={commentable}
        tabStop={tabStop?.row === index}
      />
      <CodeCell
        row={row}
        text={shown.text}
        runs={sideSyntax(syntax, row, shown)}
        emphasis={row.emphasis}
        wrap={false}
      />
    </div>
  );
});

const SplitLine = memo(function SplitLine({
  model,
  display,
  syntax,
  hunkAction,
  commentable,
  selected,
  tabStop,
}: {
  model: DiffFileModel;
  display: number;
  syntax: FileSyntax | null;
  /** Read only to redraw once more of the syntax is known. */
  syntaxVersion: number;
  hunkAction?: HunkAction;
  commentable: boolean;
  selected: { start: number; end: number } | null;
  tabStop: { row: number; column: Column } | null;
}) {
  const entry = model.split[display]!;
  if (entry.kind === "full") {
    const row = model.rows[entry.row]!;
    return (
      <div
        className={cn(
          "diff-line flex min-h-5 items-center",
          row.kind === "hunk" && "my-1",
        )}
        data-kind={row.kind}
        data-row={entry.row}
      >
        <HunkOrMeta
          row={row}
          hunkAction={hunkAction}
          hiddenWhitespace={model.hiddenWhitespace}
          gutterWidth="var(--diff-number)"
        />
      </div>
    );
  }
  const side = (index: number | null, column: "left" | "right") => {
    if (index === null) {
      return (
        <div
          className="diff-line bg-[var(--diff-gutter)] flex min-h-5 min-w-0"
          data-kind="empty"
          aria-hidden
        />
      );
    }
    const row = model.rows[index]!;
    // A context row made from a whitespace-only pair shows each side's own
    // line: the old indentation on the left, the new on the right.
    const shown = rowSide(row, column === "left" ? "old" : "new");
    return (
      <div
        className="diff-line flex min-h-5 min-w-0"
        data-kind={row.kind}
        data-row={index}
        data-selected={isSelected(selected, index) || undefined}
      >
        <Gutter
          row={row}
          index={index}
          column={column}
          display={display}
          numbers={[
            {
              side: column === "left" ? "old" : "new",
              value: column === "left" ? row.oldNo : row.newNo,
            },
          ]}
          commentable={commentable}
          tabStop={tabStop?.row === index && tabStop.column === column}
        />
        <CodeCell
          row={row}
          text={shown.text}
          runs={sideSyntax(syntax, row, shown)}
          emphasis={shown.source === row.source ? row.emphasis : undefined}
          wrap
        />
      </div>
    );
  };
  return (
    <div
      className="grid grid-cols-2 divide-x divide-border-subtle"
      data-display={display}
    >
      {side(entry.left, "left")}
      {side(entry.right, "right")}
    </div>
  );
});
