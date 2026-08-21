import { describe, expect, it, vi } from "vitest";

import { createCodeSessionStore } from "./CodeSessionStore";

const deps = {
  nextId: () => "item",
  now: () => "2026-08-19T12:00:00.000Z",
};

describe("CodeSessionStore", () => {
  it("publishes a replay chunk once after reducing every frame", () => {
    const store = createCodeSessionStore();
    const listener = vi.fn();
    store.subscribe(listener);

    const effects = store.getState().applyEvents(
      Array.from({ length: 200 }, (_, index) => ({
        seq: index + 1,
        replayed: true as const,
        event: { type: "assistant_delta" as const, text: "x" },
      })),
      deps,
    );

    expect(effects).toEqual([]);
    expect(store.getState().lastSeq).toBe(200);
    expect(store.getState().assistantBuffer).toHaveLength(200);
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("does not publish duplicate frames or unchanged external updates", () => {
    const store = createCodeSessionStore();
    store
      .getState()
      .applyEvent(
        { seq: 1, event: { type: "turn_started", turn_id: "turn-1" } },
        deps,
      );
    const listener = vi.fn();
    store.subscribe(listener);

    expect(
      store
        .getState()
        .applyEvent(
          { seq: 1, event: { type: "assistant_delta", text: "late" } },
          deps,
        ),
    ).toEqual([]);
    store.getState().update((session) => session);
    store.getState().setConnectionState("live");

    expect(listener).not.toHaveBeenCalled();
    expect(store.getState().assistantBuffer).toBe("");
  });
});
