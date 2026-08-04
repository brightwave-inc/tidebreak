import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AgentRun } from "./api";
import { BackgroundAgentList } from "./BackgroundAgentList";

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
    produced_output: status === "completed",
    terminal_text: status === "completed" ? `Result from ${id}` : null,
    created_at: "2026-07-27T12:00:00Z",
    updated_at: "2026-07-27T12:00:00Z",
  };
}

const noop = async () => undefined;
const noActivity = async () => [];

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
      />,
    );

    expect(markup).toContain("2 background agents");
    expect(markup).toContain("Working in the background");
    expect(markup).toContain("Finished");
    expect(markup).toContain("Task for run-running");
    expect(markup).toContain("Result from run-completed");
    expect(markup).not.toContain("Could not finish");
    expect(markup.indexOf("Running")).toBeLessThan(markup.indexOf("Completed"));
  });

  it("shows a terminal failure error", () => {
    const failed = run("run-failed", "call-failed", "failed");
    failed.terminal_text = "Sandbox task failed (provider_error)";
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-failed", status: "completed" }]}
        runs={[failed]}
        loading={false}
        error={null}
        onRetry={() => undefined}
        onCancel={noop}
        onLoadActivity={noActivity}
      />,
    );

    expect(markup).toContain("Error:");
    expect(markup).toContain("Sandbox task failed (provider_error)");
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
      />,
    );

    expect(markup).toEqual("");
  });

  it("offers Stop on a cancellable run and View output only once it produced a result", () => {
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
        onViewOutput={() => undefined}
      />,
    );

    expect(markup).toContain("Stop");
    expect(markup).toContain("View output");
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
      />,
    );

    expect(markup).not.toContain(">Stop<");
  });
});
