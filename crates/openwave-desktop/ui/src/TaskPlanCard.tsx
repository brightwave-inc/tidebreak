import { useId, useRef, useState } from "react";
import { Circle, CircleCheck, CircleDashed, ChevronDown } from "lucide-react";

import type { TaskPlan, TaskPlanStepStatus } from "./api";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

export type TaskPlanCardProps = {
  /** The chat's current plan, or `null` when it has none. */
  plan: TaskPlan | null;
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

  // A chat with no plan says nothing at all: an empty checklist above the
  // composer is chrome that describes nothing.
  if (!plan) return null;

  const total = plan.steps.length;
  const completed = plan.steps.filter(
    (step) => step.status === "completed",
  ).length;

  return (
    <section
      className={cn(
        "bg-background w-full max-w-prose overflow-hidden rounded-lg border",
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
        <ol id={bodyId} className="grid gap-1.5 border-t px-2.5 py-2 text-sm">
          {plan.steps.map((step, index) => (
            <TaskPlanRow
              // Steps have no identity of their own — the plan is replaced
              // whole — so position is what they are keyed on.
              key={index}
              content={step.content}
              status={step.status}
              live={live}
            />
          ))}
        </ol>
      )}
    </section>
  );
}

/**
 * One step: a status glyph and the line the agent wrote.
 *
 * Status is carried by the glyph alone. Striking a finished step through
 * trades legibility for decoration, and a plan is read to find out what is
 * left rather than to admire what is done.
 */
function TaskPlanRow({
  content,
  status,
  live,
}: {
  content: string;
  status: TaskPlanStepStatus;
  live: boolean;
}) {
  const working = status === "in_progress" && live;
  return (
    <li className="flex items-start gap-2">
      <span className="mt-0.5 shrink-0" aria-hidden="true">
        <StepGlyph status={status} live={live} />
      </span>
      <span
        className={cn(
          "min-w-0",
          working ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {content}
      </span>
      <span className="sr-only">{statusLabel(status, live)}</span>
    </li>
  );
}

function StepGlyph({
  status,
  live,
}: {
  status: TaskPlanStepStatus;
  live: boolean;
}) {
  if (status === "completed") {
    return <CircleCheck className="text-success size-4" />;
  }
  if (status === "in_progress") {
    // A spinner on a turn that is over would animate a claim that nothing is
    // making true. The step still reads as started rather than untouched, but
    // it reads as stopped.
    return live ? (
      <Spinner className="text-foreground size-4" />
    ) : (
      <CircleDashed className="text-muted-foreground size-4" />
    );
  }
  return <Circle className="text-muted-foreground/60 size-4" />;
}

function statusLabel(status: TaskPlanStepStatus, live: boolean): string {
  switch (status) {
    case "completed":
      return "Done";
    case "in_progress":
      return live ? "In progress" : "Unfinished";
    case "pending":
      return live ? "To do" : "Not started";
  }
}
