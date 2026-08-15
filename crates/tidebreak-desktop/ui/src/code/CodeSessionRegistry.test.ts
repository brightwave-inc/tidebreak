import { afterEach, describe, expect, it } from "vitest";
import type { SequencedCodeEventFrame } from "../api/types";
import {
  acquireCodeSession,
  peekCodeSession,
  releaseCodeSession,
  resetCodeSessionRegistry,
} from "./CodeSessionRegistry";

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
});
