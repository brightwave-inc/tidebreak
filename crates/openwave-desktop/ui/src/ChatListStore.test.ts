import { beforeEach, describe, expect, it } from "vitest";

import type { Chat } from "./api";
import { useChatListStore } from "./ChatListStore";

function chat(id: string, title: string | null): Chat {
  return {
    id,
    project_id: null,
    title,
    model: null,
    reasoning_effort: null,
    attachment_revision: 0,
    root_attachments: [],
    created_at: "2026-07-28T12:00:00Z",
  };
}

beforeEach(() => {
  useChatListStore.setState({ chats: [], derivedTitleChatId: null });
});

describe("a title the server derived", () => {
  it("names the chat and marks it as newly arrived", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null), chat("chat-2", null)]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");

    const state = useChatListStore.getState();
    expect(state.chats.map((item) => item.title)).toEqual([
      "Q3 revenue reconciliation",
      null,
    ]);
    expect(state.derivedTitleChatId).toBe("chat-1");
  });

  /**
   * The socket restates the current name on every connect, which is what covers
   * a title stored before the renderer was listening. Treating a restatement as
   * news would replay the typewriter on every reconnect.
   */
  it("is not news when the window already shows it", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", "Q3 revenue reconciliation")]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");

    expect(useChatListStore.getState().derivedTitleChatId).toBeNull();
  });

  it("is ignored for a chat this window does not have", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null)]);
    store.applyDerivedTitle("chat-elsewhere", "Some other conversation");

    const state = useChatListStore.getState();
    expect(state.chats).toEqual([chat("chat-1", null)]);
    expect(state.derivedTitleChatId).toBeNull();
  });
});
