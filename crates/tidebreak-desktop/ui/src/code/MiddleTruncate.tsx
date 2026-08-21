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
  // Short strings have no interesting tail to protect, and splitting them into
  // two boxes only costs the browser a wrap opportunity.
  if (text.length <= SHORT_ENOUGH) {
    return (
      <span className={cn("block truncate", className)} title={text}>
        {text}
      </span>
    );
  }
  const tail = Math.min(
    TAIL_MAX,
    Math.max(TAIL_MIN, Math.ceil(text.length / 3)),
  );
  return (
    <span className={cn("flex min-w-0", className)} title={text}>
      <span className="truncate">{text.slice(0, -tail)}</span>
      <span className="shrink-0">{text.slice(-tail)}</span>
    </span>
  );
}

const SHORT_ENOUGH = 40;
const TAIL_MIN = 12;
const TAIL_MAX = 28;
