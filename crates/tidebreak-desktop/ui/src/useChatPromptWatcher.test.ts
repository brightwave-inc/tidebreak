// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as agentNotify from "./agentNotify";
import type { ApiClient } from "./api";
import { useChatAttention } from "./ChatAttention";
import { useInbox } from "./Inbox";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";
import { useChatPromptWatcher } from "./useChatPromptWatcher";

vi.mock("./agentNotify", () => ({
  presentNeedsYou: vi.fn().mockResolvedValue("native"),
}));

function question(callId: string, turnId = "turn-1") {
  return { callId, turnId, questions: [] };
}

function folderRequest(callId: string) {
  return { callId, turnId: "turn-1", displayPath: "~/Notes" };
}

/** One internal-session inbox entry, with the calls parked behind it. */
function inboxEntry(
  chatId: string,
  calls: Array<{
    callId: string;
    kind?: "question" | "folder_access" | "tool_approval";
  }>,
  options: { title?: string; workspaceId?: string } = {},
) {
  return {
    conversation: {
      sessionId: chatId,
      workspaceId: options.workspaceId ?? null,
    },
    title: options.title ?? null,
    attention: {
      state: {
        type: "needs_you" as const,
        prompt: "waiting",
        source: "structured" as const,
      },
      source: "structured" as const,
    },
    items: calls.map(({ callId, kind = "question" as const }) => ({
      turnId: "turn-1",
      callId,
      kind,
      action: null,
      requestedAt: "2026-08-04T00:00:00Z",
    })),
    waitingSince: "2026-08-04T00:00:00Z",
  };
}

function stubClient(overrides: Record<string, unknown> = {}) {
  return {
    listInbox: vi.fn().mockResolvedValue([]),
    listPendingUserQuestions: vi.fn().mockResolvedValue([]),
    listPendingFolderAccessRequests: vi.fn().mockResolvedValue([]),
    listPendingOutputWritebackRequests: vi.fn().mockResolvedValue([]),
    ...overrides,
  } as unknown as ApiClient;
}

const presentNeedsYou = vi.mocked(agentNotify.presentNeedsYou);

beforeEach(() => {
  presentNeedsYou.mockClear();
  usePendingPrompts.setState({
    chatId: null,
    userQuestions: [],
    folderAccess: [],
    outputWritebacks: [],
  });
  useChatAttention.getState().clear();
  useInbox.getState().clear();
});
afterEach(cleanup);

