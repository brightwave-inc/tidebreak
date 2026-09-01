import { cn } from "@/lib/utils";
import { type CheckCounts, checkSummary } from "./prState";
import { STATUS_TEXT } from "./statusTone";

/**
 * The check rollup as one phrase in its tone: `1 failed`, `2 pending`,
 * `8 passed`, `3 skipped`, or `No checks`. The delivery row and the detail
 * Checks tab both render this component, so the two cannot describe the
 * same counts in different words. The wording itself lives in
 * `checkSummary` in prState.ts.
 */
export function PrCheckSummary({
  counts,
  className,
}: {
  counts: CheckCounts;
  className?: string;
}) {
  const summary = checkSummary(counts);
  return (
    <span className={cn("text-xs", STATUS_TEXT[summary.tone], className)}>
      {summary.label}
    </span>
  );
}
