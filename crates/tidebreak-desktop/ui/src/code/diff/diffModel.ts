import type { DiffFileGroup } from "../unifiedDiff";

/**
 * The rows a file's diff draws, and the three transforms the review view
 * applies to them: whitespace-insensitive pairing, the side-by-side layout,
 * and word-level emphasis inside changed lines.
 *
 * Everything here is pure and linear in the diff, apart from two small
 * dynamic programs that are capped, so a 5,000-line diff builds its model in
 * a few milliseconds.
 */

export type DiffRowKind = "add" | "del" | "context" | "hunk" | "meta";

/** A half-open span of a row's text, in UTF-16 code units. */
export type TextRange = { readonly start: number; readonly end: number };

export type DiffRow = {
  readonly kind: DiffRowKind;
  readonly oldNo: number | null;
  readonly newNo: number | null;
  /**
   * The line's content without its diff marker. A hunk or meta row keeps
   * the whole line.
   */
  readonly text: string;
  /**
   * Position of the line in the file group. Syntax runs and a hunk's revert
   * both find their line by it.
   */
  readonly source: number;
  /** Which hunk the row sits in, from zero; -1 before the first hunk. */
  readonly hunk: number;
  /**
   * A context row that was a changed pair until whitespace was ignored
   * keeps the old line here, even when the two texts are equal, so the
   * old side of the view can show that line with its own syntax.
   */
  readonly old?: { readonly text: string; readonly source: number };
  /** Changed spans of `text`, when the row pairs with a similar line. */
  readonly emphasis?: readonly TextRange[];
};

/** A row that can carry a comment: a line of code on either side. */
export function isCodeRow(row: DiffRow): boolean {
  return row.kind === "add" || row.kind === "del" || row.kind === "context";
}

/** Git header lines the view never shows: the reader already has the path. */
export function isNoisyDiffMeta(text: string): boolean {
  return (
    text.startsWith("index ") ||
    text.startsWith("--- ") ||
    text.startsWith("+++ ")
  );
}

function isBinaryMarker(text: string): boolean {
  return (
    (text.startsWith("Binary files ") && text.endsWith(" differ")) ||
    text === "GIT binary patch"
  );
}

/** Whether git reported the file as binary rather than giving its lines. */
export function isBinaryDiff(group: DiffFileGroup): boolean {
  return group.lines.some(
    (line) => line.kind === "meta" && isBinaryMarker(line.text),
  );
}

/**
 * One file's lines as rows: markers stripped, git's header noise dropped,
 * and git's binary marker left to the notice that says the same thing.
 */
export function diffRows(group: DiffFileGroup): DiffRow[] {
  const rows: DiffRow[] = [];
  let hunk = -1;
  group.lines.forEach((line, source) => {
    switch (line.kind) {
      case "hunk":
        hunk += 1;
        rows.push({ ...line, text: line.text, source, hunk });
        return;
      case "meta":
        if (isNoisyDiffMeta(line.text) || isBinaryMarker(line.text)) return;
        rows.push({ ...line, source, hunk });
        return;
      default:
        rows.push({ ...line, text: line.text.slice(1), source, hunk });
    }
  });
  return rows;
}

/**
 * A short key for what one file's diff shows: equal diffs, drawn the same
 * way, get equal keys, whichever objects hold them. Two 32-bit FNV-1a
 * hashes of every line, read in one pass.
 */
export function diffFingerprint(
  group: DiffFileGroup,
  ignoreWhitespace: boolean,
): string {
  let a = 0x811c9dc5;
  let b = 0x01000193 ^ 0x5bd1e995;
  const feed = (text: string) => {
    for (let index = 0; index < text.length; index += 1) {
      const code = text.charCodeAt(index);
      a = Math.imul(a ^ code, 0x01000193);
      b = Math.imul(b ^ code, 0x5bd1e995) ^ (b >>> 15);
    }
    // A separator no line holds, so "ab" + "c" never hashes as "a" + "bc".
    a = Math.imul(a ^ 0xffff, 0x01000193);
    b = Math.imul(b ^ 0xffff, 0x5bd1e995) ^ (b >>> 15);
  };
  feed(group.path);
  for (const line of group.lines) feed(line.text);
  return `${ignoreWhitespace ? "w" : "s"}${group.lines.length}.${(a >>> 0).toString(36)}.${(b >>> 0).toString(36)}`;
}

