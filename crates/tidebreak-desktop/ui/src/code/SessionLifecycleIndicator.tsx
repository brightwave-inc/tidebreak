import { CircleAlert } from "lucide-react";

import { Spinner } from "@/components/ui/spinner";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import type { CodeSessionSnapshot } from "../api/types";
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
}: {
  lifecycle: CodeSessionSnapshot["lifecycle"];
  harness: CodeSessionSnapshot["harness_kind"];
  version?: string;
  unrecognizedEventCount: number;
  runningLabel?: string;
}) {
  const tooltip = sessionLifecycleTooltip({
    lifecycle,
    harness,
    version,
    unrecognizedEventCount,
    runningLabel,
  });
  const label =
    lifecycle === "running" && runningLabel
      ? runningLabel
      : LIFECYCLE_LABELS[lifecycle];
  const unrecognizedLabel = `${unrecognizedEventCount} unrecognized engine ${unrecognizedEventCount === 1 ? "event" : "events"} recorded in this session`;

  return (
    <WithTooltip label={tooltip}>
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
        {lifecycle === "running" && (
          <Spinner
            className={cn("size-3", STATUS_MARK.running)}
            aria-hidden="true"
          />
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
