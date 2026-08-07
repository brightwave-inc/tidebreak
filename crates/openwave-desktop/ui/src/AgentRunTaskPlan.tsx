import { useEffect, useState } from "react";

import type { AgentRun, AgentRunTaskPlan } from "./api";
import { TaskPlanStepList } from "./TaskPlanSteps";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";

export type AgentRunTaskPlanState = {
  loading: boolean;
  error: boolean;
  loaded: boolean;
  plan: AgentRunTaskPlan | null;
};

/**
 * The full checklist behind a run's progress line, read on demand.
 *
 * Deliberately not its own polling loop: the snapshot poll `useAgentRuns`
 * already runs carries the plan's `updated_at`, so passing that in re-reads
 * the list exactly when it has actually changed, and never while the reader
 * has it closed. Mirrors `useAgentRunActivity`, which observes the run's
 * activity history the same way.
 */
export function useAgentRunTaskPlan(
  runId: string | null,
  updatedAt: string | undefined,
  enabled: boolean,
  loadTaskPlan: (runId: string) => Promise<AgentRunTaskPlan | null>,
): AgentRunTaskPlanState {
  const [state, setState] = useState<AgentRunTaskPlanState>({
    loading: false,
    error: false,
    loaded: false,
    plan: null,
  });

  useEffect(() => {
    if (!enabled || runId === null) return;
    let cancelled = false;
    setState((current) => ({
      ...current,
      loading: !current.loaded,
      error: false,
    }));
    loadTaskPlan(runId)
      .then((plan) => {
        if (!cancelled) {
          setState({ loading: false, error: false, loaded: true, plan });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setState((current) => ({ ...current, loading: false, error: true }));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, runId, updatedAt, loadTaskPlan]);

  return state;
}

/**
 * How far a background run has got, as a status row shows it.
 *
 * Secondary information on a card whose subject is the run: a hairline bar,
 * the count, and the step being worked on — no heading, and nothing that
 * competes with the task and status above it. The bar is the part that only
 * makes sense while the work is moving, so a settled run keeps the count and
 * drops it.
 */
export function AgentRunTaskPlanProgress({
  run,
  live,
  className,
}: {
  run: AgentRun;
  live: boolean;
  className?: string;
}) {
  const progress = run.task_plan;
  if (!progress || progress.total === 0) return null;

  const completed = Math.min(progress.completed, progress.total);
  const percentage = Math.round((completed / progress.total) * 100);

  return (
    <div className={cn("flex min-w-0 flex-col gap-1", className)}>
      <div className="flex min-w-0 items-center gap-2">
        {live && (
          <Progress
            value={percentage}
            className="h-1 max-w-40 flex-1 bg-muted"
            aria-label="Task plan progress"
          />
        )}
        <span
          className={cn(
            "shrink-0 text-xs tabular-nums text-muted-foreground",
            // With no bar beside it the count would otherwise float in the
            // middle of the row.
            !live && "order-first",
          )}
        >
          {completed}/{progress.total} steps
        </span>
      </div>
      {/* The current step is what the run is doing right now, so it goes when
          the run does: on a settled run it would name a step that stopped. */}
      {live && progress.current !== null && (
        <p className="min-w-0 truncate text-xs text-muted-foreground">
          {progress.current}
        </p>
      )}
    </div>
  );
}

/**
 * The run's checklist in full, in the same step vocabulary the conversation's
 * own plan uses.
 *
 * A run that has stopped never shows a spinner on the step it was working —
 * liveness here is the run's own status, not any turn's.
 */
export function AgentRunTaskPlanChecklist({
  state,
  live,
  className,
}: {
  state: AgentRunTaskPlanState;
  live: boolean;
  className?: string;
}) {
  if (state.error) {
    return (
      <p className={cn("text-xs text-muted-foreground", className)} role="status">
        The task plan could not be loaded.
      </p>
    );
  }
  // Nothing while the first read is in flight: the progress line above already
  // says a plan exists and how far it has got, so a skeleton here would only
  // make the row jump.
  if (!state.loaded || state.plan === null) return null;

  return (
    <TaskPlanStepList
      className={cn(
        "grid max-h-48 gap-1.5 overflow-y-auto text-xs",
        className,
      )}
      steps={state.plan.steps}
      live={live}
    />
  );
}
