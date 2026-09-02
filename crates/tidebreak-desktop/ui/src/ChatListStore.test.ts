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
    permission_mode: null,
    network_policy: { mode: "off" },
    attachment_revision: 0,
    memory_incognito: false,
    root_attachments: [],
    created_at: "2026-07-28T12:00:00Z",
  };
}

beforeEach(() => {
  useChatListStore.setState({
    chats: [],
    derivedTitleChatId: null,
    streamedTitles: {},
  });
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

    const state = useChatListStore.getState();
    expect(state.derivedTitleChatId).toBeNull();
    expect(state.streamedTitles).toEqual({
      "chat-1": "Q3 revenue reconciliation",
    });
  });

  it("survives arriving before the initial chat list", () => {
    const store = useChatListStore.getState();
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");
    store.setChats([chat("chat-1", null), chat("chat-2", null)]);

    const state = useChatListStore.getState();
    expect(state.chats.map((item) => item.title)).toEqual([
      "Q3 revenue reconciliation",
      null,
    ]);
    expect(state.derivedTitleChatId).toBe("chat-1");
  });

  it("is not overwritten when an older list request finishes afterward", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null)]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");
    store.setChats([chat("chat-1", null)]);

    expect(useChatListStore.getState().chats[0].title).toBe(
      "Q3 revenue reconciliation",
    );
  });

  it("does not resurrect a derivation over a later authoritative rename", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null)]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");
    store.replaceChat(chat("chat-1", "Ledger work"), true);
    store.setChats([chat("chat-1", "Ledger work")]);

    const state = useChatListStore.getState();
    expect(state.chats[0].title).toBe("Ledger work");
    expect(state.streamedTitles).toEqual({});
  });

  it("survives an unrelated mutation response that raced the title write", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null)]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");
    store.replaceChat({
      ...chat("chat-1", null),
      permission_mode: "allow",
    });

    expect(useChatListStore.getState().chats[0].title).toBe(
      "Q3 revenue reconciliation",
    );
  });

  it("honors a deliberate manual clear after a derived title", () => {
    const store = useChatListStore.getState();
    store.setChats([chat("chat-1", null)]);
    store.applyDerivedTitle("chat-1", "Q3 revenue reconciliation");
    store.replaceChat(chat("chat-1", null), true);
    store.setChats([chat("chat-1", null)]);

    const state = useChatListStore.getState();
    expect(state.chats[0].title).toBeNull();
    expect(state.streamedTitles).toEqual({});
  });
});
