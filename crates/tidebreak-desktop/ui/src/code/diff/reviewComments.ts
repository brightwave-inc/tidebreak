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
  /**
   * A line that hiding whitespace drew as unchanged, though its whitespace
   * changed, keeps the old line's text here; `text` is the new line's.
   */
  readonly oldText?: string;
};

/**
 * Who wrote a comment. A person, for now. A second engine's review pass can
 * later fill the same pending review with its own findings, marked as its
 * own, and they travel to the agent the same way.
 */
export type ReviewCommentAuthor = { readonly kind: "person" };

/** The code on either side of a comment's lines, nearest line first. */
export type ReviewCommentContext = {
  readonly before: readonly string[];
  readonly after: readonly string[];
};

/** A comment stores at most this many quoted lines. */
export const MAX_QUOTED_LINES = 200;

export type ReviewComment = {
  readonly id: string;
  readonly author: ReviewCommentAuthor;
  readonly path: string;
  /** The turn whose diff it was written on; absent for the workspace diff. */
  readonly turnId?: string;
  /**
   * The lines it is about, in diff order: their text as it was when the
   * comment was written, and their numbers as the diff last placed them.
   * Never empty, and never more than `MAX_QUOTED_LINES`.
   */
  readonly lines: readonly ReviewCommentLine[];
  /** Lines the comment covers after the quote stops. */
  readonly unquoted?: number;
  /**
   * Where the whole range sits, when the quote stops short of its end: the
   * spans `commentLineSpans` would give for every line it covers.
   */
  readonly span?: CommentLineSpans;
  /** The code around the lines, which tells two places with the same lines apart. */
  readonly context?: ReviewCommentContext;
  /** The lines it quotes changed, or left the diff, after it was written. */
  readonly outdated?: boolean;
  readonly body: string;
  readonly createdAt: string;
};

/** The new-file and old-file spans a comment covers, such as "12-14". */
export type CommentLineSpans = {
  readonly lines: string | null;
  readonly oldLines: string | null;
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
export function commentLineSpans(
  lines: readonly ReviewCommentLine[],
): CommentLineSpans {
  const newNumbers = lines.flatMap((line) =>
    line.kind !== "del" && line.newNo !== null ? [line.newNo] : [],
  );
  const oldNumbers = lines.flatMap((line) =>
    line.kind === "del" && line.oldNo !== null ? [line.oldNo] : [],
  );
  return { lines: span(newNumbers), oldLines: span(oldNumbers) };
}

/** A comment's spans: its whole range, even where the quote stops short. */
export function spansOf(
  comment: Pick<ReviewComment, "lines" | "span">,
): CommentLineSpans {
  return comment.span ?? commentLineSpans(comment.lines);
}

/**
 * "Line 12", "Lines 12–14", "Deleted lines 3–4", or both halves of a range
 * that takes in a change: "Line 12 and deleted line 12".
 */
export function commentLinesLabel(
  lines: readonly ReviewCommentLine[] | CommentLineSpans,
): string {
  const spans = "lines" in lines ? lines : commentLineSpans(lines);
  const words = (value: string, prefix: string) => {
    const [first, last] = value.split("-");
    return last ? `${prefix}lines ${first}–${last}` : `${prefix}line ${first}`;
  };
  const parts = [
    spans.lines ? words(spans.lines, "") : null,
    spans.oldLines ? words(spans.oldLines, "deleted ") : null,
  ].filter((part): part is string => part !== null);
  if (parts.length === 0) return "Lines";
  const label = parts.join(" and ");
  return label.charAt(0).toUpperCase() + label.slice(1);
}

const MARKER = { add: "+", del: "-", context: " " } as const;

/**
 * An attribute value on one line. The block is read back line by line, so a
 * newline in a path is written as a character reference like the rest.
 */
function escapeAttribute(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\n/g, "&#10;")
    .replace(/\r/g, "&#13;");
}