/** Counts of added and removed lines among `rows`. */
export function diffStatOf(rows: readonly DiffRow[]): {
  insertions: number;
  deletions: number;
} {
  let insertions = 0;
  let deletions = 0;
  for (const row of rows) {
    if (row.kind === "add") insertions += 1;
    else if (row.kind === "del") deletions += 1;
  }
  return { insertions, deletions };
}

/**
 * Runs of consecutive added and removed rows, as `[start, end)` index pairs.
 * Each run is one change: git writes its removed lines, then its added ones.
 */
export function changeBlocks(
  rows: readonly DiffRow[],
): Array<readonly [number, number]> {
  const blocks: Array<readonly [number, number]> = [];
  let start = -1;
  rows.forEach((row, index) => {
    const changed = row.kind === "add" || row.kind === "del";
    if (changed && start === -1) start = index;
    if (!changed && start !== -1) {
      blocks.push([start, index]);
      start = -1;
    }
  });
  if (start !== -1) blocks.push([start, rows.length]);
  return blocks;
}

/**
 * The line as `git diff -w` compares it. Git's whitespace is the space, the
 * tab, the carriage return, and the newline; a no-break space, a byte-order
 * mark, a vertical tab, and a form feed are content to git, so they are
 * content here too.
 */
function withoutGitWhitespace(text: string): string {
  return text.replace(/[ \t\r\n]+/g, "");
}

/** Git's "\ No newline at end of file", under the line it describes. */
function isNoNewlineMarker(row: DiffRow): boolean {
  return row.kind === "meta" && row.text.startsWith("\\");
}

/**
 * The changes `git diff -w` compares, as `[start, end)` index pairs: runs of
 * added and removed rows, each with the "\ No newline at end of file" marker
 * of any line in it. The marker sits between a file's old last line and its
 * new one, and to `-w` the newline it talks about is only whitespace.
 */
function whitespaceBlocks(
  rows: readonly DiffRow[],
): Array<readonly [number, number]> {
  const blocks: Array<readonly [number, number]> = [];
  let start = -1;
  rows.forEach((row, index) => {
    const changed = row.kind === "add" || row.kind === "del";
    if (changed && start === -1) start = index;
    if (!changed && start !== -1 && !isNoNewlineMarker(row)) {
      blocks.push([start, index]);
      start = -1;
    }
  });
  if (start !== -1) blocks.push([start, rows.length]);
  return blocks;
}

/** Beyond this many cells the alignment keeps only its common ends. */
const MAX_ALIGN_CELLS = 250_000;

/**
 * Pairs of equal entries between two sequences, in increasing order on both
 * sides: the longest common subsequence.
 *
 * Common leading and trailing runs are matched first in linear time, which
 * covers a re-indented block of any size. Only the middle that is left goes
 * through the quadratic table, and only while it stays under the cap.
 */
export function alignSequences(
  before: readonly string[],
  after: readonly string[],
  maxCells = MAX_ALIGN_CELLS,
): Array<readonly [number, number]> {
  const pairs: Array<readonly [number, number]> = [];
  let head = 0;
  while (
    head < before.length &&
    head < after.length &&
    before[head] === after[head]
  ) {
    pairs.push([head, head]);
    head += 1;
  }
  let tailBefore = before.length;
  let tailAfter = after.length;
  const tail: Array<readonly [number, number]> = [];
  while (
    tailBefore > head &&
    tailAfter > head &&
    before[tailBefore - 1] === after[tailAfter - 1]
  ) {
    tailBefore -= 1;
    tailAfter -= 1;
    tail.push([tailBefore, tailAfter]);
  }
  const rows = tailBefore - head;
  const columns = tailAfter - head;
  if (rows > 0 && columns > 0 && rows * columns <= maxCells) {
    const width = columns + 1;
    const table = new Uint32Array((rows + 1) * width);
    for (let i = rows - 1; i >= 0; i -= 1) {
      for (let j = columns - 1; j >= 0; j -= 1) {
        table[i * width + j] =
          before[head + i] === after[head + j]
            ? table[(i + 1) * width + j + 1] + 1
            : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < rows && j < columns) {
      if (before[head + i] === after[head + j]) {
        pairs.push([head + i, head + j]);
        i += 1;
        j += 1;
      } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) {
        i += 1;
      } else {
        j += 1;
      }
    }
  }
  for (let index = tail.length - 1; index >= 0; index -= 1) {
    pairs.push(tail[index]!);
  }
  return pairs;
}

