import { History } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/notice";

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
    <Notice
      tone="info"
      icon={History}
      className="mt-2"
      action={
        <Button type="button" size="sm" variant="outline" onClick={onLeave}>
          Back to latest
        </Button>
      }
    >
      {label}
    </Notice>
  );
}
