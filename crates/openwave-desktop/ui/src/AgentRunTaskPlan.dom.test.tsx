// @vitest-environment jsdom
import { cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { AgentRun, AgentRunTaskPlan } from "./api";
import {
  AgentRunTaskPlanChecklist,
  AgentRunTaskPlanProgress,
  useAgentRunTaskPlan,
  type AgentRunTaskPlanState,
} from "./AgentRunTaskPlan";

afterEach(cleanup);

const PLAN: AgentRunTaskPlan = {
  run_id: "run-1",
  updated_at: "2026-08-07T12:00:00Z",
  steps: [
    { content: "Gather the figures", status: "completed" },
    { content: "Write the summary", status: "in_progress" },
  ],
};

const LOADED: AgentRunTaskPlanState = {
  loading: false,
  error: false,
  loaded: true,
  plan: PLAN,
  retry: () => undefined,
};

function run(status: AgentRun["status"]): AgentRun {
  return {
    id: "run-1",
    parent_id: "foreground",
    spawn_call_id: "spawn-call",
    tier: "background",
    execution_location: "in_process",
    status,
    task: "Prepare the report",
    started_at: "2026-08-07T11:00:00Z",
    finished_at: null,
    last_error_code: null,
    activity: null,
    submitted_outputs: [],
    task_plan: {
      completed: 1,
      total: 2,
      current: "Write the summary",
      updated_at: "2026-08-07T12:00:00Z",
    },
    terminal_text: null,
    created_at: "2026-08-07T11:00:00Z",
    updated_at: "2026-08-07T12:00:00Z",
  };
}

describe("a background run's task plan", () => {
  it("drops every claim of work in flight once the run has stopped", () => {
    const live = render(
      <>
        <AgentRunTaskPlanProgress run={run("running")} live={true} />
        <AgentRunTaskPlanChecklist state={LOADED} live={true} />
      </>,
    );
    // Named twice while live: once as the step in flight, once on the row.
    expect(screen.getAllByText("Write the summary")).toHaveLength(2);
    expect(live.container.querySelector(".animate-spin")).not.toBeNull();
    expect(live.container.querySelector("[data-slot='progress']")).not.toBeNull();

    cleanup();

    const settled = render(
      <>
        <AgentRunTaskPlanProgress run={run("failed")} live={false} />
        <AgentRunTaskPlanChecklist state={LOADED} live={false} />
      </>,
    );
    // The record of what the run set out to do survives...
    expect(screen.getByText("1/2 steps")).toBeInTheDocument();
    // ...but the row stops naming a current step, because there isn't one.
    expect(screen.getAllByText("Write the summary")).toHaveLength(1);
    // ...without a spinner or a bar still tracking work that has stopped.
    expect(settled.container.querySelector(".animate-spin")).toBeNull();
    expect(settled.container.querySelector("[data-slot='progress']")).toBeNull();
  });
});

describe("useAgentRunTaskPlan", () => {
  it("never answers about the run it was previously asked about", async () => {
    const plans: Record<string, AgentRunTaskPlan> = {
      "run-1": PLAN,
      "run-2": { ...PLAN, run_id: "run-2", steps: [PLAN.steps[0]!] },
    };
    // Held on an object so the assignment inside the promise is visible to
    // the checker rather than narrowed away.
    const gate: { release: () => void } = { release: () => undefined };
    const loadTaskPlan = async (id: string) => {
      if (id === "run-2") {
        await new Promise<void>((resolve) => {
          gate.release = resolve;
        });
      }
      return plans[id] ?? null;
    };

    const { result, rerender } = renderHook(
      ({ runId }: { runId: string }) =>
        useAgentRunTaskPlan(runId, "2026-08-07T12:00:00Z", true, loadTaskPlan),
      { initialProps: { runId: "run-1" } },
    );
    await waitFor(() => expect(result.current.plan?.run_id).toBe("run-1"));

    // Switching runs while the next read is still in flight must not leave the
    // previous run's steps standing under the new run's heading.
    rerender({ runId: "run-2" });
    expect(result.current.plan).toBeNull();

    gate.release();
    await waitFor(() => expect(result.current.plan?.run_id).toBe("run-2"));
  });

  it("keeps the last good plan when a refresh fails, and can be retried", async () => {
    let attempts = 0;
    const loadTaskPlan = async () => {
      attempts += 1;
      if (attempts === 2) throw new Error("network");
      return PLAN;
    };

    const { result, rerender } = renderHook(
      ({ updatedAt }: { updatedAt: string }) =>
        useAgentRunTaskPlan("run-1", updatedAt, true, loadTaskPlan),
      { initialProps: { updatedAt: "2026-08-07T12:00:00Z" } },
    );
    await waitFor(() => expect(result.current.plan).toEqual(PLAN));

    rerender({ updatedAt: "2026-08-07T12:05:00Z" });
    await waitFor(() => expect(result.current.error).toBe(true));
    // A failed refresh is not evidence the plan is gone.
    expect(result.current.plan).toEqual(PLAN);

    // A settled run's timestamp never moves again, so the retry is the only
    // thing that can clear this.
    result.current.retry();
    await waitFor(() => expect(result.current.error).toBe(false));
  });
});
