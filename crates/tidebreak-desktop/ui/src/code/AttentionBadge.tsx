import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

import type { Attention } from "../api/types";
import { attentionLabel } from "./labels";

/**
 * Status affordance for a server-computed attention state.
 *
 * NeedsYou is strongest. Stalled and Fenced are distinct warnings.
 * DoneUnreviewed is a quiet mark. Working has no badge.
 */

export function AttentionBadge({
  attention,
  compact = false,
  className,
}: {
  attention: Attention | undefined;
  compact?: boolean;
  className?: string;
}) {
  if (!attention || attention.state.type === "working") return null;
  const label = attentionLabel(attention);
  const tone = attentionTone(attention);
  if (compact) {
    return (
      <span
        className={cn(
          "inline-block size-2 shrink-0 rounded-full",
          tone === "critical" && "bg-critical-foreground-muted",
          tone === "warning" && "bg-warning-foreground-muted",
          tone === "subtle" && "bg-muted-foreground/60",
          tone === "info" && "bg-info-foreground-muted",
          className,
        )}
        aria-label={label}
        title={label}
        data-attention={attention.state.type}
      />
    );
  }
  return (
    <Badge
      variant={
        tone === "critical"
          ? "critical"
          : tone === "warning"
            ? "warning"
            : tone === "info"
              ? "info"
              : "outline"
      }
      size="sm"
      className={className}
      aria-label={label}
      title={label}
      data-attention={attention.state.type}
    >
      {label}
    </Badge>
  );
}

function attentionTone(
  attention: Attention,
): "critical" | "warning" | "subtle" | "info" {
  switch (attention.state.type) {
    case "needs_you":
      return "critical";
    case "stalled":
    case "fenced":
      return "warning";
    case "manual":
      return "info";
    case "done_unreviewed":
      return "subtle";
    default:
      return "subtle";
  }
}
