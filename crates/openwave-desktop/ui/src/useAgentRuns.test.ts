// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AgentRun, ApiClient } from "./api";
import { useAgentRuns } from "./useAgentRuns";
import { useChatListStore } from "./ChatListStore";
import { useRefreshSignals } from "./RefreshSignals";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    id: "run-1",
    parent_id: null,
    execution: "sandbox",
    status: "running",
    started_at: "2026-07-24T12:00:00Z",
    finished_at: null,
    last_error_code: null,
    activity: null,
    created_at: "2026-07-24T12:00:00Z",
    updated_at: "2026-07-24T12:00:00Z",
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  promise.catch(() => {});
  return { promise, resolve, reject };
}

function stubClient(overrides: Record<string, unknown> = {}) {
  return {
    listAgentRuns: vi.fn().mockResolvedValue([]),
    cancelAgentRun: vi
      .fn()
      .mockResolvedValue({ id: "run-1", status: "cancelling" }),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  useChatListStore.getState().setDeletingChatId(null);
});

describe("useAgentRuns", () => {
  it("reads as loading until the first listing comes back", async () => {
    const listing = deferred<AgentRun[]>();
    const client = stubClient({ listAgentRuns: vi.fn(() => listing.promise) });
    const seen: boolean[] = [];
    const { result } = renderHook(() => {
      const agentRuns = useAgentRuns(client, "chat-1");
      seen.push(agentRuns.loading);
      return agentRuns;
    });

    // From the very first paint, before the effect that asks has even run:
    // "we have not asked yet" must not render as "there is no background work".
    expect(seen[0]).toBe(true);
    expect(result.current.loading).toBe(true);
    expect(result.current.runs).toEqual([]);

    await act(async () => {
      listing.resolve([run()]);
      await listing.promise;
    });

    expect(result.current.loading).toBe(false);
    expect(result.current.runs).toHaveLength(1);
    expect(client.listAgentRuns).toHaveBeenCalledWith("chat-1");
  });

  it("reports a failed listing and clears it on the next success", async () => {
    const client = stubClient({
      listAgentRuns: vi
        .fn()
        .mockRejectedValueOnce(new Error("activity unavailable"))
        .mockResolvedValue([]),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));

    await waitFor(() =>
      expect(result.current.error).toContain("activity unavailable"),
    );

    act(() => result.current.refresh());

    await waitFor(() => expect(result.current.error).toBeNull());
  });

  it("refreshes when the event stream signals", async () => {
    const client = stubClient();
    renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(client.listAgentRuns).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("agentRuns"));

    await waitFor(() => expect(client.listAgentRuns).toHaveBeenCalledTimes(2));
  });

  it("ignores a signal raised for another pollable target", async () => {
    const client = stubClient();
    renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(client.listAgentRuns).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(client.listAgentRuns).toHaveBeenCalledTimes(1);
  });

  it("polls only while a sandbox run is live", async () => {
    vi.useFakeTimers();
    try {
      const client = stubClient({
        listAgentRuns: vi
          .fn()
          .mockResolvedValueOnce([run({ status: "running" })])
          .mockResolvedValue([run({ status: "completed" })]),
      });
      const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
      await act(async () => {});
      expect(result.current.runs[0]?.status).toBe("running");

      await act(async () => {
        vi.advanceTimersByTime(5_000);
      });
      expect(client.listAgentRuns).toHaveBeenCalledTimes(2);

      // The run has finished, so the interval must not stand.
      await act(async () => {
        vi.advanceTimersByTime(15_000);
      });
      expect(client.listAgentRuns).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves a foreground run unpolled", async () => {
    vi.useFakeTimers();
    try {
      const client = stubClient({
        listAgentRuns: vi
          .fn()
          .mockResolvedValue([run({ execution: "foreground", status: "running" })]),
      });
      renderHook(() => useAgentRuns(client, "chat-1"));
      await act(async () => {});

      await act(async () => {
        vi.advanceTimersByTime(15_000);
      });

      expect(client.listAgentRuns).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("fences a second stop for a run already stopping", async () => {
    const stop = deferred<{ id: string; status: string }>();
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
      cancelAgentRun: vi.fn(() => stop.promise),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => result.current.stop("run-1"));
    await waitFor(() => expect(result.current.stoppingRunIds.size).toBe(1));
    act(() => result.current.stop("run-1"));

    expect(client.cancelAgentRun).toHaveBeenCalledTimes(1);
    await act(async () => {
      stop.resolve({ id: "run-1", status: "cancelling" });
      await stop.promise;
    });
    expect(result.current.stoppingRunIds.size).toBe(0);
  });

  it("refuses to stop a run that is not a live sandbox run", async () => {
    const client = stubClient({
      listAgentRuns: vi
        .fn()
        .mockResolvedValue([run({ status: "completed" }), run({ id: "fg", execution: "foreground" })]),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(2));

    act(() => result.current.stop("run-1"));
    act(() => result.current.stop("fg"));

    expect(client.cancelAgentRun).not.toHaveBeenCalled();
  });

  it("marks the run when its stop fails", async () => {
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
      cancelAgentRun: vi.fn().mockRejectedValue(new Error("sandbox is gone")),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    await act(async () => result.current.stop("run-1"));

    await waitFor(() =>
      expect(result.current.stopErrorRunIds.has("run-1")).toBe(true),
    );
    expect(result.current.stoppingRunIds.has("run-1")).toBe(false);
  });

  it("does not start a stop while a deletion is in flight", async () => {
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    // Any deletion, not only this chat's: the request would race a chat list
    // that is already being rebuilt.
    act(() => useChatListStore.getState().setDeletingChatId("chat-other"));
    act(() => result.current.stop("run-1"));

    expect(client.cancelAgentRun).not.toHaveBeenCalled();
  });

  it("drops a failed stop once its chat is being deleted", async () => {
    const stop = deferred<{ id: string; status: string }>();
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
      cancelAgentRun: vi.fn(() => stop.promise),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => result.current.stop("run-1"));
    await waitFor(() => expect(result.current.stoppingRunIds.size).toBe(1));

    // The reader deletes the chat before the stop settles. Its id still matches
    // the pane, which is why a plain id comparison misses this.
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      stop.reject(new Error("gone"));
      await stop.promise.catch(() => {});
    });

    expect(result.current.stopErrorRunIds.size).toBe(0);
    // The deleting chat also drops the markers, so its replacement does not
    // inherit a button stuck on "Stopping…".
    expect(result.current.stoppingRunIds.size).toBe(0);
  });

  it("lets the reader stop the run again once a failed deletion releases it", async () => {
    const first = deferred<{ id: string; status: string }>();
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
      cancelAgentRun: vi.fn(() => first.promise),
    });
    const { result } = renderHook(() => useAgentRuns(client, "chat-1"));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => result.current.stop("run-1"));
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      first.resolve({ id: "run-1", status: "cancelling" });
      await first.promise;
    });
    // The delete failed, so the conversation the reader was in is still here.
    act(() => useChatListStore.getState().setDeletingChatId(null));

    act(() => result.current.stop("run-1"));

    // A stop the fence never released would leave the button dead for good.
    expect(client.cancelAgentRun).toHaveBeenCalledTimes(2);
  });

  it("does not apply a cancellation onto the conversation that replaced it", async () => {
    const stop = deferred<{ id: string; status: string }>();
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValue([run()]),
      cancelAgentRun: vi.fn(() => stop.promise),
    });
    const { result, rerender } = renderHook(
      ({ chatId }) => useAgentRuns(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => result.current.stop("run-1"));
    await waitFor(() => expect(result.current.stoppingRunIds.size).toBe(1));

    rerender({ chatId: "chat-2" });
    await waitFor(() =>
      expect(client.listAgentRuns).toHaveBeenLastCalledWith("chat-2"),
    );
    expect(client.listAgentRuns).toHaveBeenCalledTimes(2);
    await act(async () => {
      stop.resolve({ id: "run-1", status: "cancelling" });
      await stop.promise;
    });

    expect(
      result.current.runs.some((item) => item.status === "cancelling"),
    ).toBe(false);
    // Nor may it reach for a fresh listing of the conversation now open.
    expect(client.listAgentRuns).toHaveBeenCalledTimes(2);
  });

  it("drops the previous chat's runs when the conversation changes", async () => {
    const client = stubClient({
      listAgentRuns: vi.fn().mockResolvedValueOnce([run()]).mockResolvedValue([]),
    });
    const { result, rerender } = renderHook(
      ({ chatId }) => useAgentRuns(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    rerender({ chatId: "chat-2" });

    await waitFor(() => expect(result.current.runs).toHaveLength(0));
    expect(client.listAgentRuns).toHaveBeenLastCalledWith("chat-2");
  });
});
