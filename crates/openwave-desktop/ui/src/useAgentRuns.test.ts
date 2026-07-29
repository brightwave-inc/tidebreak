// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AgentRun, ApiClient } from "./api";
import { useAgentRuns } from "./useAgentRuns";

function run(status: AgentRun["status"] = "running"): AgentRun {
  return {
    id: "run-1",
    parent_id: "foreground",
    spawn_call_id: "spawn-1",
    tier: "background",
    execution_location: "in_process",
    status,
    started_at: null,
    finished_at: null,
    last_error_code: null,
    activity: null,
    produced_output: status === "completed",
    created_at: "2026-07-27T12:00:00Z",
    updated_at: "2026-07-27T12:00:00Z",
  };
}

function client(listAgentRuns: () => Promise<AgentRun[]>): ApiClient {
  return { listAgentRuns: vi.fn(listAgentRuns) } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("useAgentRuns", () => {
  it("reads the exact chat once a spawn row is present", async () => {
    const api = client(async () => [run()]);
    const { result } = renderHook(() => useAgentRuns(api, "chat-1", ["spawn-1"]));

    await waitFor(() => expect(result.current.runs).toEqual([run()]));
    expect(api.listAgentRuns).toHaveBeenCalledWith("chat-1");
  });

  it("does not issue a read before the transcript contains a spawn", () => {
    const api = client(async () => [run()]);
    renderHook(() => useAgentRuns(api, "chat-1", []));

    expect(api.listAgentRuns).not.toHaveBeenCalled();
  });

  it("polls while the exact visible child is live", async () => {
    vi.useFakeTimers();
    const api = client(async () => [run("running")]);
    renderHook(() => useAgentRuns(api, "chat-1", ["spawn-1"]));

    await act(async () => {});
    expect(api.listAgentRuns).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(api.listAgentRuns).toHaveBeenCalledTimes(2);
  });

  it("stops polling when only an unrelated child is live", async () => {
    vi.useFakeTimers();
    const api = client(async () => [
      run("completed"),
      { ...run("running"), id: "run-other", spawn_call_id: "spawn-other" },
    ]);
    renderHook(() => useAgentRuns(api, "chat-1", ["spawn-1"]));

    await act(async () => {});
    expect(api.listAgentRuns).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(api.listAgentRuns).toHaveBeenCalledTimes(1);
  });
});
