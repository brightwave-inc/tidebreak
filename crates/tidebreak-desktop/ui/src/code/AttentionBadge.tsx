import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

import type { Attention } from "../api/types";
import { attentionLabel, attentionTooltip } from "./labels";
import {
  attentionStatusTone,
  STATUS_DOT,
  STATUS_MOTION,
  type StatusTone,
} from "./statusTone";

/**
 * Status affordance for a server-computed attention state.
 *
 * NeedsYou is strongest. Stalled and Fenced are distinct warnings.
 * DoneUnreviewed is a quiet mark.
 *
 * Working is a dot but not a pill. The compact dot is often the only state a
 * row carries, and an agent mid-turn used to draw there as nothing at all —
 * indistinguishable from an idle one, for the state a reader most wants to
 * see. The full badge always sits beside text that names the state already, so
 * a "Working" pill there would only repeat it.
 */

const BADGE_VARIANTS: Record<
  StatusTone,
  "outline" | "success" | "warning" | "critical" | "info" | "merged"
> = {
  neutral: "outline",
  running: "info",
  ready: "success",
  pending: "info",
  warning: "warning",
  critical: "critical",
  merged: "merged",
};

export function AttentionBadge({
  attention,
  compact = false,
  className,
}: {
  attention: Attention | undefined;
  compact?: boolean;
  className?: string;
}) {
  if (!attention) return null;
  if (attention.state.type === "working" && !compact) return null;
  const label = attentionLabel(attention);
  const tooltip = attentionTooltip(attention);
  const tone = attentionStatusTone(attention);
  if (compact) {
    return (
      <span
        className={cn(
          "inline-block size-2 shrink-0 rounded-full",
          STATUS_DOT[tone],
          STATUS_MOTION[tone],
          className,
        )}
        // A dot with no text is only a state if something says which one. On a
        // bare `span` the label would be dropped; `img` is the role that takes
        // a name and has no children to read.
        role="img"
        aria-label={label}
        title={tooltip}
        data-attention={attention.state.type}
      />
    );
  }
  return (
    <Badge
      variant={BADGE_VARIANTS[tone]}
      size="sm"
      className={className}
      aria-label={label}
      title={tooltip}
      data-attention={attention.state.type}
    >
      {label}
    </Badge>
  );
}