function unescapeAttribute(value: string): string {
  return value
    .replace(/&#10;/g, "\n")
    .replace(/&#13;/g, "\r")
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
  "The person reviewing your changes left these comments on a diff. Each comment names a file and the diff it was written on: the working tree against its base branch, or the changes one turn made. It quotes its lines with diff markers (+ added, - removed) and then gives the comment. Line numbers are from the diff as the reviewer last saw it, so if the file has changed since, find the lines by their quote. A comment marked outdated quotes code that has changed since it was written. Address every comment.";

/** The diff a comment was written on, as the block names it. */
const WORKING_TREE = "working tree";

/**
 * How the block names the diff of a turn: "turn 3" when the conversation
 * the message goes to knows the turn, or null when it does not.
 */
export type TurnNamer = (turnId: string) => string | null;

function diffName(comment: ReviewComment, turnName?: TurnNamer): string {
  if (!comment.turnId) return WORKING_TREE;
  return turnName?.(comment.turnId) ?? "an earlier turn";
}

/** One comment as the agent reads it. */
function commentBlock(comment: ReviewComment, turnName?: TurnNamer): string {
  const spans = spansOf(comment);
  const quoted = comment.lines.length;
  const covered = quoted + (comment.unquoted ?? 0);
  const attributes = [
    `path="${escapeAttribute(comment.path)}"`,
    `diff="${escapeAttribute(diffName(comment, turnName))}"`,
    spans.lines ? `lines="${spans.lines}"` : null,
    spans.oldLines ? `old_lines="${spans.oldLines}"` : null,
    covered > quoted ? `quote="first ${quoted} of ${covered} lines"` : null,
    comment.outdated ? 'outdated="true"' : null,
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

/** The block a message carries its review comments in. */
export function reviewCommentsBlock(
  comments: readonly ReviewComment[],
  options: { turnName?: TurnNamer } = {},
): string {
  return [
    OPEN,
    PREAMBLE,
    "",
    ...comments.map((comment) => commentBlock(comment, options.turnName)),
    CLOSE,
  ].join("\n");
}

/**
 * The message with its review comments after it, in one block. With no
 * comments it is the message exactly as typed.
 */
export function messageWithReviewComments(
  message: string,
  comments: readonly ReviewComment[],
  options: { turnName?: TurnNamer } = {},
): string {
  if (comments.length === 0) return message;
  const block = reviewCommentsBlock(comments, options);
  const text = message.trim();
  return text ? `${text}\n\n${block}` : block;
}

/** A comment read back out of a sent message. */
export type SentReviewComment = {
  readonly path: string;
  /** The diff it was written on: "working tree", or a turn. */
  readonly diff: string | null;
  /** The new-file span, "12" or "12-14", when the comment has one. */
  readonly lines: string | null;
  /** The old-file span of removed lines, when the comment has any. */
  readonly oldLines: string | null;
  /** The quoted lines, each with its diff marker. */
  readonly quote: readonly string[];
  /** Lines the comment covered past its quote. */
  readonly unquoted: number;
  readonly outdated: boolean;
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
    const cut = /^first (\d+) of (\d+) lines$/.exec(
      attribute(tag, "quote") ?? "",
    );
    comments.push({
      path: attribute(tag, "path") ?? "",
      diff: attribute(tag, "diff"),
      lines: attribute(tag, "lines"),
      oldLines: attribute(tag, "old_lines"),
      quote,
      unquoted: cut ? Math.max(0, Number(cut[2]) - Number(cut[1])) : 0,
      outdated: attribute(tag, "outdated") === "true",
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

function spanStart(value: string | null): number | null {
  if (!value) return null;
  const first = Number(value.split("-")[0]);
  return Number.isInteger(first) ? first : null;
}

/**
 * Pending comments again, from the block a message carried: what a queued
 * message gives back when it is deleted before it runs. The block keeps the
 * quote and the spans, so the lines are numbered from where the spans start.
 */
export function reviewCommentsFromSent(
  sent: readonly SentReviewComment[],
  options: {
    /** The turn a block's `diff` names, when it names one this app knows. */
    turnFor?: (diff: string) => string | null;
    newId?: () => string;
    now?: () => string;
  } = {},
): ReviewComment[] {
  const newId = options.newId ?? (() => crypto.randomUUID());
  const now = options.now ?? (() => new Date().toISOString());
  return sent.flatMap((comment): ReviewComment[] => {
    let nextNew = spanStart(comment.lines);
    let nextOld = spanStart(comment.oldLines);
    let seenRemoved = false;
    const lines = comment.quote.flatMap((quoted): ReviewCommentLine[] => {
      const text = quoted.slice(1);
      switch (quoted[0]) {
        case "-": {
          seenRemoved = true;
          const oldNo = nextOld;
          if (nextOld !== null) nextOld += 1;
          return [{ kind: "del", oldNo, newNo: null, text }];
        }
        case "+": {
          const newNo = nextNew;
          if (nextNew !== null) nextNew += 1;
          return [{ kind: "add", oldNo: null, newNo, text }];
        }
        case " ": {
          const newNo = nextNew;
          if (nextNew !== null) nextNew += 1;
          // Old numbers are only known from the first removed line on.
          const oldNo = seenRemoved ? nextOld : null;
          if (seenRemoved && nextOld !== null) nextOld += 1;
          return [{ kind: "context", oldNo, newNo, text }];
        }
        default:
          return [];
      }
    });
    if (lines.length === 0) return [];
    const turnId =
      comment.diff && comment.diff !== WORKING_TREE
        ? (options.turnFor?.(comment.diff) ?? null)
        : null;
    return [
      {
        id: newId(),
        author: { kind: "person" },
        path: comment.path,
        ...(turnId ? { turnId } : {}),
        lines,
        ...(comment.unquoted > 0
          ? {
              unquoted: comment.unquoted,
              span: { lines: comment.lines, oldLines: comment.oldLines },
            }
          : {}),
        ...(comment.outdated ? { outdated: true } : {}),
        body: comment.body,
        createdAt: now(),
      },
    ];
  });
}
