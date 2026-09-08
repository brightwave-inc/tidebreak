import type { CodeSessionDigest } from "../api/types";
import { cn } from "../lib/utils";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { SessionStateGlyph } from "./WorkspaceCard";
import { sessionRowLabel } from "./workspaceCards";

/** An index row for a conversation that has no repository workspace. */
export function WorkspaceLessSessionRow({
  digest,
  onOpen,
}: {
  digest: CodeSessionDigest;
  onOpen: (sessionId: string) => void;
}) {
  const title = digest.title?.trim() || "Untitled conversation";
  const status = sessionRowLabel(digest);
  return (
    <button
      type="button"
      className={cn(
        "flex w-full min-w-0 cursor-pointer items-start gap-2 rounded-md px-2.5 py-2 text-left hover:bg-muted",
        FOCUS_RING,
        HOVER_TINT,
      )}
      aria-label={`${title}, ${status}`}
      onClick={() => onOpen(digest.session)}
    >
      <span className="mt-1 shrink-0">
        <SessionStateGlyph digest={digest} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-sm" title={title}>
          {title}
        </span>
        <span className="truncate text-xs text-muted-foreground" title={status}>
          {status}
        </span>
      </span>
    </button>
  );
}
