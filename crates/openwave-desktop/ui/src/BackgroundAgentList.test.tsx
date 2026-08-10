import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AgentRun } from "./api";
import { BackgroundAgentList } from "./BackgroundAgentList";

/** No run in these tests narrates anything; the cursor comes back unchanged. */
const noProgress = async (_runId: string, afterSequence: number) => ({
  entries: [],
  nextSequence: afterSequence,
});

function run(
  id: string,
  spawnCallId: string,
  status: AgentRun["status"],
): AgentRun {
  return {
    id,
    parent_id: "foreground",
    spawn_call_id: spawnCallId,
    tier: "background",
    execution_location: "in_process",
    status,
    task: `Task for ${id}`,
    started_at: null,
    finished_at: null,
    last_error_code: null,
    activity: null,
    submitted_outputs: [],
    terminal_text: status === "completed" ? `Result from ${id}` : null,
    created_at: "2026-07-27T12:00:00Z",
    updated_at: "2026-07-27T12:00:00Z",
  };
}

const noop = async () => undefined;
const noActivity = async () => [];
const noTaskPlan = async () => null;

describe("BackgroundAgentList", () => {
  it("groups only the durable children of its own spawn step", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[
          { callId: "call-running", status: "completed" },
          { callId: "call-completed", status: "completed" },
        ]}
        runs={[
          run("run-running", "call-running", "running"),
          run("run-completed", "call-completed", "completed"),
          run("run-other", "different-call", "failed"),
        ]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("2 background agents");
    expect(markup).toContain("Working in the background");
    expect(markup).toContain("Finished");
    expect(markup).toContain("Task for run-running");
    expect(markup).not.toContain("Could not finish");
    expect(markup.indexOf("Running")).toBeLessThan(markup.indexOf("Completed"));
  });

  it("keeps result text out of the list, which is a list to scan", () => {
    const failed = run("run-failed", "call-failed", "failed");
    failed.terminal_text = "Sandbox task failed (provider_error)";
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[
          { callId: "call-failed", status: "completed" },
          { callId: "call-done", status: "completed" },
        ]}
        runs={[failed, run("run-done", "call-done", "completed")]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("Task for run-failed");
    expect(markup).not.toContain("Sandbox task failed (provider_error)");
    expect(markup).not.toContain("Result from run-done");
  });

  it("names the files an agent submitted, which are its deliverables", () => {
    const submitted = run("run-done", "call-done", "completed");
    submitted.submitted_outputs = [
      { output_id: "output-1", filename: "Q3 revenue.md" },
      { output_id: "output-2", filename: "revenue.xlsx" },
    ];
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-done", status: "completed" }]}
        runs={[submitted]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("Q3 revenue.md");
    expect(markup).toContain("revenue.xlsx");
  });

  it("shows a skeleton as soon as a spawn is visible but not durable yet", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-starting", status: "running" }]}
        runs={[]}
        loading
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("Starting background agent");
  });

  it("keeps a failed spawn out of the agent list when no child was admitted", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-failed", status: "failed" }]}
        runs={[]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toEqual("");
  });

  it("settles a declined spawn instead of waiting for a run that cannot arrive", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-denied", status: "denied" }]}
        runs={[]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("Declined");
    expect(markup).not.toContain("Waiting for background agent");
  });

  it("offers Stop on a cancellable run", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[
          { callId: "call-running", status: "running" },
          { callId: "call-done", status: "completed" },
        ]}
        runs={[
          run("run-running", "call-running", "running"),
          run("run-done", "call-done", "completed"),
        ]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).toContain("Stop");
  });

  it("does not offer Stop on a settled run", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-done", status: "completed" }]}
        runs={[run("run-done", "call-done", "completed")]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
        onLoadTaskPlan={noTaskPlan}
        onLoadProgress={noProgress}
      />,
    );

    expect(markup).not.toContain(">Stop<");
  });
});
