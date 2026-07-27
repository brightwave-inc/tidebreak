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
    execution: "sandbox",
    status,
    started_at: null,
    finished_at: null,
    last_error_code: null,
    activity: null,
    created_at: "2026-07-27T12:00:00Z",
    updated_at: "2026-07-27T12:00:00Z",
  };
}

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
      />,
    );

    expect(markup).toContain("2 background agents");
    expect(markup).toContain("Working in the background");
    expect(markup).toContain("Finished");
    expect(markup).not.toContain("Could not finish");
    expect(markup.indexOf("Running")).toBeLessThan(markup.indexOf("Completed"));
  });

  it("shows a skeleton as soon as a spawn is visible but not durable yet", () => {
    const markup = renderToStaticMarkup(
      <BackgroundAgentList
        spawns={[{ callId: "call-starting", status: "running" }]}
        runs={[]}
        loading
        error={null}
        onRetry={() => undefined}
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
      />,
    );

    expect(markup).toEqual("");
  });
});
