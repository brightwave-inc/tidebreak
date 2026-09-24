import { History } from "lucide-react";

import { Button } from "@/components/ui/button";

/**
 * Closes a stretch of earlier history a search opened on its own.
 *
 * The stretch does not run on to the newest messages, so it ends with the
 * way back to them instead of pretending it is the end of the conversation.
 * A notice in the message column, per DESIGN.md: full width, neutral
 * surface, the info edge.
 */
export function EarlierHistoryNotice({
  onLeave,
  label = "This is an earlier part of the conversation. Newer messages are not shown here.",
}: {
  onLeave: () => void;
  label?: string;
}) {
  return (
    <div
      role="status"
      className="notice-surface notice-info mt-2 flex w-full flex-wrap items-center gap-3 rounded-lg border px-3 py-2 text-sm"
    >
      <History
        aria-hidden="true"
        className="size-4 shrink-0 text-muted-foreground"
      />
      <p className="min-w-0 flex-1">{label}</p>
      <Button type="button" size="xs" variant="outline" onClick={onLeave}>
        Back to latest
      </Button>
    </div>
  );
}
