import {
  ArrowUpRight,
  Check,
  CircleHelp,
  Clock,
  Minus,
  TriangleAlert,
} from "lucide-react";

export type ToolTone =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "cancelled"
  | "unknown";

/**
 * Renderer-owned status glyph for tool cards and activity groups. It is derived
 * only from the allowlisted presentation tone, never from a provider-supplied
 * tool name or payload.
 */
export function ToolStatusIcon({
  tone,
  size = 15,
}: {
  tone: ToolTone;
  size?: number;
}) {
  switch (tone) {
    case "completed":
      return <Check size={size} />;
    case "failed":
      return <TriangleAlert size={size} />;
    case "cancelled":
      return <Minus size={size} />;
    case "waiting_approval":
      return <Clock size={size} />;
    case "running":
      return <ArrowUpRight size={size} />;
    default:
      return <CircleHelp size={size} />;
  }
}
