import { fromMarkdown } from "mdast-util-from-markdown";

/**
 * A code fence that has opened but not closed, at the end of a split.
 *
 * While a message streams, a fence stays open for as long as the model is
 * writing code, and only the fence can change as text is appended to it.
 */
export type OpenFence = {
  /** The opening run of backticks or tildes; a closing fence repeats it. */
  marker: string;
  /** The first word of the info string, when there is one. */
  language: string | null;
};

/**
 * Markdown source cut into its top-level blocks.
 *
 * Kept whole so the next, longer version of the same text can be split from
 * where this one ended instead of from the start.
 */
export type MarkdownBlockSplit = {
  /** The source the blocks were cut from. */
  source: string;
  /** Each block's verbatim source; joined, they reproduce `source`. */
  blocks: readonly string[];
  /** Where the last block starts in `source`. */
  lastStart: number;
  /** Set when the last block is a code fence that has not closed yet. */
  openFence: OpenFence | null;
};

const EMPTY_SPLIT: MarkdownBlockSplit = {
  source: "",
  blocks: [],
  lastStart: 0,
  openFence: null,
};

/**
 * Split Markdown source into top-level block strings using mdast's parser.
 *
 * Each returned string is the verbatim source of one block-level node plus the
 * blank lines that follow it, so concatenating the result reproduces the input.
 * The streaming renderer relies on this to memoize already-settled blocks and
 * re-parse only the trailing (still-growing) block as content streams in.
 *
 * mdast is the same parser react-markdown (remark/rehype) uses for rendering,
 * so block grouping (loose lists, tables, fenced code) matches the renderer.
 */
export function splitMarkdownBlocks(content: string): string[] {
  return [...splitMarkdownSource(content).blocks];
}

/**
 * Split `content` into top-level blocks, reusing `previous` when `content`
 * only appends to it.
 *
 * Appending text can change only the last top-level block: every block before
 * it already ended where the next one began. So the settled blocks are kept as
 * they are and only the last one is parsed again, together with whatever was
 * appended. A streaming message used to be parsed from its first character on
 * every typewriter tick, which grew with the message.
 *
 * An open code fence is cheaper still. Nothing but a closing fence can end it,
 * so appended lines that close nothing extend the fence without any parsing.
 */
export function splitMarkdownSource(
  content: string,
  previous: MarkdownBlockSplit | null = null,
): MarkdownBlockSplit {
  if (content.length === 0) return EMPTY_SPLIT;
  if (
    previous === null ||
    previous.blocks.length === 0 ||
    !content.startsWith(previous.source)
  ) {
    return splitFrom(content, 0, []);
  }
  if (content.length === previous.source.length) return previous;
  if (previous.openFence && !closesFence(content, previous)) {
    const blocks = previous.blocks.slice(0, -1);
    blocks.push(content.slice(previous.lastStart));
    return {
      source: content,
      blocks,
      lastStart: previous.lastStart,
      openFence: previous.openFence,
    };
  }
  return splitFrom(content, previous.lastStart, previous.blocks.slice(0, -1));
}

/** Parse `content` from `from` on, after the blocks already settled before it. */
function splitFrom(
  content: string,
  from: number,
  settled: string[],
): MarkdownBlockSplit {
  const tail = content.slice(from);
  const blocks = settled;
  let lastStart = from;
  let lastType: string | null = null;
  try {
    const nodes = fromMarkdown(tail).children;
    const firstStart = nodes[0]?.position?.start.offset ?? tail.length;
    if (nodes.length === 0 || firstStart > 0) {
      blocks.push(tail.slice(0, nodes.length === 0 ? tail.length : firstStart));
    }
    for (let index = 0; index < nodes.length; index += 1) {
      const node = nodes[index]!;
      const start = node.position?.start.offset;
      if (start == null) continue;
      const nextStart = nodes[index + 1]?.position?.start.offset ?? tail.length;
      const raw = tail.slice(start, nextStart);
      if (raw.length === 0) continue;
      blocks.push(raw);
      lastStart = from + start;
      lastType = node.type;
    }
  } catch {
    // A parser failure costs the tail its structure, never its text.
    blocks.push(tail);
    lastStart = from;
    lastType = null;
  }
  if (blocks.length === 0) blocks.push(content);
  return {
    source: content,
    blocks,
    lastStart,
    openFence:
      lastType === "code" ? openFenceOf(blocks[blocks.length - 1]!) : null,
  };
}

const FENCE_OPENING = /^ {0,3}(`{3,}|~{3,})(.*)$/;

/**
 * The open fence `block` starts with, or `null` when it is not a fenced code
 * block or its fence has already closed.
 *
 * The opening line must be complete: until its newline arrives, the info
 * string may still be growing, and a line of backticks alone may yet turn out
 * to be something else.
 */
function openFenceOf(block: string): OpenFence | null {
  const newline = block.indexOf("\n");
  if (newline < 0) return null;
  const opening = FENCE_OPENING.exec(
    block.slice(0, newline).replace(/\r$/, ""),
  );
  if (!opening) return null;
  const [, marker = "", info = ""] = opening;
  // A backtick fence's info string cannot hold a backtick; a line that does
  // is inline code in a paragraph, not a fence.
  if (marker.startsWith("`") && info.includes("`")) return null;
  const fence: OpenFence = {
    marker,
    language: info.trim().split(/\s+/)[0] || null,
  };
  const lines = block.slice(newline + 1).split("\n");
  return lines.some((line) => isClosingFence(line, fence)) ? null : fence;
}

/**
 * Whether the text appended since `previous` closes its open fence.
 *
 * Scanning starts at the line `previous` was still typing, because the
 * characters that complete a closing fence can arrive a few at a time.
 */
function closesFence(content: string, previous: MarkdownBlockSplit): boolean {
  const fence = previous.openFence;
  if (!fence) return false;
  const bodyStart = content.indexOf("\n", previous.lastStart) + 1;
  const typing = previous.source.lastIndexOf("\n") + 1;
  const scanFrom = Math.max(bodyStart, typing);
  return content
    .slice(scanFrom)
    .split("\n")
    .some((line) => isClosingFence(line, fence));
}

const FENCE_CLOSING = /^ {0,3}([`~]{3,})[ \t\r]*$/;

/** CommonMark's closing fence: the opening character, at least as many. */
function isClosingFence(line: string, fence: OpenFence): boolean {
  const run = FENCE_CLOSING.exec(line)?.[1];
  return (
    run !== undefined &&
    run.length >= fence.marker.length &&
    run === fence.marker[0]!.repeat(run.length)
  );
}

/**
 * The code an open fence holds so far, as the parser would read it: the lines
 * after the opening fence, without the final line break.
 *
 * A top-level block starts where its node starts, after any indentation, so
 * the fence's own indentation never reaches the lines here.
 */
export function openFenceCode(block: string): string {
  const newline = block.indexOf("\n");
  if (newline < 0) return "";
  const body = block.slice(newline + 1);
  return body.endsWith("\n") ? body.slice(0, -1) : body;
}
