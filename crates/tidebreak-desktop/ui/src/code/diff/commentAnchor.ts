import { isCodeRow, type DiffRow } from "./diffModel";
import {
  commentLineSpans,
  MAX_QUOTED_LINES,
  type CommentLineSpans,
  type ReviewCommentContext,
  type ReviewCommentLine,
} from "./reviewComments";

/**
 * Where a comment's lines are in a diff that has moved on since it was
 * written.
 *
 * A comment holds on to the code it quotes, not to line numbers: an agent
 * that adds a line above it moves every number below, and a number alone
 * would then name a different line. So the diff is searched for the quoted
 * lines themselves. Where they appear more than once, the code around them
 * when the comment was written picks the place. Where they no longer appear,
 * or nothing tells the places apart, the comment is outdated: it keeps its
 * quote and says so, rather than settling on a line it was never about.
 */

/** Lines of code kept on each side of a comment's lines. */
export const CONTEXT_LINES = 3;

/** What a comment needs to find its lines again. */
export type CommentAnchor = {
  readonly lines: readonly ReviewCommentLine[];
  /** Lines the comment covers after its quote stops. */
  readonly unquoted?: number;
  readonly context?: ReviewCommentContext;
};

export type CommentPlacement =
  | {
      readonly kind: "placed";
      /** The first and last row the comment covers. */
      readonly start: number;
      readonly end: number;
      /** The quoted lines, numbered where they are now. */
      readonly lines: readonly ReviewCommentLine[];
      /** The spans of every line the comment covers now. */
      readonly span: CommentLineSpans;
    }
  | { readonly kind: "outdated" };

const OUTDATED: CommentPlacement = { kind: "outdated" };

/** A row as a comment quotes it: whitespace pairs keep both their texts. */
export function quoteRow(row: DiffRow): ReviewCommentLine | null {
  if (row.kind !== "add" && row.kind !== "del" && row.kind !== "context") {
    return null;
  }
  return {
    kind: row.kind,
    oldNo: row.oldNo,
    newNo: row.newNo,
    text: row.text,
    ...(row.old && row.old.text !== row.text ? { oldText: row.old.text } : {}),
  };
}

/** The code rows from `start` to `end`, inclusive, as a comment quotes them. */
export function quoteRows(
  rows: readonly DiffRow[],
  start: number,
  end: number,
): ReviewCommentLine[] {
  const lines: ReviewCommentLine[] = [];
  for (let index = start; index <= end; index += 1) {
    const line = quoteRow(rows[index]!);
    if (line) lines.push(line);
  }
  return lines;
}

/** Up to `count` code rows next to `from`, stepping `step`, inside its hunk. */
function neighbors(
  rows: readonly DiffRow[],
  from: number,
  step: 1 | -1,
  count: number,
): string[] {
  const hunk = rows[from]?.hunk;
  const found: string[] = [];
  for (
    let index = from + step;
    index >= 0 && index < rows.length && found.length < count;
    index += step
  ) {
    const row = rows[index]!;
    if (row.hunk !== hunk || row.kind === "hunk") break;
    if (isCodeRow(row)) found.push(row.text);
  }
  return found;
}

/**
 * The anchor a new comment on rows `start` to `end` keeps: at most
 * `MAX_QUOTED_LINES` of its lines, how many more it covers, where the whole
 * range sits, and the code around it.
 */
export function anchorRows(
  rows: readonly DiffRow[],
  start: number,
  end: number,
): CommentAnchor & { span: CommentLineSpans } {
  const all = quoteRows(rows, start, end);
  const lines = all.slice(0, MAX_QUOTED_LINES);
  return {
    lines,
    ...(all.length > lines.length
      ? { unquoted: all.length - lines.length }
      : {}),
    span: commentLineSpans(all),
    context: {
      before: neighbors(rows, start, -1, CONTEXT_LINES),
      after: neighbors(rows, end, 1, CONTEXT_LINES),
    },
  };
}

/** Whether `row` shows the line a comment quoted, in either whitespace view. */
export function rowShowsLine(row: DiffRow, line: ReviewCommentLine): boolean {
  // A whitespace pair drawn as one row: its old line and its new line.
  const paired = row.kind === "context" && row.old !== undefined;
  switch (line.kind) {
    case "del":
      return (
        (row.kind === "del" && row.text === line.text) ||
        (paired && row.old!.text === line.text)
      );
    case "add":
      return (
        (row.kind === "add" && row.text === line.text) ||
        (paired && row.text === line.text)
      );
    case "context":
      if (line.oldText !== undefined) {
        // Quoted from a whitespace pair: the same pair, or, with whitespace
        // shown, the new line it was drawn with.
        return (
          (paired &&
            row.text === line.text &&
            row.old!.text === line.oldText) ||
          (row.kind === "add" && row.text === line.text)
        );
      }
      return (
        row.kind === "context" &&
        row.text === line.text &&
        (!paired || row.old!.text === row.text)
      );
    default:
      return false;
  }
}

