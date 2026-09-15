// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ChatTranscript, PendingToolApproval } from "./api";
import { useChatHydration } from "./useChatHydration";
import { useChatSessionStore } from "./ChatSessionStore";

const transcript: ChatTranscript = {
  messages: [],
  tool_activity: [],
  terminal_turns: [],
  last_event_seq: 7,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

afterEach(() => {
  cleanup();
  useChatSessionStore.getState().reset();
});

it.each(["transcript", "approvals"])(
  "retries a failed %s read without enabling sends before both reads finish",
  async (failure) => {
    const approvalRead = deferred<PendingToolApproval[]>();
    const client = {
      listChatMessages: vi.fn().mockResolvedValue(transcript),
      listPendingApprovals: vi.fn().mockResolvedValue([]),
    };
    if (failure === "transcript")
      client.listChatMessages.mockRejectedValueOnce(
        new Error("Connection lost"),
      );
    else
      client.listPendingApprovals.mockRejectedValueOnce(
        new Error("Connection lost"),
      );
    const { result } = renderHook(() => useChatHydration(client, "chat-1"));
    await waitFor(() => expect(result.current.error).toBe("Connection lost"));
    expect(result.current.hydrated).toBe(false);
    expect(useChatSessionStore.getState().lastSeq).toBe(0);

    client.listPendingApprovals.mockReturnValueOnce(approvalRead.promise);
    act(() => result.current.retry());
    expect(result.current.hydrated).toBe(false);
    expect(result.current.error).toBeNull();
    await waitFor(() =>
      expect(client.listChatMessages).toHaveBeenCalledTimes(2),
    );
    expect(useChatSessionStore.getState().lastSeq).toBe(0);
    await act(async () => approvalRead.resolve([]));
    expect(result.current.hydrated).toBe(true);
    expect(result.current.error).toBeNull();
    expect(useChatSessionStore.getState().lastSeq).toBe(7);
  },
);

it("ignores a transcript that arrives after leaving the chat", async () => {
  const pending = deferred<ChatTranscript>();
  const client = {
    listChatMessages: vi.fn().mockReturnValue(pending.promise),
    listPendingApprovals: vi.fn().mockResolvedValue([]),
  };
  const { unmount } = renderHook(() => useChatHydration(client, "old-chat"));
  unmount();
  await act(async () => pending.resolve(transcript));
  expect(client.listPendingApprovals).not.toHaveBeenCalled();
  expect(useChatSessionStore.getState().lastSeq).toBe(0);
});

it("rejects stale approval results when a retry replaces the request", async () => {
  const pending = deferred<PendingToolApproval[]>();
  const client = {
    listChatMessages: vi
      .fn()
      .mockResolvedValueOnce(transcript)
      .mockResolvedValue({ ...transcript, last_event_seq: 20 }),
    listPendingApprovals: vi
      .fn()
      .mockReturnValueOnce(pending.promise)
      .mockResolvedValue([]),
  };
  const { result } = renderHook(() => useChatHydration(client, "chat-1"));
  await waitFor(() =>
    expect(client.listPendingApprovals).toHaveBeenCalledTimes(1),
  );
  act(() => result.current.retry());
  await waitFor(() => expect(result.current.hydrated).toBe(true));
  await act(async () => pending.resolve([]));
  expect(useChatSessionStore.getState().lastSeq).toBe(20);
});

it("invalidates a loaded chat when the client changes", async () => {
  const client = {
    listChatMessages: vi.fn().mockResolvedValue(transcript),
    listPendingApprovals: vi.fn().mockResolvedValue([]),
  };
  const replacement = {
    ...client,
    listChatMessages: vi.fn().mockRejectedValue(new Error("Offline")),
  };
  const { result, rerender } = renderHook(
    ({ source }) => useChatHydration(source, "chat-1"),
    { initialProps: { source: client } },
  );
  await waitFor(() => expect(result.current.hydrated).toBe(true));
  rerender({ source: replacement });
  expect(result.current.hydrated).toBe(false);
  await waitFor(() => expect(result.current.error).toBe("Offline"));
  act(() => result.current.retry());
  await waitFor(() => expect(result.current.error).toBe("Offline"));
  expect(result.current.hydrated).toBe(false);
});

it("restores a parked approval after retry before reporting hydration complete", async () => {
  const pending: PendingToolApproval = {
    callId: "call-search",
    turnId: "turn-live",
    action: "search",
    approval: "search_may_share_query_and_excerpts",
    class: "sensitive",
    preview: null,
    canApprove: true,
    canRemember: true,
    grantRungs: ["whole_tool"],
    autoJudgeStatus: null,
  };
  const client = {
    listChatMessages: vi.fn().mockResolvedValue(transcript),
    listPendingApprovals: vi
      .fn()
      .mockRejectedValueOnce(new Error("Offline"))
      .mockResolvedValue([pending]),
  };
  const { result } = renderHook(() => useChatHydration(client, "chat-1"));
  await waitFor(() => expect(result.current.error).toBe("Offline"));
  act(() => result.current.retry());
  await waitFor(() => expect(result.current.hydrated).toBe(true));
  const state = useChatSessionStore.getState();
  expect(state.lastSeq).toBe(7);
  expect(state.busy).toBe(true);
  expect(state.activeTurnId).toBe("turn-live");
  expect(state.messages).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        role: "approval",
        callId: "call-search",
        canApprove: true,
      }),
    ]),
  );
});