/**
 * The rows as `git diff -w` would draw them, worked out from the diff alone.
 *
 * Inside each change, a removed line and an added line that differ only in
 * whitespace become one context row: the new text, both line numbers, and
 * the old line kept for the side-by-side view and for its syntax. A file
 * whose only change is a final newline has nothing left to show, as with
 * `-w`. A hunk left with no change at all goes, header and all. Working from
 * the diff rather than asking git again means the pull request's diff, which
 * GitHub produced, gets the same treatment as the worktree's.
 */
export function ignoreWhitespaceChanges(rows: readonly DiffRow[]): DiffRow[] {
  const merged: DiffRow[] = [];
  // A loop rather than a spread: a diff can run to tens of thousands of rows,
  // past what one call's argument list should carry.
  const append = (from: readonly DiffRow[], start: number, end: number) => {
    for (let index = start; index < end; index += 1) merged.push(from[index]!);
  };
  let cursor = 0;
  for (const [start, end] of whitespaceBlocks(rows)) {
    append(rows, cursor, start);
    const removed: DiffRow[] = [];
    const added: DiffRow[] = [];
    // The newline marker under a line, if git wrote one.
    const marker = new Map<DiffRow, DiffRow>();
    for (let index = start; index < end; index += 1) {
      const row = rows[index]!;
      if (row.kind === "del") removed.push(row);
      else if (row.kind === "add") added.push(row);
      else if (index > start) marker.set(rows[index - 1]!, row);
    }
    const emit = (row: DiffRow) => {
      merged.push(row);
      const note = marker.get(row);
      if (note) merged.push(note);
    };
    const emitRange = (
      from: readonly DiffRow[],
      first: number,
      last: number,
    ) => {
      for (let index = first; index < last; index += 1) emit(from[index]!);
    };
    const pairs = alignSequences(
      removed.map((row) => withoutGitWhitespace(row.text)),
      added.map((row) => withoutGitWhitespace(row.text)),
    );
    let nextRemoved = 0;
    let nextAdded = 0;
    for (const [removedIndex, addedIndex] of pairs) {
      emitRange(removed, nextRemoved, removedIndex);
      emitRange(added, nextAdded, addedIndex);
      const was = removed[removedIndex]!;
      const now = added[addedIndex]!;
      // The pair reads as its new line, so only the new line's marker, if
      // it has one, still says anything.
      const paired: DiffRow = {
        kind: "context",
        oldNo: was.oldNo,
        newNo: now.newNo,
        text: now.text,
        source: now.source,
        hunk: now.hunk,
        old: { text: was.text, source: was.source },
      };
      merged.push(paired);
      const note = marker.get(now);
      if (note) merged.push(note);
      nextRemoved = removedIndex + 1;
      nextAdded = addedIndex + 1;
    }
    emitRange(removed, nextRemoved, removed.length);
    emitRange(added, nextAdded, added.length);
    cursor = end;
  }
  append(rows, cursor, rows.length);

  const changedHunks = new Set<number>();
  for (const row of merged) {
    if (row.kind === "add" || row.kind === "del") changedHunks.add(row.hunk);
  }
  return merged.filter((row) => row.hunk < 0 || changedHunks.has(row.hunk));
}

/**
 * The removed and added rows of each change, paired in order: the first
 * removed line with the first added one, and so on. The pairing lays out the
 * side-by-side view and picks the lines word emphasis compares.
 */
export function changePairs(
  rows: readonly DiffRow[],
): Array<{ removed: number[]; added: number[] }> {
  return changeBlocks(rows).map(([start, end]) => {
    const removed: number[] = [];
    const added: number[] = [];
    for (let index = start; index < end; index += 1) {
      if (rows[index]!.kind === "del") removed.push(index);
      else added.push(index);
    }
    return { removed, added };
  });
}

/** One row of the side-by-side view: an index into the rows on each side. */
export type SplitRow =
  | {
      readonly kind: "pair";
      readonly left: number | null;
      readonly right: number | null;
    }
  | { readonly kind: "full"; readonly row: number };

/**
 * The side-by-side layout. Context sits on both sides; each change puts its
 * removed lines on the left and its added lines on the right, paired in
 * order, with a blank cell where one side runs out. Hunk and meta rows span
 * both sides.
 */
