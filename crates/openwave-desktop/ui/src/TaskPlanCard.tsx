import { useId, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";

import type { TaskPlan } from "./api";
import { TaskPlanStepList } from "./TaskPlanSteps";
import { cn } from "@/lib/utils";

export type TaskPlanCardProps = {
  /**
   * The chat's plan. A chat without one renders no card at all, which the
   * caller decides — there is no such thing as an empty plan to draw.
   */
  plan: TaskPlan;
  /**
   * Whether the turn that wrote this plan is still running.
   *
   * A plan outlives its turn — it stays on screen as the record of what the
   * agent set out to do — so nothing about a settled plan may keep claiming
   * that work is under way.
   */
  live: boolean;
};

/**
 * The checklist the agent keeps for a long turn.
 *
 * It sits between the transcript and the composer rather than in the
 * transcript, because the whole point of it is to still be readable an hour
 * into a turn that has scrolled several screens of activity past it. Open
 * while its turn runs, folded to one line once the turn ends: a finished plan
 * is history worth keeping, not something to keep looking at.
 */
export function TaskPlanCard({ plan, live }: TaskPlanCardProps) {
  const [expanded, setExpanded] = useState(live);
  const bodyId = useId();

  // Liveness drives the fold, and a toggle in between is the reader's to keep
  // until liveness itself changes. Adjusted during render rather than in an
  // effect so a turn ending does not paint the open card once before folding
  // it.
  const lastLiveRef = useRef(live);
  if (lastLiveRef.current !== live) {
    lastLiveRef.current = live;
    setExpanded(live);
  }

  const total = plan.steps.length;
  const completed = plan.steps.filter(
    (step) => step.status === "completed",
  ).length;

  return (
    <section
      className={cn(
        "bg-background mx-auto w-full max-w-3xl overflow-hidden rounded-lg border",
        !live && "opacity-80",
      )}
      aria-label="Task plan"
    >
      <button
        type="button"
        className="hover:bg-muted/50 focus-visible:ring-ring flex w-full items-center justify-between gap-2 px-2.5 py-1.5 text-left transition-colors focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-hidden"
        aria-expanded={expanded}
        aria-controls={bodyId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <ChevronDown
            className={cn(
              "size-3.5 shrink-0 transition-transform",
              !expanded && "-rotate-90",
            )}
            aria-hidden="true"
          />
          <span className="truncate">Task plan</span>
        </span>
        <span className="text-muted-foreground shrink-0 text-xs tabular-nums">
          {completed}/{total}
        </span>
      </button>
      {expanded && (
        // Capped and scrolled rather than allowed to grow: the card opens
        // itself when a turn goes live, so a twenty-step plan would otherwise
        // push the composer out of a pane that cannot scroll. The cap shows
        // most of a plan at a glance and leaves the rest a scroll away.
        <TaskPlanStepList
          id={bodyId}
          className="grid max-h-64 gap-1.5 overflow-y-auto border-t px-2.5 py-2 text-sm"
          steps={plan.steps}
          live={live}
        />
      )}
    </section>
  );
}
