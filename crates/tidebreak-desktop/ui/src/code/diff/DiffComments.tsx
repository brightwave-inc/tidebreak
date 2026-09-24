import { useId, useState, type KeyboardEvent } from "react";
import { Pencil, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { usesCommandModifier } from "@/ShellShortcuts";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import {
  commentLinesLabel,
  type ReviewComment,
  type ReviewCommentLine,
} from "./reviewComments";

const SUBMIT_CHORD =
  typeof navigator !== "undefined" && usesCommandModifier(navigator.userAgent)
    ? "⌘↩"
    : "Ctrl+↩";

/**
 * The frame a comment or its editor sits in, inside the diff. It stays in
 * view when the code scrolls sideways: `--diff-viewport` is the diff's
 * visible width, which the view measures.
 */
const COMMENT_FRAME =
  "sticky left-0 box-border w-[var(--diff-viewport,100%)] max-w-full px-2 py-1.5 font-sans";

/** Writing a new comment on the picked lines, or rewriting one. */
export function CommentComposer({
  lines,
  initial = "",
  submitLabel,
  onSubmit,
  onCancel,
}: {
  lines: readonly ReviewCommentLine[];
  initial?: string;
  submitLabel: string;
  onSubmit: (body: string) => void;
  onCancel: () => void;
}) {
  const [body, setBody] = useState(initial);
  const id = useId();
  const hintId = `${id}-hint`;
  const ready = body.trim().length > 0;

  function submit() {
    if (ready) onSubmit(body.trim());
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submit();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      // The diff's own Escape would drop the selection under the editor.
      event.stopPropagation();
      onCancel();
    }
  }

  return (
    <div className={COMMENT_FRAME} data-diff-comment="editor">
      <div className="bg-background border-border flex flex-col gap-2 rounded-lg border p-2.5">
        <label htmlFor={id} className="text-foreground text-xs font-medium">
          Comment on {commentLinesLabel(lines).toLowerCase()}
        </label>
        <Textarea
          id={id}
          autoFocus
          rows={3}
          value={body}
          aria-describedby={hintId}
          placeholder="What should change here?"
          className="min-h-18 resize-y text-sm"
          onChange={(event) => setBody(event.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p id={hintId} className="text-muted-foreground text-xs">
            Goes to the agent with your next message. {SUBMIT_CHORD} to save.
          </p>
          <div className="flex items-center gap-1.5">
            <Button type="button" variant="ghost" size="xs" onClick={onCancel}>
              Cancel
            </Button>
            <Button type="button" size="xs" disabled={!ready} onClick={submit}>
              {submitLabel}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * One pending comment under the lines it is about. A comment whose lines
 * the diff no longer shows carries its own quote, so it still reads.
 */
export function CommentCard({
  comment,
  sending,
  showQuote = false,
  onEdit,
  onDelete,
}: {
  comment: ReviewComment;
  sending: boolean;
  showQuote?: boolean;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  const label = commentLinesLabel(comment.lines);
  const headingId = useId();
  return (
    <div className={COMMENT_FRAME} data-diff-comment="pending">
      <article
        aria-labelledby={headingId}
        aria-busy={sending || undefined}
        className="bg-background border-border flex flex-col gap-1 rounded-lg border px-2.5 py-2"
      >
        <header className="flex min-w-0 items-center gap-2">
          <span
            id={headingId}
            className="text-foreground min-w-0 truncate text-xs font-medium"
          >
            {label}
          </span>
          <span className="text-muted-foreground flex min-w-0 items-center gap-1 truncate text-xs">
            {sending ? (
              <>
                <Spinner className="size-3" aria-hidden />
                Sending with your message…
              </>
            ) : (
              "Goes with your next message"
            )}
          </span>
          {!sending && (onEdit || onDelete) && (
            <span className="ml-auto flex shrink-0 items-center gap-0.5">
              {onEdit && (
                <CardAction
                  label={`Edit the comment on ${label.toLowerCase()}`}
                  onClick={onEdit}
                >
                  <Pencil className="size-3" aria-hidden />
                  Edit
                </CardAction>
              )}
              {onDelete && (
                <CardAction
                  label={`Delete the comment on ${label.toLowerCase()}`}
                  onClick={onDelete}
                >
                  <Trash2 className="size-3" aria-hidden />
                  Delete
                </CardAction>
              )}
            </span>
          )}
        </header>
        {showQuote && <CommentQuote lines={comment.lines} />}
        <p className="text-foreground text-sm break-words whitespace-pre-wrap">
          {comment.body}
        </p>
      </article>
    </div>
  );
}

function CardAction({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      className={cn(
        "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 text-xs",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

const QUOTE_MARKER = { add: "+", del: "-", context: " " } as const;

/** The quoted lines, compact, for a comment shown away from its rows. */
export function CommentQuote({
  lines,
  max = 6,
}: {
  lines: readonly ReviewCommentLine[];
  max?: number;
}) {
  const shown = lines.slice(0, max);
  const rest = lines.length - shown.length;
  return (
    <pre className="bg-muted text-foreground overflow-x-auto rounded-md px-2 py-1 font-mono text-xs">
      {shown.map((line, index) => (
        <span key={`${index}:${line.text}`} className="block whitespace-pre">
          <span className="text-muted-foreground select-none">
            {QUOTE_MARKER[line.kind]}
          </span>
          {line.text || " "}
        </span>
      ))}
      {rest > 0 && (
        <span className="text-muted-foreground block font-sans">
          {rest} more {rest === 1 ? "line" : "lines"}
        </span>
      )}
    </pre>
  );
}
