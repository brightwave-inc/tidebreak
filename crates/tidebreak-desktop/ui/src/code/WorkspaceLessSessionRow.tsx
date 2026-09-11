import { recoveryDigest } from "./sessionRecovery";
import { useRecoveryDelay } from "./useRecoveryDelay";
import type { CodeSessionDigest } from "../api/types";
import { cn } from "../lib/utils";
import { FOCUS_RING_INSET, HOVER_TINT } from "./interactive";
import { SessionStateGlyph } from "./WorkspaceCard";
import { sessionRowLabel } from "./workspaceCards";

/** An index row for a conversation that has no repository workspace. */
export function WorkspaceLessSessionRow({
  digest,
  onOpen,
  active = false,
  density = "detailed",
}: {
  digest: CodeSessionDigest;
  active?: boolean;
  density?: "compact" | "detailed";
  onOpen: (sessionId: string) => void;
}) {
  digest = recoveryDigest(digest);
  const origin = digest.external_origin;
  const channel = origin?.external_key.split("/")[1];
  const source =
    origin?.channel_kind === "slack"
      ? channel?.startsWith("D")
        ? "Slack direct message"
        : "Slack channel"
      : origin?.channel_kind;
  const title =
    digest.title?.trim() ||
    (source ? `${source} conversation` : "Untitled conversation");
  const recovering = digest.attention.state.type === "fenced";
  const showRecovery = useRecoveryDelay(recovering);
  const status = recovering && !showRecovery ? "" : sessionRowLabel(digest);
  return (
    <button
      type="button"
      className={cn(
        "flex w-full min-w-0 cursor-pointer items-start gap-2 rounded-xl px-2.5 py-2 text-left hover:bg-muted",
        FOCUS_RING_INSET,
        HOVER_TINT,
        active && "bg-muted",
      )}
      aria-label={[title, source, status].filter(Boolean).join(", ")}
      aria-current={active ? "page" : undefined}
      onClick={() => onOpen(digest.session)}
    >
      <span className="mt-1 shrink-0">
        <SessionStateGlyph digest={digest} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-md font-medium leading-5" title={title}>
          {title}
        </span>
        {density === "detailed" && (
          <span
            className="truncate text-xs text-muted-foreground"
            title={[source, status].filter(Boolean).join(" · ")}
          >
            {[source, status].filter(Boolean).join(" · ")}
          </span>
        )}
      </span>
    </button>
  );
}
