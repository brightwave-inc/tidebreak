import { useId, useState } from "react";
import { ChevronDown, ChevronRight, MessageSquareDiff } from "lucide-react";

import { cn } from "@/lib/utils";
import { MiddleTruncate } from "../MiddleTruncate";
import { HARNESS_LABELS } from "../labels";
import { SEVERITY_LABEL } from "./DiffComments";
import type { SentReviewComment } from "./reviewComments";

/** A reviewer's mark: the engine that found it, and how much it matters. */
function reviewerLabel(comment: SentReviewComment): string | null {
  if (!comment.reviewer) return null;
  const engine = HARNESS_LABELS[comment.reviewer];
  return comment.severity
    ? `${engine}, ${SEVERITY_LABEL[comment.severity].toLowerCase()}`
    : engine;
}

/** What a comment says first: a reviewer's title, or its opening line. */
function headline(comment: SentReviewComment): string {
  return comment.title ?? comment.body.split("\n")[0] ?? "";
}

/** Comments listed before the block folds the rest behind its disclosure. */
const PREVIEW_COMMENTS = 3;

/**
 * Where a comment points: `path:12-14`, or the old lines of a deletion, and
 * whether its code had changed by the time it was sent.
 */
function spanLabel(comment: SentReviewComment, path = comment.path): string {
  if (comment.general) return "The changes as a whole";
  const where = comment.lines
    ? `${path}:${comment.lines}`
    : comment.oldLines
      ? `${path}:${comment.oldLines} (deleted)`
      : path;
  return comment.outdated ? `${where} (outdated)` : where;
}

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1) || path;
}

/**
 * The diff comments a message carried, as the transcript shows them.
 *
 * The agent read a block of quoted lines and comments; the reader wrote a
 * handful of notes on lines. So the transcript lists the notes, one line
 * each, and keeps the quotes behind the disclosure.
 */
export function ReviewCommentsBlock({
  comments,
}: {
  comments: readonly SentReviewComment[];
}) {
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const files = new Set(
    comments
      .filter((comment) => !comment.general)
      .map((comment) => comment.path),
  ).size;
  const shown = open ? comments : comments.slice(0, PREVIEW_COMMENTS);
  const hidden = comments.length - shown.length;
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <div
      className="border-border bg-muted/50 text-muted-foreground not-prose my-2 flex max-w-full flex-col gap-1.5 rounded-lg border px-2 py-1.5"
      data-testid="review-comments-block"
    >
      <button
        type="button"
        className={cn(
          "flex min-w-0 cursor-pointer items-center gap-2 rounded-md text-left",
          "focus-visible:ring-ring focus-visible:ring-2 focus-visible:outline-none",
        )}
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((current) => !current)}
      >
        {/* A plain mark on the title's line, as the transcript's rows draw
            their icons: no tile behind it. */}
        <MessageSquareDiff
          className="text-muted-foreground size-3.5 shrink-0"
          aria-hidden="true"
        />
        <span className="flex min-w-0 flex-1 items-baseline gap-2">
          <strong className="text-foreground shrink-0 text-xs font-semibold">
            Review comments
          </strong>
          <span className="truncate text-xs">
            {comments.length} {comments.length === 1 ? "comment" : "comments"}{" "}
            on {files} {files === 1 ? "file" : "files"}
          </span>
        </span>
        <Chevron className="size-3.5 shrink-0" aria-hidden="true" />
      </button>
      <ul id={bodyId} className="m-0 flex list-none flex-col gap-1 p-0 pl-5.5">
        {shown.map((comment, index) => (
          <li
            key={`${index}:${comment.path}:${comment.lines ?? comment.oldLines}`}
            className="min-w-0"
          >
            {open ? (
              <div className="flex min-w-0 flex-col gap-1 py-0.5">
                <MiddleTruncate
                  text={spanLabel(comment)}
                  className="text-foreground font-mono text-xs"
                />
                {reviewerLabel(comment) && (
                  <span className="text-xs">{reviewerLabel(comment)}</span>
                )}
                {comment.quote.length > 0 && (
                  // Long quoted lines wrap: a quote that scrolled sideways
                  // would be a scroll region nothing can focus.
                  <pre className="bg-background text-foreground rounded-md px-2 py-1 font-mono text-xs">
                    {comment.quote.map((line, lineIndex) => (
                      <span
                        key={`${lineIndex}:${line}`}
                        className="block [overflow-wrap:anywhere] whitespace-pre-wrap"
                      >
                        {line || " "}
                      </span>
                    ))}
                  </pre>
                )}
                {comment.title && (
                  <p className="text-foreground text-sm font-medium break-words">
                    {comment.title}
                  </p>
                )}
                <p className="text-foreground text-sm break-words whitespace-pre-wrap">
                  {comment.body}
                </p>
              </div>
            ) : (
              // Folded, the file name is enough to tell comments apart and
              // leaves the row to what the comment says.
              <p className="flex min-w-0 items-baseline gap-2 text-xs">
                <span
                  className="text-foreground shrink-0 font-mono"
                  title={spanLabel(comment)}
                >
                  {comment.general
                    ? "Whole change"
                    : spanLabel(comment, fileName(comment.path))}
                </span>
                {reviewerLabel(comment) && (
                  <span className="shrink-0">{reviewerLabel(comment)}</span>
                )}
                <span className="min-w-0 truncate">{headline(comment)}</span>
              </p>
            )}
          </li>
        ))}
        {hidden > 0 && (
          <li className="text-2xs">
            {hidden} more {hidden === 1 ? "comment" : "comments"}
          </li>
        )}
      </ul>
    </div>
  );
}
