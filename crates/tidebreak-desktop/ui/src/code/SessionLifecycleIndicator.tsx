import { useRecoveryDelay } from "./useRecoveryDelay";
import { CircleAlert } from "lucide-react";

import { Loader } from "@/components/motion/loader";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import type { Attention, CodeSessionSnapshot } from "../api/types";
import { FOCUS_RING } from "./interactive";
import { LIFECYCLE_LABELS, sessionLifecycleTooltip } from "./labels";
import { STATUS_MARK } from "./statusTone";

/**
 * The lifecycle text shown in the code workspace header.
 *
 * Motion marks work that is happening now. A trailing warning icon marks
 * engine events that the adapter could not classify. The warning stays
 * separate from the lifecycle mark because it describes transcript fidelity,
 * not whether the session is running or stopped.
 */
export function SessionLifecycleIndicator({
  lifecycle,
  harness,
  version,
  unrecognizedEventCount,
  runningLabel,
  attention,
}: {
  lifecycle: CodeSessionSnapshot["lifecycle"];
  harness: CodeSessionSnapshot["harness_kind"];
  version?: string;
  unrecognizedEventCount: number;
  runningLabel?: string;
  attention?: Attention;
}) {
  const recovering =
    lifecycle === "fenced" && attention?.state.type === "fenced";
  const showRecovery = useRecoveryDelay(recovering);
  const tooltip = sessionLifecycleTooltip({
    lifecycle,
    harness,
    version,
    unrecognizedEventCount,
    runningLabel,
  });
  const label =
    lifecycle === "fenced" && attention?.state.type === "needs_you"
      ? "Needs you"
      : lifecycle === "running" && runningLabel
        ? runningLabel
        : LIFECYCLE_LABELS[lifecycle];
  const unrecognizedLabel = `${unrecognizedEventCount} unrecognized engine ${unrecognizedEventCount === 1 ? "event" : "events"} recorded in this session`;

  if (recovering && !showRecovery) return null;
  return (
    <WithTooltip
      label={
        attention?.state.type === "needs_you" && lifecycle === "fenced"
          ? attention.state.prompt
          : tooltip
      }
    >
      {/*
       * The tooltip carries the engine version and the dropped-event warning,
       * and neither is anywhere else on the page. A span cannot be tabbed to,
       * so the tab stop keeps the explanation available without a pointer.
       */}
      <span
        tabIndex={0}
        className={cn(
          "inline-flex items-center gap-1.5 rounded-sm text-xs text-muted-foreground",
          FOCUS_RING,
        )}
      >
        {(lifecycle === "running" || recovering) && (
          <Loader variant="comet" size={12} className="text-live" decorative />
        )}
        <span>{label}</span>
        {unrecognizedEventCount > 0 && (
          <CircleAlert
            data-testid="unrecognized-event-indicator"
            className={cn("size-3 shrink-0", STATUS_MARK.warning)}
            role="img"
            aria-label={unrecognizedLabel}
          />
        )}
      </span>
    </WithTooltip>
  );
}
