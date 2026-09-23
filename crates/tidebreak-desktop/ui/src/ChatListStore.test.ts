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
    last_activity_at: "2026-07-28T12:00:00Z",
    pinned_at: null,
    archived_at: null,
    running: false,
    unread: false,
    turn_count: 1,
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

describe("a list refresh", () => {
  beforeEach(() => {
    useChatListStore.setState({
      chats: [],
      archivedChats: [],
      revision: 0,
      streamedTitles: {},
    });
  });

  it("applies when nothing changed locally while it was in flight", () => {
    const startedAt = useChatListStore.getState().revision;
    expect(
      useChatListStore
        .getState()
        .acceptFetchedChats([chat("chat-1", "Roadmap")], startedAt),
    ).toBe(true);
    expect(useChatListStore.getState().chats.map((item) => item.id)).toEqual([
      "chat-1",
    ]);
  });

  /**
   * The race a background refresh would otherwise lose: the list is read,
   * the reader starts new work, and the older answer lands after. Applying it
   * would drop the conversation that was just created, and the chat route
   * would send the reader home mid-send.
   */
  it("is dropped when a chat was created while it was in flight", () => {
    const store = useChatListStore.getState();
    const startedAt = store.revision;
    store.prependChat(chat("new-chat", null));
    expect(
      useChatListStore
        .getState()
        .acceptFetchedChats([chat("chat-1", "Roadmap")], startedAt),
    ).toBe(false);
    expect(useChatListStore.getState().chats.map((item) => item.id)).toEqual([
      "new-chat",
    ]);
  });
});

describe("archiving", () => {
  beforeEach(() => {
    useChatListStore.setState({
      chats: [chat("chat-1", "Roadmap"), chat("chat-2", "Budget")],
      archivedChats: [],
      streamedTitles: {},
    });
  });

  it("moves a row into the archive and back", () => {
    const store = useChatListStore.getState();
    store.replaceChat({
      ...chat("chat-1", "Roadmap"),
      archived_at: "2026-09-20T10:00:00Z",
    });
    let state = useChatListStore.getState();
    expect(state.chats.map((item) => item.id)).toEqual(["chat-2"]);
    expect(state.archivedChats.map((item) => item.id)).toEqual(["chat-1"]);

    state.replaceChat(chat("chat-1", "Roadmap"));
    state = useChatListStore.getState();
    expect(state.chats.map((item) => item.id).sort()).toEqual([
      "chat-1",
      "chat-2",
    ]);
    expect(state.archivedChats).toEqual([]);
  });

  it("drops the archived copy once the list holds the chat again", () => {
    // A message sent into an archived conversation brings it back on the
    // server; the next list read is where the renderer learns that.
    useChatListStore.setState({
      archivedChats: [
        { ...chat("chat-9", "Old plan"), archived_at: "2026-09-01T10:00:00Z" },
      ],
      revision: 0,
    });
    const store = useChatListStore.getState();
    expect(
      store.acceptFetchedChats(
        [chat("chat-9", "Old plan"), chat("chat-1", "Roadmap")],
        0,
      ),
    ).toBe(true);
    expect(useChatListStore.getState().archivedChats).toEqual([]);
  });

  it("adopts an archived chat opened by id into the archive", () => {
    useChatListStore.getState().adoptChat({
      ...chat("chat-9", "Old plan"),
      archived_at: "2026-09-01T10:00:00Z",
    });
    const state = useChatListStore.getState();
    expect(state.archivedChats.map((item) => item.id)).toEqual(["chat-9"]);
    expect(state.chats.map((item) => item.id)).toEqual(["chat-1", "chat-2"]);
  });
});
