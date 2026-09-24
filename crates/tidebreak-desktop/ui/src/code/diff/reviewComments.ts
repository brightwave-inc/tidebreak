/**
 * Line comments on a diff, and the message they become.
 *
 * A comment is written against lines of a diff and waits in the workspace's
 * pending review until the next message goes. That message carries every
 * pending comment in one `<review_comments>` block after what the person
 * typed: the file, the lines, the lines as the diff showed them, and the
 * comment. The engine sees text, as it does for every other turn input
 * (decision 0046); the transcript reads the block back and folds it.
 */

/** One quoted line, as the diff showed it when the comment was written. */
export type ReviewCommentLine = {
  readonly kind: "add" | "del" | "context";
  readonly oldNo: number | null;
  readonly newNo: number | null;
  readonly text: string;
};

/**
 * Who wrote a comment. A person, for now. A second engine's review pass can
 * later fill the same pending review with its own findings, marked as its
 * own, and they travel to the agent the same way.
 */
export type ReviewCommentAuthor = { readonly kind: "person" };

export type ReviewComment = {
  readonly id: string;
  readonly author: ReviewCommentAuthor;
  readonly path: string;
  /** The turn whose diff it was written on; absent for the workspace diff. */
  readonly turnId?: string;
  /** The lines it is about, in diff order. Never empty. */
  readonly lines: readonly ReviewCommentLine[];
  readonly body: string;
  readonly createdAt: string;
};

function span(numbers: readonly number[]): string | null {
  if (numbers.length === 0) return null;
  const first = Math.min(...numbers);
  const last = Math.max(...numbers);
  return first === last ? String(first) : `${first}-${last}`;
}

/**
 * The lines a comment covers, as the message and the view name them. New
 * line numbers name added and unchanged lines, because that is the file the
 * agent can open; removed lines only have old numbers.
 */
export function commentLineSpans(lines: readonly ReviewCommentLine[]): {
  lines: string | null;
  oldLines: string | null;
} {
  const newNumbers = lines.flatMap((line) =>
    line.kind !== "del" && line.newNo !== null ? [line.newNo] : [],
  );
  const oldNumbers = lines.flatMap((line) =>
    line.kind === "del" && line.oldNo !== null ? [line.oldNo] : [],
  );
  return { lines: span(newNumbers), oldLines: span(oldNumbers) };
}

/** "Line 12", "Lines 12–14", "Deleted lines 3–4", for people. */
export function commentLinesLabel(lines: readonly ReviewCommentLine[]): string {
  const spans = commentLineSpans(lines);
  const words = (value: string, prefix: string) => {
    const [first, last] = value.split("-");
    return last ? `${prefix}lines ${first}–${last}` : `${prefix}line ${first}`;
  };
  if (spans.lines) {
    const label = words(spans.lines, "");
    return label.charAt(0).toUpperCase() + label.slice(1);
  }
  if (spans.oldLines) return words(spans.oldLines, "Deleted ");
  return "Lines";
}

const MARKER = { add: "+", del: "-", context: " " } as const;