export function splitRows(rows: readonly DiffRow[]): SplitRow[] {
  const out: SplitRow[] = [];
  let index = 0;
  while (index < rows.length) {
    const row = rows[index]!;
    if (row.kind === "hunk" || row.kind === "meta") {
      out.push({ kind: "full", row: index });
      index += 1;
      continue;
    }
    if (row.kind === "context") {
      out.push({ kind: "pair", left: index, right: index });
      index += 1;
      continue;
    }
    const removed: number[] = [];
    const added: number[] = [];
    while (
      index < rows.length &&
      (rows[index]!.kind === "del" || rows[index]!.kind === "add")
    ) {
      if (rows[index]!.kind === "del") removed.push(index);
      else added.push(index);
      index += 1;
    }
    const length = Math.max(removed.length, added.length);
    for (let k = 0; k < length; k += 1) {
      out.push({
        kind: "pair",
        left: removed[k] ?? null,
        right: added[k] ?? null,
      });
    }
  }
  return out;
}

/** Lines longer than this are compared whole, never word by word. */
const MAX_WORD_DIFF_CHARS = 1_000;
/** The token table's size cap for one pair of lines. */
const MAX_WORD_DIFF_CELLS = 10_000;
/**
 * The share of the longer line's visible characters the two must have in
 * common before emphasis helps. Below it the lines are simply different, and
 * marking nearly every word says less than the row tint already does.
 */
const MIN_WORD_DIFF_SIMILARITY = 0.3;

type Token = { text: string; start: number };

const TOKEN = /[\p{L}\p{N}_]+|\s+|[^\p{L}\p{N}_\s]/gu;

function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  for (const match of text.matchAll(TOKEN)) {
    tokens.push({ text: match[0], start: match.index ?? 0 });
  }
  return tokens;
}

function visibleLength(text: string): number {
  return text.replace(/\s+/g, "").length;
}

function rangesOf(tokens: readonly Token[], changed: readonly boolean[]) {
  const ranges: TextRange[] = [];
  tokens.forEach((token, index) => {
    if (!changed[index]) return;
    const end = token.start + token.text.length;
    const last = ranges.at(-1);
    if (last && last.end === token.start) {
      ranges[ranges.length - 1] = { start: last.start, end };
    } else {
      ranges.push({ start: token.start, end });
    }
  });
  return ranges;
}

/**
 * Bridge one whitespace token between two changed tokens, so `a b` changing
 * to `c d` reads as one edit instead of two with a gap between them.
 */
function bridgeGaps(tokens: readonly Token[], changed: boolean[]): void {
  for (let index = 1; index < tokens.length - 1; index += 1) {
    if (
      !changed[index] &&
      changed[index - 1] &&
      changed[index + 1] &&
      /^\s+$/.test(tokens[index]!.text)
    ) {
      changed[index] = true;
    }
  }
}

/**
 * The spans that changed between a removed line and the added line paired
 * with it, token by token: words, runs of whitespace, and single symbols.
 *
 * Returns null when there is nothing worth marking: identical lines, lines
 * too long to compare cheaply, or lines too different for emphasis to add
 * anything beyond the row's own tint.
 */
export function wordDiff(
  before: string,
  after: string,
): { before: TextRange[]; after: TextRange[] } | null {
  if (before === after) return null;
  if (
    before.length > MAX_WORD_DIFF_CHARS ||
    after.length > MAX_WORD_DIFF_CHARS
  ) {
    return null;
  }
  const a = tokenize(before);
  const b = tokenize(after);
  let head = 0;
  while (head < a.length && head < b.length && a[head]!.text === b[head]!.text)
    head += 1;
  let tailA = a.length;
  let tailB = b.length;
  while (
    tailA > head &&
    tailB > head &&
    a[tailA - 1]!.text === b[tailB - 1]!.text
  ) {
    tailA -= 1;
    tailB -= 1;
  }
  const changedA = a.map((_, index) => index >= head && index < tailA);
  const changedB = b.map((_, index) => index >= head && index < tailB);
  const middleA = a.slice(head, tailA).map((token) => token.text);
  const middleB = b.slice(head, tailB).map((token) => token.text);
  if (middleA.length > 0 && middleB.length > 0) {
    const pairs = alignSequences(middleA, middleB, MAX_WORD_DIFF_CELLS);
    for (const [i, j] of pairs) {
      changedA[head + i] = false;
      changedB[head + j] = false;
    }
  }

  let common = 0;
  a.forEach((token, index) => {
    if (!changedA[index]) common += visibleLength(token.text);
  });
  const longest = Math.max(visibleLength(before), visibleLength(after));
  // Lines that differ only in whitespace have nothing visible to compare,
  // and the changed whitespace is exactly what the reader needs to see.
  if (longest > 0 && common / longest < MIN_WORD_DIFF_SIMILARITY) return null;
  if (changedA.every(Boolean) && changedB.every(Boolean)) return null;

  bridgeGaps(a, changedA);
  bridgeGaps(b, changedB);
  return { before: rangesOf(a, changedA), after: rangesOf(b, changedB) };
}

