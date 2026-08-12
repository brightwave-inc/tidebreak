import { describe, expect, it } from "vitest";

import {
  AGENT_RUN_STATUS_GROUPS,
  RUNNING_AGENT_STATUSES,
  getAgentRunDotClass,
} from "./AgentRunDisplay";

describe("background-agent display vocabulary", () => {
  it("keeps live work ahead of settled outcomes", () => {
    expect(AGENT_RUN_STATUS_GROUPS.map((group) => group.label)).toEqual([
      "Running",
      "Needs input",
      "Stopping",
      "Completed",
      "Stopped",
      "Failed",
    ]);
    expect(RUNNING_AGENT_STATUSES).toEqual(
      new Set(["active", "queued", "running", "waiting", "retry_wait", "cancelling"]),
    );
  });

  it("gives every durable status a non-empty dot presentation", () => {
    for (const status of [
      "active",
      "queued",
      "running",
      "cancelling",
      "waiting",
      "retry_wait",
      "needs_input",
      "completed",
      "failed",
      "cancelled",
    ] as const) {
      expect(getAgentRunDotClass(status)).not.toEqual("");
    }
  });
});
