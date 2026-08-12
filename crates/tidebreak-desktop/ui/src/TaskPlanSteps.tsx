import { Circle, CircleCheck, CircleDashed } from "lucide-react";

import type { TaskPlanStep, TaskPlanStepStatus } from "./api";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

/**
 * How a checklist's steps are drawn, in one place.
 *
 * Two surfaces render a task plan — the conversation's own, above the
 * composer, and a background run's, on its card — and they agree on more than
 * they differ on: the glyph vocabulary, the wrapping, the muting, and what a
 * step says to a screen reader. Only the frame around the list differs, so
 * that is all each caller supplies.
 */

/**
 * One step: a status glyph and the line the agent wrote.
 *
 * Status is carried by the glyph alone. Striking a finished step through
 * trades legibility for decoration, and a plan is read to find out what is
 * left rather than to admire what is done.
 */
export function TaskPlanStepRow({
  content,
  status,
  live,
}: {
  content: string;
  status: TaskPlanStepStatus;
  /**
   * Whether the work this plan describes is still under way — a running turn,
   * or a running background run. A settled plan stays readable as the record
   * of what was attempted, but nothing in it may still claim to be moving.
   */
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
          // The line is the agent's own text, up to 500 characters of it and
          // not necessarily with a space in them. It wraps rather than being
          // clipped away by the card's own overflow.
          "min-w-0 break-words",
          working ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {content}
      </span>
      <span className="sr-only">{statusLabel(status, live)}</span>
    </li>
  );
}

/**
 * The ordered list of steps. The caller owns the frame — its padding, its
 * height cap, and whatever it is nested in — because that is the only part
 * the two surfaces disagree about.
 */
export function TaskPlanStepList({
  steps,
  live,
  id,
  className,
}: {
  steps: readonly TaskPlanStep[];
  live: boolean;
  id?: string;
  className?: string;
}) {
  return (
    <ol id={id} className={className}>
      {steps.map((step, index) => (
        <TaskPlanStepRow
          // Steps have no identity of their own — the plan is replaced
          // whole — so position is what they are keyed on.
          key={index}
          content={step.content}
          status={step.status}
          live={live}
        />
      ))}
    </ol>
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
    // A spinner on work that has stopped would animate a claim that nothing is
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
