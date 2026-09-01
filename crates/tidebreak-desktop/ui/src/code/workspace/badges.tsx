import type { ApiClient } from "../../api/client";
import type { Attention } from "../../api/types";
import { AttentionBadge } from "../AttentionBadge";
import { Badge } from "@/components/ui/badge";
import { FOCUS_RING } from "../interactive";
import { cn } from "@/lib/utils";
import { followScrollBehavior } from "@/ChatScroll";
import { useRegisteredCodeSession } from "./CodeSessionPane";

export function SessionAttentionBadge({
  sessionId,
  client,
  fallback,
}: {
  sessionId: string;
  client: ApiClient;
  fallback: Attention | undefined;
}) {
  const store = useRegisteredCodeSession(sessionId, client);
  const live = store((state) => state.attention);
  const attention = live ?? fallback;
  // The lifecycle indicator owns live motion in this header. Keep the
  // attention mark for states that carry separate information.
  if (attention?.state.type === "working") return null;
  return <AttentionBadge compact attention={attention} />;
}

export function PendingApprovalBadge({
  sessionId,
  client,
}: {
  sessionId: string;
  client: ApiClient;
}) {
  const store = useRegisteredCodeSession(sessionId, client);
  // The selector must return a primitive: zustand v5 re-renders whenever the
  // snapshot is not referentially stable, so a fresh array here loops forever.
  const pending = store(
    (state) =>
      state.items.filter(
        (item) => item.kind === "approval" && item.state === "pending",
      ).length,
  );
  if (pending === 0) return null;
  const noun = pending === 1 ? "approval" : "approvals";
  return (
    <button
      type="button"
      data-testid="pending-approval-badge"
      // The count alone names a state, not a control. This button scrolls the
      // transcript to the first parked approval, so its name says so.
      aria-label={`Jump to ${pending} pending ${noun}`}
      className={cn(
        "cursor-pointer rounded-full border-0 bg-transparent p-0",
        FOCUS_RING,
      )}
      onClick={() => {
        document
          .querySelector('[data-code-approval-state="pending"]')
          ?.scrollIntoView({
            block: "nearest",
            behavior: followScrollBehavior(false),
          });
      }}
    >
      <Badge variant="warning" size="sm" className="tabular-nums">
        {pending} {noun}
      </Badge>
    </button>
  );
}
