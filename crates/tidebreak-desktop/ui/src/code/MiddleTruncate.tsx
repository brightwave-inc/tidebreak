import { cn } from "@/lib/utils";

/**
 * A command, path, or branch that keeps its tail when the row runs out of room.
 *
 * The unique part of a path is its end, so an end-truncated
 * `crates/tidebreak-server/src/routes/…` tells a reader nothing they did not
 * already know. This keeps a tail proportional to the string and lets the head
 * take whatever space is left, so the same name reads the same way in a 200px
 * rail and a full-width panel — no character cap, no per-site tuning.
 *
 * The full text always rides along as `title`, because every one of these
 * surfaces can truncate to something ambiguous.
 */
export function MiddleTruncate({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  // A command summary is one row even when the command is a heredoc or a
  // multi-line script. Keep the original in the tooltip, but collapse line
  // breaks before splitting the visible text. The tail uses `whitespace-pre`
  // to keep a split-leading space, so an uncollapsed newline there would turn
  // a folded row into several lines.
  const visibleText = text.replace(/[ \t]*[\r\n]+[ \t]*/g, " ");
  // Short strings have no interesting tail to protect, and splitting them into
  // two boxes only costs the browser a wrap opportunity.
  if (visibleText.length <= SHORT_ENOUGH) {
    return (
      <span className={cn("block truncate", className)} title={text}>
        {visibleText}
      </span>
    );
  }
  const tail = Math.min(
    TAIL_MAX,
    Math.max(TAIL_MIN, Math.ceil(visibleText.length / 3)),
  );
  return (
    <span className={cn("flex min-w-0", className)} title={text}>
      <span className="truncate">{visibleText.slice(0, -tail)}</span>
      {/* `whitespace-pre` because the split can land on a space, and a flex
          item drops the whitespace it starts with — which reads as a command
          with two of its words run together. */}
      <span className="shrink-0 whitespace-pre">
        {visibleText.slice(-tail)}
      </span>
    </span>
  );
}

const SHORT_ENOUGH = 40;
const TAIL_MIN = 12;
const TAIL_MAX = 28;
