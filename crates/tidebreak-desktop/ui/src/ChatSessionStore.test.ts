import { describe, expect, it } from "vitest";
import { createChatSessionStore } from "./ChatSessionStore";
import { initialChatSessionState } from "./ChatSessionReducer";

const deps = { nextId: () => "id", now: () => "2026-07-23T12:00:00.000Z" };

describe("ChatSessionStore", () => {
  it("applies stream events through the reducer and returns their effects", () => {
    const store = createChatSessionStore();
    const effects = store
      .getState()
      .applyEvent(
        { seq: 1, event: { type: "turn_started", turn_id: "turn-1" } },
        deps,
      );
    expect(store.getState().busy).toBe(true);
    expect(store.getState().activeTurnId).toBe("turn-1");
    expect(store.getState().lastSeq).toBe(1);
    expect(effects.map((effect) => effect.type)).toEqual([
      "invalidate_terminal_hydration",
      "turn_began",
    ]);
  });

  it("supports external session updates without disturbing actions", () => {
    const store = createChatSessionStore();
    store.getState().update((session) => ({
      ...session,
      busy: true,
      messages: [{ id: "m1", role: "user", text: "hi" }],
    }));
    expect(store.getState().busy).toBe(true);
    expect(store.getState().messages).toHaveLength(1);
    // Actions survive an update and further events still apply.
    store
      .getState()
      .applyEvent({ seq: 1, event: { type: "text_delta", text: "x" } }, deps);
    expect(store.getState().messages).toHaveLength(2);
  });

  it("reset restores a pristine session", () => {
    const store = createChatSessionStore();
    store
      .getState()
      .applyEvent(
        { seq: 5, event: { type: "turn_started", turn_id: "turn-1" } },
        deps,
      );
    store.getState().reset();
    const fresh = initialChatSessionState();
    expect(store.getState().lastSeq).toBe(fresh.lastSeq);
    expect(store.getState().messages).toEqual([]);
    expect(store.getState().busy).toBe(false);
    expect(store.getState().activeTurnId).toBeNull();
  });

  it("publishes a run of frames once, with every frame's effects", () => {
    const store = createChatSessionStore();
    let publishes = 0;
    store.subscribe(() => {
      publishes += 1;
    });
    const effects = store.getState().applyEvents(
      [
        { seq: 1, event: { type: "turn_started", turn_id: "turn-1" } },
        { seq: 2, event: { type: "text_delta", text: "Hel" } },
        { seq: 3, event: { type: "text_delta", text: "lo" } },
      ],
      deps,
    );
    expect(publishes).toBe(1);
    expect(store.getState().lastSeq).toBe(3);
    expect(store.getState().messages.at(-1)).toMatchObject({
      role: "assistant",
      text: "Hello",
    });
    expect(effects.map((effect) => effect.type)).toEqual([
      "invalidate_terminal_hydration",
      "turn_began",
    ]);

    // A run the cursor has already passed publishes nothing at all.
    store
      .getState()
      .applyEvents(
        [{ seq: 2, event: { type: "text_delta", text: "x" } }],
        deps,
      );
    expect(publishes).toBe(1);
  });

  it("keeps the reducer's dedup: stale seqs change nothing and yield no effects", () => {
    const store = createChatSessionStore();
    store
      .getState()
      .applyEvent(
        { seq: 3, event: { type: "turn_started", turn_id: "turn-1" } },
        deps,
      );
    const before = store.getState().messages;
    const effects = store
      .getState()
      .applyEvent(
        { seq: 3, event: { type: "text_delta", text: "late" } },
        deps,
      );
    expect(effects).toEqual([]);
    expect(store.getState().messages).toBe(before);
  });
});
