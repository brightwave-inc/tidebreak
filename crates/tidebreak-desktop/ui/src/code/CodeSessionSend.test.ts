import { describe, expect, it, vi } from "vitest";

import { createCodeSessionStore } from "./CodeSessionStore";
import { submitAcceptedTurn } from "./CodeSessionSend";
import { userItemId } from "./CodeSessionReducer";

const TURN = {
  id: "turn-1",
  session_id: "sess-1",
  ordinal: 1,
  status: "running" as const,
  user_input: "list the files",
  started_at: "2026-08-15T12:00:00.000Z",
};

describe("submitAcceptedTurn", () => {
  it("inserts a turn-keyed user item only after the server accepts", async () => {
    const store = createCodeSessionStore();
    await submitAcceptedTurn(store.getState().update, async () => ({
      kind: "ran" as const,
      turn: TURN,
    }));
    expect(store.getState().items).toEqual([
      {
        kind: "user",
        id: userItemId("turn-1"),
        turnId: "turn-1",
        text: "list the files",
      },
    ]);
  });

  it("leaves the transcript empty when submit fails, so a retry cannot stack", async () => {
    const store = createCodeSessionStore();
    await expect(
      submitAcceptedTurn(store.getState().update, async () => {
        throw new Error("session is fenced");
      }),
    ).rejects.toThrow("session is fenced");
    expect(store.getState().items).toEqual([]);
  });
});

describe("retry after a failed submit", () => {
  it("does not keep a bubble from the failed attempt", async () => {
    const store = createCodeSessionStore();
    const submit = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({ kind: "ran" as const, turn: TURN });
    await expect(
      submitAcceptedTurn(store.getState().update, submit),
    ).rejects.toThrow("offline");
    expect(store.getState().items).toEqual([]);
    await submitAcceptedTurn(store.getState().update, submit);
    expect(store.getState().items).toHaveLength(1);
    expect(store.getState().items[0]).toMatchObject({
      kind: "user",
      turnId: "turn-1",
      text: "list the files",
    });
  });
});

describe("accepted turn after the socket already painted", () => {
  it("inserts the prompt above the streamed assistant reply", async () => {
    const store = createCodeSessionStore();
    const deps = {
      nextId: () => "streamed",
      now: () => "2026-08-15T12:00:02.000Z",
    };
    store.getState().applyEvent(
      { seq: 1, event: { type: "turn_started", turn_id: "turn-1" } },
      deps,
    );
    store.getState().applyEvent(
      { seq: 2, event: { type: "assistant_delta", text: "README.md" } },
      deps,
    );
    store.getState().applyEvent(
      {
        seq: 3,
        event: {
          type: "turn_completed",
          usage: {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          },
        },
      },
      deps,
    );
    await submitAcceptedTurn(store.getState().update, async () => ({
      kind: "ran" as const,
      turn: { ...TURN, status: "completed" as const },
    }));
    expect(store.getState().items.map((item) => item.kind)).toEqual([
      "user",
      "assistant",
      "turn_boundary",
    ]);
    expect(store.getState().items[0]).toMatchObject({
      kind: "user",
      turnId: "turn-1",
      text: "list the files",
    });
  });
});
