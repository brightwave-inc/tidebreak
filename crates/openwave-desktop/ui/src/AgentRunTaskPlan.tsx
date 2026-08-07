import { useCallback, useEffect, useState } from "react";

import type { AgentRun, AgentRunTaskPlan } from "./api";
import { TaskPlanStepList } from "./TaskPlanSteps";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";

export type AgentRunTaskPlanState = {
  loading: boolean;
  error: boolean;
  loaded: boolean;
  plan: AgentRunTaskPlan | null;
  /** Read the list again after a failure; the effect alone never retries. */
  retry: () => void;
};

/** The held read, plus the run it is an answer about. */
type HeldTaskPlan = {
  runId: string | null;
  loading: boolean;
  error: boolean;
  loaded: boolean;
  plan: AgentRunTaskPlan | null;
};

const NO_PLAN_HELD: HeldTaskPlan = {
  runId: null,
  loading: false,
  error: false,
  loaded: false,
  plan: null,
};

/**
 * The full checklist behind a run's progress line, read on demand.
 *
 * Deliberately not its own polling loop: the snapshot poll `useAgentRuns`
 * already runs carries the plan's `updated_at`, so passing that in re-reads
 * the list exactly when it has actually changed, and never while the reader
 * has it closed. Mirrors `useAgentRunActivity`, which observes the run's
 * activity history the same way.
 *
 * The held answer remembers which run it is about. A panel that switches runs
 * without remounting would otherwise show the previous run's steps under the
 * new run's heading until the next read lands — a wrong answer rather than a
 * late one.
 *
 * A settled run's `updated_at` never changes again, so nothing would re-run
 * the effect after a failed read. The retry is therefore explicit rather than
 * a matter of waiting.
 */
export function useAgentRunTaskPlan(
  runId: string | null,
  updatedAt: string | undefined,
  enabled: boolean,
  loadTaskPlan: (runId: string) => Promise<AgentRunTaskPlan | null>,
): AgentRunTaskPlanState {
  const [held, setHeld] = useState<HeldTaskPlan>(NO_PLAN_HELD);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!enabled || runId === null) return;
    let cancelled = false;
    setHeld((current) =>
      current.runId === runId
        ? { ...current, loading: !current.loaded, error: false }
        : { ...NO_PLAN_HELD, runId, loading: true },
    );
    loadTaskPlan(runId)
      .then((plan) => {
        if (!cancelled) {
          setHeld({ runId, loading: false, error: false, loaded: true, plan });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setHeld((current) =>
            current.runId === runId
              ? { ...current, loading: false, error: true }
              : current,
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, runId, updatedAt, loadTaskPlan, attempt]);

  const retry = useCallback(() => setAttempt((current) => current + 1), []);

  // Answers about another run are not this run's to show, not even for the
  // one commit before the effect resets them.
  const current = held.runId === runId ? held : NO_PLAN_HELD;
  return {
    loading: current.loading,
    error: current.error,
    loaded: current.loaded,
    plan: current.plan,
    retry,
  };
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
        <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
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
  /**
   * The frame around the list, including its height cap. The caller owns it:
   * these lists are nested inside scrollers of their own, and a cap chosen
   * here would stack a second scrollbar inside the first.
   */
  className?: string;
}) {
  // A failed refresh is not evidence the plan is gone. While a good read is
  // still held it keeps rendering, and the failure costs nothing visible —
  // the next successful read replaces it.
  if (state.error && state.plan === null) {
    return (
      <div
        className="flex items-center justify-between gap-3 text-xs text-muted-foreground"
        role="status"
      >
        <span>The task plan could not be loaded.</span>
        <button
          type="button"
          className="shrink-0 font-medium text-primary hover:underline"
          onClick={state.retry}
        >
          Retry
        </button>
      </div>
    );
  }
  // Nothing while the first read is in flight: the progress line above already
  // says a plan exists and how far it has got, so a skeleton here would only
  // make the row jump.
  if (state.plan === null) return null;

  return (
    <TaskPlanStepList
      className={cn("grid gap-1.5 overflow-y-auto text-xs", className)}
      steps={state.plan.steps}
      live={live}
    />
  );
}
