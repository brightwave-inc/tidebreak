// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useChatSessionStore } from "./ChatSessionStore";
import { useToolApprovals } from "./useToolApprovals";

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

afterEach(cleanup);

describe("useToolApprovals", () => {
  it("sends a decision and marks its card resolved", async () => {
    seedApprovalCard("call-1");
    const client = stubClient();
    const { result } = renderHook(() => useToolApprovals(client, "chat-1"));

    await act(async () => result.current.decide("call-1", "approve", true));

    expect(client.decideApproval).toHaveBeenCalledWith(
      "chat-1",
      "call-1",
      "approve",
      true,
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
      false,
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
    await waitFor(() => expect(result.current.deciding.has("call-1")).toBe(true));
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
});
