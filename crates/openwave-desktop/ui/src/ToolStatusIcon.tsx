import { Ban, CircleAlert, Clock, Loader2 } from "lucide-react";

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
      return <Loader2 className={`${className} animate-spin`} />;
    case "waiting_approval":
      return <Clock className={`text-muted-foreground ${className}`} />;
    case "completed":
      return null;
    case "cancelled":
      return <Ban className={`text-muted-foreground ${className}`} />;
    case "failed":
    case "unknown":
      return <CircleAlert className={`text-muted-foreground ${className}`} />;
  }
}