function escapeAttribute(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function unescapeAttribute(value: string): string {
  return value
    .replace(/&quot;/g, '"')
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

/** A body line that would read as one of the block's own tags. */
const TAG_LINE = /^\\*<\/?(?:comment|review_comments)\b/;

function escapeBodyLine(line: string): string {
  return TAG_LINE.test(line) ? `\\${line}` : line;
}

function unescapeBodyLine(line: string): string {
  return TAG_LINE.test(line) && line.startsWith("\\") ? line.slice(1) : line;
}

/** A fence longer than any backtick run inside the quote. */
function fenceFor(lines: readonly string[]): string {
  let ticks = "```";
  while (lines.some((line) => line.includes(ticks))) ticks += "`";
  return ticks;
}

const OPEN = "<review_comments>";
const CLOSE = "</review_comments>";
const PREAMBLE =
  "The person reviewing your changes left these comments on the diff. Each one names a file and its lines, quotes those lines with diff markers (+ added, - removed), and then gives the comment. Address every comment.";

/** One comment as the agent reads it. */
function commentBlock(comment: ReviewComment): string {
  const spans = commentLineSpans(comment.lines);
  const attributes = [
    `path="${escapeAttribute(comment.path)}"`,
    spans.lines ? `lines="${spans.lines}"` : null,
    spans.oldLines ? `old_lines="${spans.oldLines}"` : null,
  ]
    .filter(Boolean)
    .join(" ");
  const quote = comment.lines.map((line) => `${MARKER[line.kind]}${line.text}`);
  const fence = fenceFor(quote);
  const body = comment.body.trim().split("\n").map(escapeBodyLine);
  return [
    `<comment ${attributes}>`,
    `${fence}diff`,
    ...quote,
    fence,
    ...body,
    "</comment>",
  ].join("\n");
}

/**
 * The message with its review comments after it, in one block. With no
 * comments it is the message exactly as typed.
 */
export function messageWithReviewComments(
  message: string,
  comments: readonly ReviewComment[],
): string {
  if (comments.length === 0) return message;
  const block = [OPEN, PREAMBLE, "", ...comments.map(commentBlock)]
    .join("\n")
    .concat(`\n${CLOSE}`);
  const text = message.trim();
  return text ? `${text}\n\n${block}` : block;
}

/** A comment read back out of a sent message. */
export type SentReviewComment = {
  readonly path: string;
  /** The new-file span, "12" or "12-14", when the comment has one. */
  readonly lines: string | null;
  /** The old-file span of removed lines, when the comment has any. */
  readonly oldLines: string | null;
  /** The quoted lines, each with its diff marker. */
  readonly quote: readonly string[];
  readonly body: string;
};

function attribute(tag: string, name: string): string | null {
  const match = new RegExp(`\\b${name}="([^"]*)"`).exec(tag);
  return match ? unescapeAttribute(match[1]!) : null;
}

function parseComments(inner: readonly string[]): SentReviewComment[] {
  const comments: SentReviewComment[] = [];
  let index = 0;
  while (index < inner.length) {
    const open = /^<comment( [^>]*)?>$/.exec(inner[index]!);
    if (!open) {
      index += 1;
      continue;
    }
    const tag = open[1] ?? "";
    index += 1;
    const quote: string[] = [];
    const fence = /^(`{3,})diff$/.exec(inner[index] ?? "");
    if (fence) {
      index += 1;
      while (index < inner.length && inner[index] !== fence[1]) {
        quote.push(inner[index]!);
        index += 1;
      }
      index += 1;
    }
    const body: string[] = [];
    while (index < inner.length && inner[index] !== "</comment>") {
      body.push(unescapeBodyLine(inner[index]!));
      index += 1;
    }
    index += 1;
    comments.push({
      path: attribute(tag, "path") ?? "",
      lines: attribute(tag, "lines"),
      oldLines: attribute(tag, "old_lines"),
      quote,
      body: body.join("\n").trim(),
    });
  }
  return comments;
}

/**
 * Take the review block back out of a sent message, so the transcript can
 * show the comments compactly instead of as the text the agent read.
 *
 * The block is the one the send put last. A message with no block, or with
 * one that never closes, comes back whole.
 */
export function splitReviewComments(message: string): {
  prose: string;
  comments: SentReviewComment[];
} {
  const lines = message.split("\n");
  let open = -1;
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    if (lines[index] === OPEN) {
      open = index;
      break;
    }
  }
  if (open === -1) return { prose: message, comments: [] };
  let close = -1;
  for (let index = lines.length - 1; index > open; index -= 1) {
    if (lines[index] === CLOSE) {
      close = index;
      break;
    }
  }
  if (close === -1) return { prose: message, comments: [] };
  const comments = parseComments(lines.slice(open + 1, close));
  if (comments.length === 0) return { prose: message, comments: [] };
  const prose = [...lines.slice(0, open), ...lines.slice(close + 1)]
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { prose, comments };
}
