import type { Element, Root, RootContent, Text } from "hast";

/** A half-open character range in the source a tree was parsed from. */
export type HighlightRange = { start: number; end: number };

/**
 * How a cited passage reads wherever it is drawn — the extracted text, a plain
 * original, a rendered one — kept in one place so the three agree.
 *
 * The class is the hook a viewer scrolls to; the style is separate from it
 * because the mark inside a rendered tree is built by the plugin below rather
 * than written as an element.
 */
export const CITATION_MARK_CLASS = "citation-mark";
export const CITATION_MARK_STYLE = "rounded bg-yellow-200/60 dark:bg-yellow-500/25";
export const CITATION_MARK_LABEL = "Cited passage";

/**
 * Where each piece of a split text begins in the text it was split from.
 *
 * Both splits a citation has to survive — the viewer's chunks, the renderer's
 * blocks — are the verbatim source in order, so a piece starts at the sum of the
 * lengths before it and a range can be clipped onto it exactly.
 */
export function pieceStartOffsets(pieces: readonly string[]): number[] {
  let offset = 0;
  return pieces.map((piece) => {
    const start = offset;
    offset += piece.length;
    return start;
  });
}

/**
 * The part of a range that falls inside one piece, in that piece's own offsets,
 * or null for a piece the range does not reach. A passage crossing a boundary is
 * marked in both pieces rather than in neither.
 */
export function rangeWithinPiece(
  range: HighlightRange,
  pieceStart: number,
  pieceLength: number,
): HighlightRange | null {
  const start = Math.max(range.start - pieceStart, 0);
  const end = Math.min(range.end - pieceStart, pieceLength);
  return end > start ? { start, end } : null;
}

/**
 * Mark the text a character range covers, wherever it landed in the tree.
 *
 * The range addresses the string the parser was handed, which is what makes
 * this possible at all: every text node in a hast tree carries the offsets it
 * was read from, so the passage is found by arithmetic rather than by matching
 * its text against the rendered output. A range that spans several nodes — a
 * sentence with a bold word in it, or a passage crossing a paragraph — marks
 * each of them, since the words in between are not contiguous in the tree.
 *
 * Nodes whose position was dropped by an earlier transform are skipped rather
 * than guessed at; in practice that is syntax-highlighted code and rendered
 * math, whose text no longer stands in one-to-one relation to its source.
 */
export function rehypeHighlightRange({ range }: { range: HighlightRange }) {
  return (tree: Root) => {
    if (range.end <= range.start) return;
    markRange(tree, range.start, range.end);
  };
}

function markRange(parent: Root | Element, start: number, end: number): void {
  // Backwards, so replacing one child cannot shift the index of an unvisited one.
  for (let i = parent.children.length - 1; i >= 0; i--) {
    const child = parent.children[i]!;
    if (child.type === "element") {
      markRange(child, start, end);
      continue;
    }
    if (child.type !== "text") continue;

    const nodeStart = child.position?.start.offset;
    const nodeEnd = child.position?.end.offset;
    if (nodeStart == null || nodeEnd == null) continue;
    if (nodeEnd <= start || nodeStart >= end) continue;

    const replacement = splitAroundOverlap(child, nodeStart, start, end);
    if (replacement) parent.children.splice(i, 1, ...replacement);
  }
}

/**
 * One text node split in three around the part of it the range covers.
 *
 * A node's text is its source verbatim in all but a few cases — an escape, a
 * character reference — where it is shorter, and the offsets inside it are that
 * much further along than the text is. The overlap is clamped to the text
 * actually held, so the worst that costs is a boundary drawn a character or two
 * off inside the one node that held the escape.
 */
function splitAroundOverlap(
  node: Text,
  nodeStart: number,
  start: number,
  end: number,
): RootContent[] | null {
  const text = node.value;
  const from = clamp(start - nodeStart, 0, text.length);
  const to = clamp(end - nodeStart, from, text.length);
  if (to === from) return null;

  const mark: Element = {
    type: "element",
    tagName: "mark",
    properties: {
      className: [CITATION_MARK_CLASS, ...CITATION_MARK_STYLE.split(" ")],
      ariaLabel: CITATION_MARK_LABEL,
    },
    children: [{ type: "text", value: text.slice(from, to) }],
  };

  const parts: RootContent[] = [];
  if (from > 0) parts.push({ type: "text", value: text.slice(0, from) });
  parts.push(mark);
  if (to < text.length) parts.push({ type: "text", value: text.slice(to) });
  return parts;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(value, low), high);
}
