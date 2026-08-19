import { afterEach, describe, expect, it, vi } from "vitest";
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
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(
    public readonly after: number,
    public readonly emit: (frame: SequencedCodeEventFrame) => void,
  ) {}
  close() {
    this.closed = true;
  }
}

afterEach(() => {
  vi.useRealTimers();
  resetCodeSessionRegistry();
});

describe("CodeSessionRegistry", () => {
  it("marks the session hydrated even when the snapshot cannot be read", async () => {
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => new FakeSocket(after, onFrame) as unknown as WebSocket;

    const store = acquireCodeSession("s1", openSocket, undefined, async () => {
      throw new Error("offline");
    });
    expect(store.getState().hydrated).toBe(false);

    await Promise.resolve();
    await Promise.resolve();
    // The skeleton has to come down either way: a snapshot that never arrives
    // must leave a transcript the reader can send into.
    expect(store.getState().hydrated).toBe(true);
  });

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
          attachments: [],
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
      createdAt: "2026-08-15T12:00:00.000Z",
      attachments: [],
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

  it("records reconnecting from the controller", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };

    const store = acquireCodeSession("s1", openSocket);
    expect(store.getState().connectionState).toBe("live");

    await Promise.resolve();
    await Promise.resolve();
    expect(store.getState().connectionState).toBe("live");
    expect(sockets).toHaveLength(1);

    sockets[0]?.onclose?.();
    expect(store.getState().connectionState).toBe("reconnecting");
  });

  it("fills in the prompt of a turn the socket announces", async () => {
    // A queued follow-up is promoted by the worker, so the client never sees
    // a turn snapshot for it; the same is true of any turn started elsewhere.
    const sockets: FakeSocket[] = [];
    const openSocket = (after: number, onFrame: (frame: SequencedCodeEventFrame) => void) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    let calls = 0;
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: () => "2026-08-15T12:00:02.500Z" },
      async () => {
        calls += 1;
        return calls === 1
          ? []
          : [
              {
                id: "t2",
                session_id: "s1",
                ordinal: 2,
                status: "running" as const,
                user_input: "and run the tests",
                attachments: [],
                started_at: "2026-08-15T12:00:03.000Z",
              },
            ];
      },
    );
    await Promise.resolve();
    await Promise.resolve();

    sockets[0]?.emit({ seq: 1, event: { type: "turn_started", turn_id: "t2" } });
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    expect(store.getState().items).toContainEqual({
      kind: "user",
      id: userItemId("t2"),
      turnId: "t2",
      text: "and run the tests",
      createdAt: "2026-08-15T12:00:03.000Z",
      attachments: [],
    });
    expect(calls).toBe(2);
  });

  it("coalesces a large journal replay into one store publication", async () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const store = acquireCodeSession("s1", openSocket);
    const listener = vi.fn();
    store.subscribe(listener);

    for (let index = 0; index < 200; index += 1) {
      sockets[0]?.emit({
        seq: index + 1,
        replayed: true,
        event: { type: "assistant_delta", text: "x" },
      });
    }

    // Replay is withheld until one scheduled flush rather than forcing React
    // through one external-store render for every historical token.
    expect(store.getState().lastSeq).toBe(0);
    await vi.runAllTimersAsync();
    expect(store.getState().lastSeq).toBe(200);
    expect(store.getState().assistantBuffer).toHaveLength(200);
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("flushes queued replay before the first live frame", () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const store = acquireCodeSession("s1", openSocket);
    const listener = vi.fn();
    store.subscribe(listener);

    sockets[0]?.emit({
      seq: 1,
      replayed: true,
      event: { type: "assistant_delta", text: "old" },
    });
    sockets[0]?.emit({
      seq: 2,
      event: { type: "assistant_delta", text: "live" },
    });

    expect(store.getState().lastSeq).toBe(2);
    expect(store.getState().assistantBuffer).toBe("oldlive");
    expect(listener).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});
