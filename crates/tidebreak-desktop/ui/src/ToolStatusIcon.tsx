import { Ban, CircleAlert, Clock } from "lucide-react";

import { Loader } from "@/components/motion/loader";
import { cn } from "@/lib/utils";

export type ToolTone =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "cancelled"
  | "unknown";

/**
 * Renderer-owned status glyph, derived only from the allowlisted presentation
 * tone and never from a provider-supplied tool name or payload.
 *
 * Success renders nothing on purpose. Most calls succeed, so a checkmark on
 * each one is a column of noise that makes the rows that did fail harder to
 * find; the past-tense title already says the work is done.
 */
export function ToolStatusIcon({
  tone,
  className = "size-4",
}: {
  tone: ToolTone;
  className?: string;
}) {
  switch (tone) {
    case "running":
      return (
        <Loader
          variant="comet"
          size={14}
          className={cn(className, "text-live")}
          decorative
        />
      );
    case "waiting_approval":
      return <Clock className={cn("text-muted-foreground", className)} />;
    case "completed":
      return null;
    case "cancelled":
      return <Ban className={cn("text-muted-foreground", className)} />;
    case "failed":
    case "unknown":
      return <CircleAlert className={cn("text-muted-foreground", className)} />;
  }
}