/** Rows by the texts they show, so a search starts where the first line is. */
export type RowIndex = ReadonlyMap<string, readonly number[]>;

export function indexRows(rows: readonly DiffRow[]): RowIndex {
  const index = new Map<string, number[]>();
  const put = (text: string, row: number) => {
    const list = index.get(text);
    if (list) {
      if (list.at(-1) !== row) list.push(row);
    } else {
      index.set(text, [row]);
    }
  };
  rows.forEach((row, at) => {
    if (!isCodeRow(row)) return;
    put(row.text, at);
    if (row.old) put(row.old.text, at);
  });
  return index;
}

/** The rows `lines` sit on when the first of them is row `start`, or null. */
function matchFrom(
  rows: readonly DiffRow[],
  start: number,
  lines: readonly ReviewCommentLine[],
): number[] | null {
  const hunk = rows[start]!.hunk;
  const matched: number[] = [];
  let index = start;
  for (const line of lines) {
    // A "\ No newline at end of file" note can sit between quoted lines.
    while (
      index < rows.length &&
      rows[index]!.hunk === hunk &&
      rows[index]!.kind === "meta"
    ) {
      index += 1;
    }
    const row = rows[index];
    if (!row || row.hunk !== hunk || !rowShowsLine(row, line)) return null;
    matched.push(index);
    index += 1;
  }
  return matched;
}

/** The last row of a range whose quote stopped `unquoted` lines short. */
function extendPast(
  rows: readonly DiffRow[],
  last: number,
  unquoted: number,
): number {
  const hunk = rows[last]!.hunk;
  let end = last;
  let left = unquoted;
  for (let index = last + 1; left > 0 && index < rows.length; index += 1) {
    const row = rows[index]!;
    if (row.hunk !== hunk || row.kind === "hunk") break;
    if (!isCodeRow(row)) continue;
    end = index;
    left -= 1;
  }
  return end;
}

/** How many of the remembered neighbors are still next to rows `start`–`end`. */
function contextScore(
  rows: readonly DiffRow[],
  start: number,
  end: number,
  context: ReviewCommentContext | undefined,
): number {
  if (!context) return 0;
  const before = neighbors(rows, start, -1, context.before.length);
  const after = neighbors(rows, end, 1, context.after.length);
  let score = 0;
  context.before.forEach((text, index) => {
    if (before[index] === text) score += 1;
  });
  context.after.forEach((text, index) => {
    if (after[index] === text) score += 1;
  });
  return score;
}

/** The number a line goes by: its new number, or its old one if removed. */
function lineNumber(line: {
  kind: string;
  oldNo: number | null;
  newNo: number | null;
}): number | null {
  return line.kind === "del" ? line.oldNo : line.newNo;
}

/**
 * Where a comment's lines are in `rows`, or that they are gone.
 *
 * Every place the quoted lines appear, in order and inside one hunk, is a
 * candidate. One candidate is the place. Among several, the one with the
 * most of the remembered code around it wins, and a tie goes to the one
 * nearest the line the comment last sat on. Several with none of that code
 * around them are indistinguishable, so the comment is outdated.
 */
export function placeComment(
  rows: readonly DiffRow[],
  anchor: CommentAnchor,
  index: RowIndex = indexRows(rows),
): CommentPlacement {
  const first = anchor.lines[0];
  const last = anchor.lines.at(-1);
  if (!first || !last) return OUTDATED;
  const was = lineNumber(last);
  const candidates: Array<{
    matched: number[];
    end: number;
    score: number;
    distance: number;
  }> = [];
  for (const start of index.get(first.text) ?? []) {
    const matched = matchFrom(rows, start, anchor.lines);
    if (!matched) continue;
    const end = extendPast(rows, matched.at(-1)!, anchor.unquoted ?? 0);
    const lastRow = rows[matched.at(-1)!]!;
    const now = last.kind === "del" ? lastRow.oldNo : lastRow.newNo;
    candidates.push({
      matched,
      end,
      score: contextScore(rows, start, end, anchor.context),
      distance: was === null || now === null ? Infinity : Math.abs(now - was),
    });
  }
  if (candidates.length === 0) return OUTDATED;
  let chosen = candidates[0]!;
  if (candidates.length > 1) {
    const best = Math.max(...candidates.map((candidate) => candidate.score));
    if (best === 0) return OUTDATED;
    chosen = candidates
      .filter((candidate) => candidate.score === best)
      .reduce((nearest, candidate) =>
        candidate.distance < nearest.distance ? candidate : nearest,
      );
  }
  const start = chosen.matched[0]!;
  const lines = anchor.lines.map((line, at) => {
    const row = rows[chosen.matched[at]!]!;
    // The quote keeps its own kind and text; only the numbers move.
    return {
      ...line,
      oldNo: line.kind === "add" ? null : (row.oldNo ?? line.oldNo),
      newNo: line.kind === "del" ? null : (row.newNo ?? line.newNo),
    };
  });
  return {
    kind: "placed",
    start,
    end: chosen.end,
    lines,
    span: commentLineSpans(quoteRows(rows, start, chosen.end)),
  };
}
