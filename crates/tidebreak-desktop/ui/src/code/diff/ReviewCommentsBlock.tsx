import { useId, useState } from "react";
import { ChevronDown, ChevronRight, MessageSquareDiff } from "lucide-react";

import { cn } from "@/lib/utils";
import type { SentReviewComment } from "./reviewComments";

/** Comments listed before the block folds the rest behind its disclosure. */
const PREVIEW_COMMENTS = 3;

function spanLabel(comment: SentReviewComment): string {
  if (comment.lines) return `${comment.path}:${comment.lines}`;
  if (comment.oldLines) return `${comment.path}:${comment.oldLines} (deleted)`;
  return comment.path;
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
  const files = new Set(comments.map((comment) => comment.path)).size;
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
        <span className="bg-background inline-flex size-9 shrink-0 items-center justify-center rounded-md">
          <MessageSquareDiff className="size-4" aria-hidden="true" />
        </span>
        <span className="grid min-w-0 flex-1 gap-px">
          <strong className="text-foreground text-xs font-semibold">
            Review comments
          </strong>
          <small className="text-2xs truncate">
            {comments.length} {comments.length === 1 ? "comment" : "comments"}{" "}
            on {files} {files === 1 ? "file" : "files"}
          </small>
        </span>
        <Chevron className="size-4 shrink-0" aria-hidden="true" />
      </button>
      <ul id={bodyId} className="m-0 flex list-none flex-col gap-1 p-0 pl-11">
        {shown.map((comment, index) => (
          <li
            key={`${index}:${comment.path}:${comment.lines ?? comment.oldLines}`}
            className="min-w-0"
          >
            {open ? (
              <div className="flex min-w-0 flex-col gap-1 py-0.5">
                <span className="text-foreground truncate font-mono text-xs">
                  {spanLabel(comment)}
                </span>
                {comment.quote.length > 0 && (
                  <pre className="bg-background text-foreground overflow-x-auto rounded-md px-2 py-1 font-mono text-xs">
                    {comment.quote.map((line, lineIndex) => (
                      <span
                        key={`${lineIndex}:${line}`}
                        className="block whitespace-pre"
                      >
                        {line || " "}
                      </span>
                    ))}
                  </pre>
                )}
                <p className="text-foreground text-sm break-words whitespace-pre-wrap">
                  {comment.body}
                </p>
              </div>
            ) : (
              <p className="flex min-w-0 items-baseline gap-2 text-xs">
                <span className="text-foreground shrink-0 font-mono">
                  {spanLabel(comment)}
                </span>
                <span className="min-w-0 truncate">
                  {comment.body.split("\n")[0]}
                </span>
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
