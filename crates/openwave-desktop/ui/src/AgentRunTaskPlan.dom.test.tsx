// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { AgentRun, AgentRunTaskPlan } from "./api";
import {
  AgentRunTaskPlanChecklist,
  AgentRunTaskPlanProgress,
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
