import { afterEach, describe, expect, it } from "vitest";
import type { SequencedCodeEventFrame } from "../api/types";
import {
  acquireCodeSession,
  peekCodeSession,
  releaseCodeSession,
  resetCodeSessionRegistry,
} from "./CodeSessionRegistry";
import { userItemId } from "./CodeSessionReducer";

class FakeSocket {
  closed = false;
  constructor(
    public readonly after: number,
    public readonly emit: (frame: SequencedCodeEventFrame) => void,
  ) {}
  close() {
    this.closed = true;
  }
}

afterEach(() => {
  resetCodeSessionRegistry();
});

describe("CodeSessionRegistry", () => {
  it("shares one store across two acquires and closes the socket on last release", () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (after: number, onFrame: (frame: SequencedCodeEventFrame) => void) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };

    const first = acquireCodeSession("s1", openSocket);
    const second = acquireCodeSession("s1", openSocket);
    expect(first).toBe(second);
    expect(peekCodeSession("s1")?.refCount).toBe(2);
    expect(sockets).toHaveLength(1);

    first.getState().applyEvent(
      { seq: 1, event: { type: "turn_started", turn_id: "t1" } },
      { nextId: () => "id", now: () => "2026-08-15T00:00:00.000Z" },
    );
    expect(second.getState().busy).toBe(true);

    releaseCodeSession("s1");
    expect(sockets[0]?.closed).toBe(false);
    expect(peekCodeSession("s1")?.refCount).toBe(1);

    releaseCodeSession("s1");
    expect(sockets[0]?.closed).toBe(true);
    expect(peekCodeSession("s1")).toBeUndefined();
  });

  it("reopen hydrates user prompts before the journal replays from after=0", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (after: number, onFrame: (frame: SequencedCodeEventFrame) => void) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: () => "2026-08-15T12:00:02.500Z" },
      async () => [
        {
          id: "t1",
          session_id: "s1",
          ordinal: 1,
          status: "completed",
          user_input: "list the files",
          started_at: "2026-08-15T12:00:00.000Z",
          ended_at: "2026-08-15T12:00:02.500Z",
        },
      ],
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(store.getState().items[0]).toEqual({
      kind: "user",
      id: userItemId("t1"),
      turnId: "t1",
      text: "list the files",
    });
    expect(sockets[0]?.after).toBe(0);

    store.getState().applyEvent(
      {
        seq: 1,
        event: { type: "turn_started", turn_id: "t1" },
        replayed: true,
      },
      { nextId: () => "id", now: () => "2026-08-15T12:00:02.500Z" },
    );
    store.getState().applyEvent(
      {
        seq: 2,
        event: { type: "assistant_delta", text: "README.md" },
        replayed: true,
      },
      { nextId: () => "a1", now: () => "2026-08-15T12:00:02.500Z" },
    );
    expect(store.getState().items.filter((item) => item.kind === "user")).toHaveLength(
      1,
    );
    expect(store.getState().items.find((item) => item.kind === "assistant")).toMatchObject({
      text: "README.md",
    });
  });
});
