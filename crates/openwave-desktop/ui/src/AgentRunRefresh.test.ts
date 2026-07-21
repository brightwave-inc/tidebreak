import { describe, expect, it } from "vitest";
import type { AgentEvent } from "./api";
import { shouldRefreshAgentRunsAfterToolEvent } from "./AgentRunRefresh";

describe("agent-run tool refresh boundary", () => {
  it("refreshes after durable tool completion, not speculative tool start", () => {
    const started: AgentEvent = {
      type: "tool_call_started",
      call_id: "call-1",
      name: "spawn_sandbox_agent",
    };
    const completed: AgentEvent = {
      type: "tool_call_completed",
      call_id: "call-1",
      status: "completed",
    };

    expect(shouldRefreshAgentRunsAfterToolEvent(started)).toBe(false);
    expect(shouldRefreshAgentRunsAfterToolEvent(completed)).toBe(true);
    expect(
      shouldRefreshAgentRunsAfterToolEvent({
        ...completed,
        status: "failed",
      }),
    ).toBe(true);
  });
});