describe("useChatPromptWatcher", () => {
  it("publishes what the open conversation is waiting on", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi.fn().mockResolvedValue([question("call-1")]),
      listPendingFolderAccessRequests: vi
        .fn()
        .mockResolvedValue([folderRequest("call-2")]),
    });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));

    await waitFor(() =>
      expect(usePendingPrompts.getState().userQuestions).toHaveLength(1),
    );
    await waitFor(() =>
      expect(usePendingPrompts.getState().folderAccess).toHaveLength(1),
    );
    expect(client.listPendingUserQuestions).toHaveBeenCalledWith("chat-1");
  });

  it("reads again when the event stream says to", async () => {
    const client = stubClient();
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() =>
      expect(client.listPendingUserQuestions).toHaveBeenCalledTimes(1),
    );

    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await waitFor(() =>
      expect(client.listPendingUserQuestions).toHaveBeenCalledTimes(2),
    );
    await waitFor(() => expect(client.listInbox).toHaveBeenCalledTimes(2));
  });

  it("leaves what was already waiting at launch to the inbox badge", async () => {
    const client = stubClient({
      listInbox: vi
        .fn()
        .mockResolvedValue([inboxEntry("chat-2", [{ callId: "call-1" }])]),
    });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(useInbox.getState().entries).toHaveLength(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(client.listInbox).toHaveBeenCalledTimes(2));

    expect(presentNeedsYou).not.toHaveBeenCalled();
  });

  it("notifies once per question that parks after the first read", async () => {
    const listInbox = vi
      .fn()
      .mockResolvedValueOnce([])
      .mockResolvedValue([
        inboxEntry("chat-2", [{ callId: "call-1" }], {
          title: "Plan the trip",
        }),
      ]);
    const client = stubClient({
      listInbox,
      listPendingUserQuestions: vi.fn().mockResolvedValue([
        {
          callId: "call-1",
          turnId: "turn-1",
          askedAt: "2026-08-04T00:00:00Z",
          questions: [
            {
              id: "q1",
              header: "Dates",
              question: "Which week works for you?",
              options: [],
              questionType: "single_select",
              allowFreeForm: true,
            },
          ],
        },
      ]),
    });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(presentNeedsYou).toHaveBeenCalledTimes(1));
    const notice = presentNeedsYou.mock.calls[0]![0];
    expect(notice).toMatchObject({
      name: "Plan the trip",
      href: "/c/chat-2",
      viewing: false,
    });
    // The question comes from the conversation's own route, and only when
    // the notice asks for it.
    await expect(notice.question()).resolves.toEqual({
      kind: "question",
      text: "Which week works for you?",
    });
    expect(notice.stillWaiting()).toBe(true);

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(3));
    expect(presentNeedsYou).toHaveBeenCalledTimes(1);
  });

  it("tells the notice when the parked chat is the one on screen", async () => {
    const listInbox = vi
      .fn()
      .mockResolvedValueOnce([])
      .mockResolvedValue([inboxEntry("chat-1", [{ callId: "call-1" }])]);
    const client = stubClient({ listInbox });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await waitFor(() => expect(presentNeedsYou).toHaveBeenCalledTimes(1));
    expect(presentNeedsYou.mock.calls[0]![0]).toMatchObject({
      name: "New work",
      viewing: true,
    });
  });

  it("leaves a code conversation to its digest", async () => {
    const listInbox = vi
      .fn()
      .mockResolvedValueOnce([])
      .mockResolvedValue([
        inboxEntry("session-1", [{ callId: "call-1" }], {
          workspaceId: "ws-1",
        }),
      ]);
    const client = stubClient({ listInbox });
    renderHook(() => useChatPromptWatcher(client, null));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(2));

    expect(presentNeedsYou).not.toHaveBeenCalled();
  });

  it("forgets a question once it stops being pending", async () => {
    // The announce-set spans the life of the shell, so it has to be pruned or a
    // long session accumulates the id of every question ever asked.
    const listInbox = vi
      .fn()
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([inboxEntry("chat-2", [{ callId: "call-1" }])])
      .mockResolvedValue([]);
    const client = stubClient({ listInbox });
    renderHook(() => useChatPromptWatcher(client, "chat-1"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(presentNeedsYou).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("userQuestions"));
    await waitFor(() => expect(listInbox).toHaveBeenCalledTimes(3));

    listInbox.mockResolvedValue([inboxEntry("chat-2", [{ callId: "call-2" }])]);
    act(() => useRefreshSignals.getState().signal("userQuestions"));

    await waitFor(() => expect(presentNeedsYou).toHaveBeenCalledTimes(2));
  });

  it("drops the previous conversation's requests when the open chat changes", async () => {
    const client = stubClient({
      listPendingUserQuestions: vi
        .fn()
        .mockResolvedValueOnce([question("call-1")])
        .mockResolvedValue([]),
    });
    const { rerender } = renderHook(
      ({ chatId }) => useChatPromptWatcher(client, chatId),
      {
        initialProps: { chatId: "chat-1" },
      },
    );
    await waitFor(() =>
      expect(usePendingPrompts.getState().userQuestions).toHaveLength(1),
    );

    rerender({ chatId: "chat-2" });

    await waitFor(() =>
      expect(usePendingPrompts.getState().userQuestions).toHaveLength(0),
    );
    expect(client.listPendingUserQuestions).toHaveBeenLastCalledWith("chat-2");
  });

  it("discards a read that lands after the reader has moved on", async () => {
    let settleFirst!: (value: unknown[]) => void;
    const client = stubClient({
      listPendingUserQuestions: vi
        .fn()
        .mockImplementationOnce(
          () => new Promise((resolve) => (settleFirst = resolve)),
        )
        .mockResolvedValue([]),
    });
    const { rerender } = renderHook(
      ({ chatId }) => useChatPromptWatcher(client, chatId),
      {
        initialProps: { chatId: "chat-1" },
      },
    );

    rerender({ chatId: "chat-2" });
    await act(async () => {
      settleFirst([question("call-1")]);
    });

    // Publishing that would put chat-1's question on screen under chat-2.
    expect(usePendingPrompts.getState().userQuestions).toHaveLength(0);
  });

  it("marks parked chats even when no conversation is open", async () => {
    const client = stubClient({
      listInbox: vi.fn().mockResolvedValue([
        inboxEntry("chat-2", [{ callId: "call-question" }]),
        inboxEntry("chat-3", [
          { callId: "call-folder", kind: "folder_access" },
          { callId: "call-approval", kind: "tool_approval" },
        ]),
      ]),
    });
    renderHook(() => useChatPromptWatcher(client, null));

    await waitFor(() =>
      expect(useChatAttention.getState().chatIdsWithPendingPrompts).toEqual(
        new Set(["chat-2", "chat-3"]),
      ),
    );
    // The rail's markers and the inbox come from the same read, so a chat
    // parked on an approval is marked exactly like one parked on a question.
    // Two entries, three calls: the queue is conversations now, and chat-3 is
    // one row holding two parked calls rather than two rows.
    expect(useInbox.getState().entries).toHaveLength(2);
    expect(
      useInbox.getState().entries.flatMap((entry) => entry.items),
    ).toHaveLength(3);
    // Already waiting when the watcher started: the badge says so.
    expect(presentNeedsYou).not.toHaveBeenCalled();
    expect(client.listPendingUserQuestions).not.toHaveBeenCalled();
    expect(usePendingPrompts.getState().userQuestions).toEqual([]);
  });
});
