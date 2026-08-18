import { useId, useMemo, useState } from "react";

import { ClipboardCopyButton } from "./ClipboardCopyButton";

/** How much of a tool's output is worth showing before the reader asks. */
export const DEFAULT_COLLAPSED_LINES = 8;

type ToolOutputPreviewProps = {
  /** What the tool printed, verbatim. */
  text: string;
  /** Lines shown before the expander takes over. */
  collapsedLines?: number;
  /** What this block is, for the copy control and assistive technology. */
  label?: string;
};

/**
 * A tool's output, clamped to the first few lines with the rest one click away.
 *
 * Output is the part of a transcript most likely to be enormous and least
 * likely to be read in full, so the default is a glance: enough lines to see
 * what happened, an honest count of what is hidden, and the whole text on the
 * clipboard whether or not it is expanded. Clamping by line rather than by
 * height keeps the count in the expander truthful — "show 40 more lines" that
 * turns out to be four is worse than no count at all.
 */
export function ToolOutputPreview({
  text,
  collapsedLines = DEFAULT_COLLAPSED_LINES,
  label = "Output",
}: ToolOutputPreviewProps) {
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  // A trailing newline is punctuation, not a line worth offering to expand.
  const lines = useMemo(() => text.replace(/\n+$/, "").split("\n"), [text]);
  const hiddenCount = Math.max(0, lines.length - collapsedLines);
  const body =
    hiddenCount > 0 && !expanded
      ? lines.slice(0, collapsedLines).join("\n")
      : lines.join("\n");

  if (text.trim().length === 0) return null;

  return (
    <div className="flex flex-col items-start gap-1">
      <div className="group relative w-full">
        <pre
          id={bodyId}
          aria-label={label}
          className="bg-muted text-muted-foreground overflow-x-auto rounded-md p-2 pr-9 font-mono text-[13px] break-words whitespace-pre-wrap [overflow-anchor:none]"
        >
          {body}
        </pre>
        <ClipboardCopyButton
          value={text}
          label={`Copy ${label.toLowerCase()}`}
          copiedAnnouncement={`${label} copied to clipboard.`}
          failedAnnouncement={`${label} could not be copied.`}
          className="border-border bg-background text-muted-foreground hover:text-foreground absolute top-1 right-1 inline-flex items-center rounded-md border p-1 opacity-0 transition-opacity duration-[140ms] ease-out group-focus-within:opacity-100 group-hover:opacity-100 focus-visible:opacity-100 motion-reduce:transition-none"
        />
      </div>
      {hiddenCount > 0 && (
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground text-xs underline-offset-2 hover:underline"
          aria-expanded={expanded}
          aria-controls={bodyId}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded
            ? "Show less"
            : `Show ${hiddenCount} more line${hiddenCount === 1 ? "" : "s"}`}
        </button>
      )}
    </div>
  );
}
