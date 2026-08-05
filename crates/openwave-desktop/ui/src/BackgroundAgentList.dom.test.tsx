// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { AgentActivityHistoryEntry, AgentRun } from "./api";
import { BackgroundAgentList } from "./BackgroundAgentList";

afterEach(cleanup);

function run(status: AgentRun["status"]): AgentRun {
  return {
    id: "run-1",
    parent_id: "foreground",
    spawn_call_id: "spawn-call",
    tier: "background",
    execution_location: "in_process",
    status,
    task: "Prepare the report",
    started_at: "2026-08-05T18:35:00Z",
    finished_at: status === "completed" ? "2026-08-05T18:37:00Z" : null,
    last_error_code: null,
    activity:
      status === "running" ? { kind: "exec", status: "running" } : null,
    submitted_outputs: [],
    terminal_text: status === "completed" ? "Report complete" : null,
    created_at: "2026-08-05T18:35:00Z",
    updated_at:
      status === "completed"
        ? "2026-08-05T18:37:00Z"
        : "2026-08-05T18:36:00Z",
  };
}

describe("BackgroundAgentList activity disclosure", () => {
  it("preserves the row and rail preferences when polling moves a run between groups", async () => {
    let activity: AgentActivityHistoryEntry[] = [
      {
        kind: "exec",
        outcome: "running",
        at: "2026-08-05T18:36:00Z",
      },
    ];
    const loadActivity = async () => activity;
    const list = (agentRun: AgentRun) => (
      <BackgroundAgentList
        spawns={[{ callId: "spawn-call", status: "completed" }]}
        runs={[agentRun]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={async () => undefined}
        onLoadActivity={loadActivity}
      />
    );

    const { rerender } = render(list(run("running")));
    fireEvent.click(screen.getByRole("button", { name: "Show activity" }));

    const liveTrigger = await screen.findByRole("button", {
      name: "Running a command",
    });
    expect(liveTrigger.getAttribute("aria-expanded")).toBe("true");

    // End on an explicit open preference: the terminal default is closed, so
    // losing this choice during the status-group remount fails the assertion.
    fireEvent.click(liveTrigger);
    fireEvent.click(liveTrigger);
    expect(liveTrigger.getAttribute("aria-expanded")).toBe("true");

    activity = [
      {
        kind: "exec",
        outcome: "completed",
        at: "2026-08-05T18:37:00Z",
      },
    ];
    rerender(list(run("completed")));

    expect(
      screen.getByRole("button", { name: "Hide activity" }),
    ).toBeTruthy();
    const settledTrigger = await screen.findByRole("button", {
      name: "Ran 1 tool call",
    });
    expect(settledTrigger.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("list")).toBeTruthy();
  });
});
