// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "./api";
import { useChatAttention } from "./ChatAttention";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";
import { useChatPromptWatcher } from "./useChatPromptWatcher";
import * as host from "./host";

vi.mock("./host", () => ({
  requestUserAttention: vi.fn().mockResolvedValue(undefined),
}));

function question(callId: string, turnId = "turn-1") {
  return { callId, turnId, questions: [] };
}

function folderRequest(callId: string) {
  return { callId, turnId: "turn-1", displayPath: "~/Notes" };
}

function prompt(
  chatId: string,
  {
    questions = [],
    folderAccess = [],
    outputWritebacks = [],
  }: {
    questions?: string[];
    folderAccess?: string[];
    outputWritebacks?: string[];
  } = {},
) {
  return {
    chatId,
    questionCallIds: questions,
    folderAccessCallIds: folderAccess,
    outputWritebackCallIds: outputWritebacks,
  };
}

function stubClient(overrides: Record<string, unknown> = {}) {
  return {
    listPendingChatPrompts: vi.fn().mockResolvedValue([]),
    listPendingUserQuestions: vi.fn().mockResolvedValue([]),
    listPendingFolderAccessRequests: vi.fn().mockResolvedValue([]),
    listPendingOutputWritebackRequests: vi.fn().mockResolvedValue([]),
    ...overrides,
  } as unknown as ApiClient;
}

beforeEach(() => {
  vi.mocked(host.requestUserAttention).mockClear();
  usePendingPrompts.setState({
    chatId: null,
    userQuestions: [],
    folderAccess: [],
    outputWritebacks: [],
  });
  useChatAttention.getState().clear();
});
afterEach(cleanup);

describe("useChatPromptWatcher", () => {
  it("publishes what the open conversation is waiting on", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([question("call-1")]),
      listPendingFolderAccessRequests: vi.fn().mockResolvedValue([folderRequest("call-2")]),
    });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));

    await waitFor(() => expect(usePendingPrompts.getState().userQuestions).toHaveLength(1));
    await waitFor(() => expect(usePendingPrompts.getState().folderAccess).toHaveLength(1));
    expect(client.listPendingUserQuestions).toHaveBeenCalledWith("chat-1");
  });

  it("reads again when the event stream says to", async () => {
    const client = stubClient();
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(client.listPendingUserQuestions).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await waitFor(() => expect(client.listPendingUserQuestions).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(client.listPendingChatPrompts).toHaveBeenCalledTimes(2));
  });

  it("asks for attention once per question, not once per read", async () => {
    const client = stubClient({
      listPendingChatPrompts: vi.fn().mockResolvedValue([prompt("chat-2", { questions: ["call-1"] })]),
    });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(host.requestUserAttention).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(client.listPendingChatPrompts).toHaveBeenCalledTimes(2));

    expect(host.requestUserAttention).toHaveBeenCalledTimes(1);
  });

  it("forgets a question once it stops being pending", async () => {
    // The announce-set spans the life of the shell, so it has to be pruned or a
    // long session accumulates the id of every question ever asked.
    const listPendingChatPrompts = vi
      .fn()
      .mockResolvedValueOnce([prompt("chat-1", { questions: ["call-1"] })])
      .mockResolvedValue([]);
    const client = stubClient({ listPendingChatPrompts });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(host.requestUserAttention).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(usePendingPrompts.getState().userQuestions).toHaveLength(0));

    listPendingChatPrompts.mockResolvedValue([prompt("chat-1", { questions: ["call-2"] })]);
    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await waitFor(() => expect(host.requestUserAttention).toHaveBeenCalledTimes(2));
  });

  it("drops the previous conversation's requests when the open chat changes", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi
        .fn()
        .mockResolvedValueOnce([question("call-1")])
        .mockResolvedValue([]),
    });
    const { rerender } = renderHook(({ chatId }) => useChatPromptWatcher(client, chatId), {
      initialProps: { chatId: "chat-1" },
    });
    await waitFor(() => expect(usePendingPrompts.getState().userQuestions).toHaveLength(1));

    rerender({ chatId: "chat-2" });

    await waitFor(() => expect(usePendingPrompts.getState().userQuestions).toHaveLength(0));
    expect(client.listPendingUserQuestions).toHaveBeenLastCalledWith("chat-2");
  });

  it("discards a read that lands after the reader has moved on", async () => {
    let settleFirst!: (value: unknown[]) => void;
    const client = stubClient({
      listPendingUserQuestions: vi
        .fn()
        .mockImplementationOnce(() => new Promise((resolve) => (settleFirst = resolve)))
        .mockResolvedValue([]),
    });
    const { rerender } = renderHook(({ chatId }) => useChatPromptWatcher(client, chatId), {
      initialProps: { chatId: "chat-1" },
    });

    rerender({ chatId: "chat-2" });
    await act(async () => {
      settleFirst([question("call-1")]);
    });

    // Publishing that would put chat-1's question on screen under chat-2.
    expect(usePendingPrompts.getState().userQuestions).toHaveLength(0);
  });

  it("marks parked chats even when no conversation is open", async () => {
    const client = stubClient({
      listPendingChatPrompts: vi.fn().mockResolvedValue([
        prompt("chat-2", { questions: ["call-question"] }),
        prompt("chat-3", { folderAccess: ["call-folder"] }),
      ]),
    });
    renderHook(() => useChatPromptWatcher(client, null));

    await waitFor(() =>
      expect(useChatAttention.getState().chatIdsWithPendingPrompts).toEqual(
        new Set(["chat-2", "chat-3"]),
      ),
    );
    expect(host.requestUserAttention).toHaveBeenCalledTimes(1);
    expect(client.listPendingUserQuestions).not.toHaveBeenCalled();
    expect(usePendingPrompts.getState().userQuestions).toEqual([]);
  });
});
