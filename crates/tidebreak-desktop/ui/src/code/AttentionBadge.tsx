import { Ban, CircleAlert, CircleCheck, Clock, Pin } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { Attention } from "../api/types";
import { attentionLabel, attentionTooltip } from "./labels";
import {
  attentionStatusTone,
  STATUS_MARK,
  type StatusTone,
} from "./statusTone";

/**
 * Status affordance for a server-computed attention state.
 *
 * NeedsYou is strongest. Stalled and Fenced are distinct warnings.
 * DoneUnreviewed is a quiet mark.
 *
 * Working is a spinner but not a pill. Compact surfaces use a distinct glyph
 * for each state, so the shape still carries the meaning when color is subtle
 * or unavailable. The full badge always sits beside text that names the state
 * already, so a "Working" pill there would only repeat it.
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
        className={cn("inline-flex size-3 shrink-0 items-center", className)}
        // A glyph with no text is only a state if something says which one.
        // `img` is the role that gives the wrapper an accessible name while
        // the inner icon stays decorative.
        role="img"
        aria-label={label}
        title={tooltip}
        data-attention={attention.state.type}
      >
        <CompactAttentionMark attention={attention} tone={tone} />
      </span>
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

function CompactAttentionMark({
  attention,
  tone,
}: {
  attention: Attention;
  tone: StatusTone;
}) {
  const className = cn("size-3", STATUS_MARK[tone]);
  switch (attention.state.type) {
    case "working":
      return <Spinner className={className} aria-hidden="true" />;
    case "needs_you":
      return <CircleAlert className={className} aria-hidden="true" />;
    case "stalled":
      return <Clock className={className} aria-hidden="true" />;
    case "fenced":
      return <Ban className={className} aria-hidden="true" />;
    case "done_unreviewed":
      return <CircleCheck className={className} aria-hidden="true" />;
    case "manual":
      return <Pin className={className} aria-hidden="true" />;
  }
}
