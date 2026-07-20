import { describe, expect, it } from "vitest";
import type { AgentRun } from "./api";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
  sandboxAgentStopKey,
} from "./SandboxAgentStop";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    id: "run-1",
    parent_id: null,
    execution: "sandbox",
    status: "running",
    started_at: null,
    finished_at: null,
    last_error_code: null,
    activity: { kind: "web_search", status: "running" },
    created_at: "2026-07-20T12:00:00Z",
    updated_at: "2026-07-20T12:00:00Z",
    ...overrides,
  };
}

describe("SandboxAgentStopFence", () => {
  it("deduplicates an exact request while allowing independent sandbox children", () => {
    const fence = new SandboxAgentStopFence();
    const first = fence.begin("chat-a", "run-1");

    expect(first).not.toBeNull();
    expect(fence.begin("chat-a", "run-1")).toBeNull();
    expect(fence.begin("chat-a", "run-2")).not.toBeNull();
    expect(fence.begin("chat-b", "run-1")).not.toBeNull();
    expect(sandboxAgentStopKey("a:b", "c")).not.toBe(
      sandboxAgentStopKey("a", "b:c"),
    );
  });

  it("prevents a stale completion from painting after chat switch or unmount", () => {
    const fence = new SandboxAgentStopFence();
    const switched = fence.begin("chat-a", "run-1");
    expect(switched && fence.isCurrent(switched, "chat-b")).toBe(false);

    const unmounted = fence.begin("chat-a", "run-2");
    fence.invalidate();
    expect(unmounted && fence.isCurrent(unmounted, "chat-a")).toBe(false);
    expect(unmounted && fence.finish(unmounted, "chat-a")).toBe(false);
  });

  it("releases only the current exact request so a failed stop can retry", () => {
    const fence = new SandboxAgentStopFence();
    const request = fence.begin("chat-a", "run-1");
    expect(request && fence.finish(request, "chat-a")).toBe(true);
    expect(fence.begin("chat-a", "run-1")).not.toBeNull();
  });
});

describe("sandbox stop presentation state", () => {
  it("offers stop only for cancellable sandbox lifecycle states", () => {
    for (const status of ["queued", "running", "waiting", "retry_wait"] as const) {
      expect(canStopSandboxAgentRun(run({ status }))).toBe(true);
    }
    for (const status of [
      "active",
      "cancelling",
      "completed",
      "failed",
      "cancelled",
    ] as const) {
      expect(canStopSandboxAgentRun(run({ status }))).toBe(false);
    }
    expect(
      canStopSandboxAgentRun(run({ execution: "foreground", status: "running" })),
    ).toBe(false);
  });

  it("reconciles only the exact sandbox child and clears stale activity", () => {
    const untouched = run({ id: "run-2" });
    const foreground = run({ id: "run-1", execution: "foreground" });
    const runs = [run(), untouched, foreground];

    const reconciled = reconcileSandboxAgentCancellation(runs, {
      id: "run-1",
      status: "cancelled",
    });

    expect(reconciled[0]).toMatchObject({ id: "run-1", status: "cancelled", activity: null });
    expect(reconciled[1]).toBe(untouched);
    expect(reconciled[2]).toBe(foreground);
  });

  it("does not let a delayed acknowledgement regress authoritative terminal state", () => {
    for (const status of ["completed", "failed", "cancelled"] as const) {
      const authoritative = run({ status, activity: null });
      const reconciled = reconcileSandboxAgentCancellation([authoritative], {
        id: authoritative.id,
        status: "cancelling",
      });
      expect(reconciled[0]).toBe(authoritative);
    }

    expect(
      reconcileSandboxAgentCancellation([run({ status: "cancelling" })], {
        id: "run-1",
        status: "cancelled",
      })[0],
    ).toMatchObject({ status: "cancelled", activity: null });
  });
});
