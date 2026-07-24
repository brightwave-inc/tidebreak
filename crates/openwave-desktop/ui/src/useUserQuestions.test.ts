// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useUserQuestions } from "./useUserQuestions";
import { useRefreshSignals } from "./RefreshSignals";
import * as host from "./host";

vi.mock("./host", () => ({
  requestUserAttention: vi.fn().mockResolvedValue(undefined),
}));

function pending(callId: string, turnId = "turn-1") {
  return { callId, turnId, questions: [] };
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
    listPendingUserQuestions: vi.fn().mockResolvedValue([]),
    answerUserQuestions: vi.fn().mockResolvedValue(undefined),
    cancel: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  useChatListStore.getState().setDeletingChatId(null);
});

describe("useUserQuestions", () => {
  it("asks for attention only when a request is new", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([pending("call-1")]),
    });
    renderHook(() => useUserQuestions(client, "chat-1"));

    await waitFor(() =>
      expect(host.requestUserAttention).toHaveBeenCalledTimes(1),
    );

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() =>
      expect(client.listPendingUserQuestions).toHaveBeenCalledTimes(2),
    );
    expect(host.requestUserAttention).toHaveBeenCalledTimes(1);
  });

  it("ignores a second answer for a call already in flight", async () => {
    let release: (() => void) | undefined;
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([pending("call-1")]),
      answerUserQuestions: vi
        .fn()
        .mockImplementation(
          () => new Promise<void>((resolve) => (release = resolve)),
        ),
    });
    const { result } = renderHook(() => useUserQuestions(client, "chat-1"));
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    act(() => result.current.answer("call-1", []));
    await waitFor(() => expect(result.current.answering.size).toBe(1));
    act(() => result.current.answer("call-1", []));

    expect(client.answerUserQuestions).toHaveBeenCalledTimes(1);
    await act(async () => {
      release?.();
    });
  });

  it("cancels the turn that owns a request", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi
        .fn()
        .mockResolvedValue([pending("call-9", "turn-9")]),
    });
    const { result } = renderHook(() => useUserQuestions(client, "chat-1"));
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    await act(async () => result.current.cancel("turn-9"));

    expect(client.cancel).toHaveBeenCalledWith("chat-1", "turn-9");
  });

  it("ignores a cancel for a turn it has no request for", async () => {
    const client = stubClient();
    const { result } = renderHook(() => useUserQuestions(client, "chat-1"));

    await act(async () => result.current.cancel("turn-unknown"));

    expect(client.cancel).not.toHaveBeenCalled();
  });

  it("does not report a failure onto the conversation that replaced it", async () => {
    let reject: ((err: Error) => void) | undefined;
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([pending("call-1")]),
      answerUserQuestions: vi
        .fn()
        .mockImplementation(
          () => new Promise<void>((_resolve, no) => (reject = no)),
        ),
    });
    const { result, rerender } = renderHook(
      ({ chatId }) => useUserQuestions(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    act(() => result.current.answer("call-1", []));
    await waitFor(() => expect(result.current.answering.size).toBe(1));

    rerender({ chatId: "chat-2" });
    await act(async () => {
      reject?.(new Error("network went away"));
    });

    expect(result.current.errors).toEqual({});
  });

  it("drops a failed answer once its chat is being deleted", async () => {
    const answer = deferred<void>();
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([pending("call-1")]),
      answerUserQuestions: vi.fn(() => answer.promise),
    });
    const { result } = renderHook(() => useUserQuestions(client, "chat-1"));
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    act(() => result.current.answer("call-1", []));
    // The reader deletes the chat before the answer settles. Its id still
    // matches the pane, which is why a plain id comparison misses this.
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      answer.reject(new Error("gone"));
      await answer.promise.catch(() => {});
    });

    expect(result.current.errors).toEqual({});
  });
});
