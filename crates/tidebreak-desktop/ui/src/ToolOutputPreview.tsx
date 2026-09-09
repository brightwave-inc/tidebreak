import { useId, useMemo, useState } from "react";

import { ClipboardCopyButton } from "./ClipboardCopyButton";
import { cn } from "@/lib/utils";

/** How much of a tool's output is worth showing before the reader asks. */
export const DEFAULT_COLLAPSED_LINES = 8;

/** Bounded running tail: enough to see progress without growing the row. */
export const RUNNING_COLLAPSED_LINES = 6;

type ToolOutputPreviewProps = {
  /** What the tool printed, verbatim. */
  text: string;
  /** Lines shown before the expander takes over. */
  collapsedLines?: number;
  /**
   * Prefer the end of the stream (live command tail). Ignored once the reader
   * expands the block — expansion always shows the full text.
   */
  followTail?: boolean;
  /**
   * When not following the tail, start the collapsed window at this line.
   * Clamped so a short block still shows up to `collapsedLines` lines.
   */
  startLine?: number;
  /** What this block is, for the copy control and assistive technology. */
  label?: string;
  /**
   * Plain indented output: no surface, no padding box. The expander reads
   * "· · · N more lines" instead of "Show N more lines".
   */
  bare?: boolean;
  /**
   * The reader expanded or collapsed the block. Hosts that follow the tail of
   * a transcript use it to stop, so the block stays where it was clicked.
   */
  onToggle?: () => void;
};

/**
 * A tool's output, clamped to a few lines with the rest one click away.
 *
 * Output is the part of a transcript most likely to be enormous and least
 * likely to be read in full, so the default is a glance: enough lines to see
 * what happened, an honest count of what is hidden, and the whole text on the
 * clipboard whether or not it is expanded. Clamping by line rather than by
 * height keeps the count in the expander truthful — "show 40 more lines" that
 * turns out to be four is worse than no count at all.
 *
 * The block is width-bounded (`min-w-0`, horizontal scroll on long lines) so a
 * single unwrapped command cannot stretch the parent reading column.
 */
export function ToolOutputPreview({
  text,
  collapsedLines = DEFAULT_COLLAPSED_LINES,
  followTail = false,
  startLine = 0,
  label = "Output",
  bare = false,
  onToggle,
}: ToolOutputPreviewProps) {
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  // A trailing newline is punctuation, not a line worth offering to expand.
  const lines = useMemo(() => text.replace(/\n+$/, "").split("\n"), [text]);
  const hiddenCount = Math.max(0, lines.length - collapsedLines);
  const firstLine = collapsedWindowStart(
    lines.length,
    collapsedLines,
    followTail,
    startLine,
  );
  const body =
    hiddenCount > 0 && !expanded
      ? lines.slice(firstLine, firstLine + collapsedLines).join("\n")
      : lines.join("\n");

  if (text.trim().length === 0) return null;

  return (
    <div className="flex w-full min-w-0 flex-col items-start gap-1">
      <div className="group relative w-full min-w-0">
        <pre
          id={bodyId}
          // A bare `aria-label` on a `pre` names nothing: the element is
          // generic. The group role is what carries the label — and the copy
          // control beside it uses the same word.
          role="group"
          aria-label={label}
          className={cn(
            bare
              ? "text-muted-foreground max-h-80 overflow-auto pr-7 font-mono text-sm whitespace-pre [overflow-anchor:none]"
              : "bg-muted text-muted-foreground overflow-x-auto rounded-md p-2 pr-9 font-mono text-xs break-words whitespace-pre-wrap",
            bare && followTail && !expanded && "h-[6em]",
          )}
        >
          {body}
        </pre>
        <ClipboardCopyButton
          value={text}
          label={`Copy ${label.toLowerCase()}`}
          copiedAnnouncement={`${label} copied to clipboard.`}
          failedAnnouncement={`${label} could not be copied.`}
          className={
            bare
              ? "text-muted-foreground hover:text-foreground absolute top-0 right-0 inline-flex items-center p-0.5 opacity-0 transition-opacity duration-[140ms] ease-out group-focus-within:opacity-100 group-hover:opacity-100 focus-visible:opacity-100 motion-reduce:transition-none"
              : "border-border bg-background text-muted-foreground hover:text-foreground absolute top-1 right-1 inline-flex items-center rounded-md border p-1 opacity-0 transition-opacity duration-150 group-focus-within:opacity-100 group-hover:opacity-100 focus-visible:opacity-100 motion-reduce:transition-none"
          }
        />
      </div>
      {hiddenCount > 0 && (
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground ring-offset-background focus-visible:ring-ring cursor-pointer rounded-sm text-xs focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none motion-safe:transition-colors motion-safe:duration-[140ms] motion-safe:ease-out"
          aria-expanded={expanded}
          aria-controls={bodyId}
          onClick={() => {
            onToggle?.();
            setExpanded((current) => !current);
          }}
        >
          {expanded
            ? "Show less"
            : bare
              ? `· · · ${hiddenCount} more line${hiddenCount === 1 ? "" : "s"}`
              : `Show ${hiddenCount} more line${hiddenCount === 1 ? "" : "s"}`}
        </button>
      )}
    </div>
  );
}

/** First line index for the collapsed window. Exported for unit tests. */
export function collapsedWindowStart(
  lineCount: number,
  collapsedLines: number,
  followTail: boolean,
  startLine: number,
): number {
  if (lineCount <= collapsedLines) return 0;
  if (followTail) return lineCount - collapsedLines;
  return Math.max(0, Math.min(startLine, lineCount - collapsedLines));
}
