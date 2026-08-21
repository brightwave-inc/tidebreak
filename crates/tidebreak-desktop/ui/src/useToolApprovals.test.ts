// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useChatSessionStore } from "./ChatSessionStore";
import { useChatListStore } from "./ChatListStore";
import { useToolApprovals } from "./useToolApprovals";

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
    decideApproval: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

function seedApprovalCard(callId: string) {
  useChatSessionStore.getState().update((session) => ({
    ...session,
    messages: [
      {
        id: "m1",
        role: "approval" as const,
        callId,
        summary: "Run a command",
        canApprove: true,
        canRemember: true,
      },
    ],
  }));
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
});

afterEach(() => {
  cleanup();
  useChatListStore.getState().setDeletingChatId(null);
});

describe("useToolApprovals", () => {
  it("sends a decision and marks its card resolved", async () => {
    seedApprovalCard("call-1");
    const client = stubClient();
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    await act(async () =>
      result.current.decide("call-1", "approve", "whole_tool"),
    );

    expect(client.decideApproval).toHaveBeenCalledWith(
      "chat-1",
      "call-1",
      "approve",
      "whole_tool",
    );
    const [message] = useChatSessionStore.getState().messages;
    expect(message.role === "approval" && message.resolved).toBe(true);
  });

  it("defaults to not remembering the decision", async () => {
    const client = stubClient();
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    await act(async () => result.current.decide("call-1", "reject"));

    expect(client.decideApproval).toHaveBeenCalledWith(
      "chat-1",
      "call-1",
      "reject",
      null,
    );
  });

  it("ignores a second click while a decision is in flight", async () => {
    let release: (() => void) | undefined;
    const client = stubClient({
      decideApproval: vi
        .fn()
        .mockImplementation(
          () => new Promise<void>((resolve) => (release = resolve)),
        ),
    });
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    act(() => result.current.decide("call-1", "approve"));
    await waitFor(() =>
      expect(result.current.deciding.has("call-1")).toBe(true),
    );
    act(() => result.current.decide("call-1", "reject"));

    expect(client.decideApproval).toHaveBeenCalledTimes(1);
    await act(async () => {
      release?.();
    });
    await waitFor(() => expect(result.current.deciding.size).toBe(0));
  });

  it("decides two different calls at once", async () => {
    const client = stubClient({
      decideApproval: vi.fn().mockImplementation(() => new Promise(() => {})),
    });
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    act(() => result.current.decide("call-1", "approve"));
    act(() => result.current.decide("call-2", "approve"));

    await waitFor(() => expect(result.current.deciding.size).toBe(2));
    expect(client.decideApproval).toHaveBeenCalledTimes(2);
  });

  it("reports a failed decision and leaves the card unresolved", async () => {
    seedApprovalCard("call-1");
    const client = stubClient({
      decideApproval: vi.fn().mockRejectedValue(new Error("server said no")),
    });
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    await act(async () => result.current.decide("call-1", "approve"));

    await waitFor(() =>
      expect(result.current.errors["call-1"]).toContain("server said no"),
    );
    const [message] = useChatSessionStore.getState().messages;
    expect(message.role === "approval" && message.resolved).toBeFalsy();
  });

  it("drops a failed decision once its chat is being deleted", async () => {
    // The pane stays mounted on the doomed chat for the whole delete round
    // trip, so its id still matches — the deletion is the only signal that the
    // conversation this error would land on is on its way out.
    const decision = deferred<void>();
    const client = stubClient({
      decideApproval: vi.fn(() => decision.promise),
    });
    seedApprovalCard("call-1");
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    act(() => result.current.decide("call-1", "approve"));
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      decision.reject(new Error("gone"));
      await decision.promise.catch(() => {});
    });

    expect(result.current.errors).toEqual({});
  });

  it("does not carry a decision error into the next conversation", async () => {
    // The pane is keyed today, so this hook is replaced rather than reused.
    // Switching in place is what a caller without that key would do, and the
    // previous conversation's error must not follow the reader across.
    const decision = deferred<void>();
    const client = stubClient({
      decideApproval: vi.fn(() => decision.promise),
    });
    seedApprovalCard("call-1");
    const { result, rerender } = renderHook(
      ({ chatId }) => useToolApprovals(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );

    act(() => result.current.decide("call-1", "approve"));
    await act(async () => {
      decision.reject(new Error("gone"));
      await decision.promise.catch(() => {});
    });
    expect(result.current.errors).not.toEqual({});

    rerender({ chatId: "chat-2" });

    expect(result.current.errors).toEqual({});
    expect(result.current.deciding.size).toBe(0);
  });
});