/**
 * The rows with word emphasis on each removed line and the added line it
 * pairs with. Rows without a partner, or with a partner too different to
 * compare, keep only their tint.
 */
export function withWordEmphasis(rows: readonly DiffRow[]): DiffRow[] {
  const out = rows.slice();
  for (const { removed, added } of changePairs(rows)) {
    const length = Math.min(removed.length, added.length);
    for (let k = 0; k < length; k += 1) {
      const was = out[removed[k]!]!;
      const now = out[added[k]!]!;
      const diff = wordDiff(was.text, now.text);
      if (!diff) continue;
      out[removed[k]!] = { ...was, emphasis: diff.before };
      out[added[k]!] = { ...now, emphasis: diff.after };
    }
  }
  return out;
}

/** One side of a row as the view draws it: the text, and where it came from. */
export type RowSide = {
  readonly text: string;
  /** The line's position in the file group, which its syntax is keyed by. */
  readonly source: number;
  /** The side of the hunk the line was highlighted with. */
  readonly side: "old" | "new";
};

/**
 * What a row shows on one side of the diff. A removed line is always old and
 * an added line always new. A context row shows its own text, except that the
 * old side of a whitespace pair shows the old line. The view takes a line's
 * text and its colors from this one answer, so the two cannot disagree.
 */
export function rowSide(row: DiffRow, side: "old" | "new"): RowSide {
  if (row.kind === "del") {
    return { text: row.text, source: row.source, side: "old" };
  }
  if (row.kind === "add") {
    return { text: row.text, source: row.source, side: "new" };
  }
  if (side === "old" && row.old) {
    return { text: row.old.text, source: row.old.source, side: "old" };
  }
  return { text: row.text, source: row.source, side };
}

/** Where a comment points: a line on one side of the diff. */
export type RowAnchor = { readonly side: "old" | "new"; readonly line: number };

/**
 * The line a row stands for. Added and context rows are named by their line
 * in the new file, which is what the agent can open; a removed row exists
 * only in the old one.
 */
export function rowAnchor(row: DiffRow): RowAnchor | null {
  if (row.kind === "del" && row.oldNo !== null) {
    return { side: "old", line: row.oldNo };
  }
  if ((row.kind === "add" || row.kind === "context") && row.newNo !== null) {
    return { side: "new", line: row.newNo };
  }
  return null;
}

export function sameAnchor(a: RowAnchor, b: RowAnchor): boolean {
  return a.side === b.side && a.line === b.line;
}

/** Everything the view draws for one file, in both layouts. */
export type DiffFileModel = {
  readonly rows: readonly DiffRow[];
  /** The side-by-side layout of the same rows. */
  readonly split: readonly SplitRow[];
  /** Git gave no lines: the file is binary. */
  readonly binary: boolean;
  /**
   * Every change in the file was whitespace, and hiding whitespace left
   * nothing to show.
   */
  readonly onlyWhitespace: boolean;
  /** Digits in the largest line number, which sizes both gutters. */
  readonly gutterDigits: number;
  /**
   * Per hunk, how many lines hiding whitespace drew as unchanged though
   * their whitespace changed. Reverting the hunk puts them back too.
   */
  readonly hiddenWhitespace: ReadonlyMap<number, number>;
};

export function buildDiffFileModel(
  group: DiffFileGroup,
  options: { ignoreWhitespace: boolean },
): DiffFileModel {
  const all = diffRows(group);
  const hadChanges = all.some(
    (row) => row.kind === "add" || row.kind === "del",
  );
  const shaped = options.ignoreWhitespace ? ignoreWhitespaceChanges(all) : all;
  const rows = withWordEmphasis(shaped);
  let widest = 1;
  const hiddenWhitespace = new Map<number, number>();
  for (const row of rows) {
    widest = Math.max(widest, row.oldNo ?? 0, row.newNo ?? 0);
    if (row.old) {
      hiddenWhitespace.set(row.hunk, (hiddenWhitespace.get(row.hunk) ?? 0) + 1);
    }
  }
  return {
    rows,
    split: splitRows(rows),
    binary: isBinaryDiff(group),
    onlyWhitespace:
      options.ignoreWhitespace &&
      hadChanges &&
      !rows.some((row) => row.kind === "add" || row.kind === "del"),
    gutterDigits: String(widest).length,
    hiddenWhitespace,
  };
}
