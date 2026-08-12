// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { usePendingPrompts } from "./PendingPrompts";
import { useUserQuestions } from "./useUserQuestions";

function pending(callId: string, turnId = "turn-1") {
  return { callId, turnId, questions: [] };
}

/**
 * The questions themselves are the shell watcher's job, so a responder test
 * puts them where the watcher would have.
 */
function seedQuestions(chatId: string, requests: ReturnType<typeof pending>[]) {
  usePendingPrompts.setState({
    chatId,
    userQuestions: requests as never,
    refresh: vi.fn(),
  });
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
    answerUserQuestions: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  useChatListStore.getState().setDeletingChatId(null);
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
});

describe("useUserQuestions", () => {
  it("ignores a second answer for a call already in flight", async () => {
    let release: (() => void) | undefined;
    const client = stubClient({
      answerUserQuestions: vi
        .fn()
        .mockImplementation(
          () => new Promise<void>((resolve) => (release = resolve)),
        ),
    });
    seedQuestions("chat-1", [pending("call-1")]);
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

  it("forwards additional context with partial answers", async () => {
    const client = stubClient();
    seedQuestions("chat-1", [pending("call-9", "turn-9")]);
    const { result } = renderHook(() => useUserQuestions(client, "chat-1"));
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    act(() =>
      result.current.answer(
        "call-9",
        [
          {
            questionId: "scope",
            selectedOptionIds: ["desktop"],
          },
        ],
        "Keep the interaction compact.",
      ),
    );

    await waitFor(() =>
      expect(client.answerUserQuestions).toHaveBeenCalledWith(
        "chat-1",
        "call-9",
        [{ questionId: "scope", selectedOptionIds: ["desktop"] }],
        "Keep the interaction compact.",
      ),
    );
  });

  it("does not report a failure onto the conversation that replaced it", async () => {
    let reject: ((err: Error) => void) | undefined;
    const client = stubClient({
      answerUserQuestions: vi
        .fn()
        .mockImplementation(
          () => new Promise<void>((_resolve, no) => (reject = no)),
        ),
    });
    seedQuestions("chat-1", [pending("call-1")]);
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
    const client = stubClient({ answerUserQuestions: vi.fn(() => answer.promise) });
    seedQuestions("chat-1", [pending("call-1")]);
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

  it("does not carry an answer error into the next conversation", async () => {
    const answer = deferred<void>();
    const client = stubClient({ answerUserQuestions: vi.fn(() => answer.promise) });
    seedQuestions("chat-1", [pending("call-1")]);
    const { result, rerender } = renderHook(
      ({ chatId }) => useUserQuestions(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    act(() => result.current.answer("call-1", []));
    await act(async () => {
      answer.reject(new Error("gone"));
      await answer.promise.catch(() => {});
    });
    expect(result.current.errors).not.toEqual({});

    rerender({ chatId: "chat-2" });

    expect(result.current.errors).toEqual({});
    expect(result.current.answering.size).toBe(0);
  });
});
