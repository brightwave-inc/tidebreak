import { useEffect, useState } from "react";

import { Badge } from "@/components/ui/badge";
import type { CodeWorkspaceTree } from "../api/types";

type Revision = NonNullable<CodeWorkspaceTree["revision"]>;

/**
 * The title-and-path block of a center pane header that carries this chip.
 *
 * It fills the row, and when the row cannot fit its basis beside the chip
 * and controls, they wrap under it instead of squeezing the path. The path
 * itself keeps its tail through `MiddleTruncate`, with the full text as its
 * title.
 */
export const HEADER_CAPTION = "min-w-0 flex-[1_1_18rem]";

/** How often a live chip re-reads the clock. Checkpoints land about once a minute. */
const TICK_MS = 15_000;

/**
 * Which sandbox checkpoint a remote workspace view was read from.
 *
 * Live means the sandbox that pushed the checkpoint is still running; the
 * supervisor pushes about once a minute, so the chip says how old the view
 * is. Saved checkpoint means the sandbox has stopped and this is the work it
 * left. Host worktrees omit this chip.
 *
 * Files, the changed-file list, and the standalone diff all render this one
 * chip, so a reader never has to guess whether two panes show the same
 * revision.
 */
export function WorkspaceRevisionChip({
  revision,
  revisionRef,
  savedAt,
}: {
  revision?: Revision;
  revisionRef?: string;
  savedAt?: string;
}) {
  const now = useNow(revision === "live" && savedAt !== undefined);
  if (!revision) return null;
  const saved = savedAt === undefined ? undefined : Date.parse(savedAt);
  const age =
    saved === undefined || Number.isNaN(saved)
      ? undefined
      : savedAgo(now - saved);
  const label =
    revision === "live"
      ? age
        ? `Live · saved ${age}`
        : "Live"
      : "Saved checkpoint";
  const title = [
    revision === "live"
      ? "Latest checkpoint from the running sandbox"
      : "Checkpoint the sandbox saved before it stopped",
    revisionRef,
    saved !== undefined && !Number.isNaN(saved)
      ? `Saved ${new Date(saved).toLocaleString()}`
      : undefined,
  ]
    .filter(Boolean)
    .join("\n");
  return (
    <Badge
      variant={revision === "live" ? "live" : "secondary"}
      size="sm"
      className="shrink-0 font-normal tabular-nums"
      title={title}
    >
      {label}
    </Badge>
  );
}

function useNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), TICK_MS);
    return () => window.clearInterval(id);
  }, [ticking]);
  return now;
}

/** "just now", "40s ago", "3m ago", "2h ago", "4d ago". */
export function savedAgo(elapsedMs: number): string {
  const seconds = Math.max(0, Math.floor(elapsedMs / 1_000));
  if (seconds < 10) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}
