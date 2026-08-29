import { afterEach, describe, expect, it, vi } from "vitest";
import type { CodeTurnSnapshot, SequencedCodeEventFrame } from "../api/types";
import {
  acquireCodeSession,
  acquireCodeSessionFromClient,
  applyLiveTurnRewrite,
  MAX_RETAINED_CODE_SESSIONS,
  peekCodeSession,
  releaseCodeSession,
  resetCodeSessionRegistry,
} from "./CodeSessionRegistry";
import { resetCodeClientGenerationForTests } from "./CodeClientGeneration";
import { activateCodeClient } from "./CodeClientScope";
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

const NO_USAGE = {
  input_tokens: 10,
  output_tokens: 4,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
  context_tokens: 0,
};

function turnSnapshot(
  id: string,
  status: CodeTurnSnapshot["status"],
  startedAt: string,
  endedAt?: string,
): CodeTurnSnapshot {
  return {
    id,
    session_id: "s1",
    ordinal: id === "t1" ? 1 : 2,
    status,
    fast_mode: false,
    user_input: id === "t1" ? "list the files" : "run the tests",
    attachments: [],
    started_at: startedAt,
    ...(endedAt ? { ended_at: endedAt, usage: NO_USAGE } : {}),
  };
}

afterEach(() => {
  vi.useRealTimers();
  resetCodeSessionRegistry();
  resetCodeClientGenerationForTests();
});

describe("CodeSessionRegistry", () => {
  it("does not let old session hydration populate a replacement client", async () => {
    const staleTurns = deferred<CodeTurnSnapshot[]>();
    const oldSockets: FakeSocket[] = [];
    const first = {
      listCodeSessionTurns: vi.fn(() => staleTurns.promise),
      openCodeEvents: vi.fn(
        (
          _sessionId: string,
          after: number,
          onFrame: (frame: SequencedCodeEventFrame) => void,
        ) => {
          const socket = new FakeSocket(after, onFrame);
          oldSockets.push(socket);
          return socket as unknown as WebSocket;
        },
      ),
    };
    activateCodeClient(first);
    const oldStore = acquireCodeSessionFromClient("s1", first as never);

    const newSockets: FakeSocket[] = [];
    const replacement = {
      listCodeSessionTurns: vi.fn(async () => [
        turnSnapshot(
          "t2",
          "completed",
          "2026-08-26T12:00:00.000Z",
          "2026-08-26T12:00:01.000Z",
        ),
      ]),
      openCodeEvents: vi.fn(
        (
          _sessionId: string,
          after: number,
          onFrame: (frame: SequencedCodeEventFrame) => void,
        ) => {
          const socket = new FakeSocket(after, onFrame);
          newSockets.push(socket);
          return socket as unknown as WebSocket;
        },
      ),
    };
    activateCodeClient(replacement);
    const newStore = acquireCodeSessionFromClient("s1", replacement as never);
    await flushMicrotasks();

    staleTurns.resolve([
      turnSnapshot(
        "t1",
        "completed",
        "2026-08-25T12:00:00.000Z",
        "2026-08-25T12:00:01.000Z",
      ),
    ]);
    await flushMicrotasks();

    expect(oldSockets).toHaveLength(0);
    expect(newSockets).toHaveLength(1);
    expect(peekCodeSession("s1")?.store).toBe(newStore);
    expect(oldStore.getState().items).toEqual([]);
    expect(newStore.getState().items).toContainEqual(
      expect.objectContaining({ kind: "user", turnId: "t2" }),
    );
    expect(newStore.getState().items).not.toContainEqual(
      expect.objectContaining({ kind: "user", turnId: "t1" }),
    );
  });

  it("marks the session hydrated even when the snapshot cannot be read", async () => {
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

    const store = acquireCodeSession("s1", openSocket, undefined, async () => {
      throw new Error("offline");
    });
    expect(store.getState().hydrated).toBe(false);

    await Promise.resolve();
    await Promise.resolve();
    expect(store.getState().hydrated).toBe(false);
    sockets[0]?.onopen?.();
    await vi.runAllTimersAsync();
    // The skeleton has to come down either way once the initial journal is
    // quiet: a snapshot that never arrives must still leave a transcript the
    // reader can send into.
    expect(store.getState().hydrated).toBe(true);
  });

  it("shares one store and parks it after the last release", () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };

    const first = acquireCodeSession("s1", openSocket);
    const second = acquireCodeSession("s1", openSocket);
    expect(first).toBe(second);
    expect(peekCodeSession("s1")?.refCount).toBe(2);
    expect(sockets).toHaveLength(1);

    first
      .getState()
      .applyEvent(
        { seq: 1, event: { type: "turn_started", turn_id: "t1" } },
        { nextId: () => "id", now: () => "2026-08-15T00:00:00.000Z" },
      );
    expect(second.getState().busy).toBe(true);

    releaseCodeSession("s1");
    expect(sockets[0]?.closed).toBe(false);
    expect(peekCodeSession("s1")?.refCount).toBe(1);

    releaseCodeSession("s1");
    expect(sockets[0]?.closed).toBe(true);
    expect(peekCodeSession("s1")).toMatchObject({
      controller: null,
      refCount: 0,
    });
    expect(first.getState().connectionState).toBe("reconnecting");
  });

  it("reopens a parked store from its last sequence without hydrating again", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const hydrateTurns = vi.fn(async () => [
      {
        id: "t1",
        session_id: "s1",
        ordinal: 1,
        status: "completed" as const,
        fast_mode: false,
        user_input: "list the files",
        attachments: [],
        started_at: "2026-08-15T12:00:00.000Z",
        ended_at: "2026-08-15T12:00:02.500Z",
      },
    ]);
    const first = acquireCodeSession("s1", openSocket, undefined, hydrateTurns);
    await Promise.resolve();
    await Promise.resolve();
    first
      .getState()
      .applyEvent(
        { seq: 7, event: { type: "turn_started", turn_id: "t2" } },
        { nextId: () => "id", now: () => "2026-08-15T12:00:03.000Z" },
      );

    releaseCodeSession("s1");
    const reopened = acquireCodeSession(
      "s1",
      openSocket,
      undefined,
      hydrateTurns,
    );

    expect(reopened).toBe(first);
    expect(reopened.getState().items).toContainEqual({
      kind: "user",
      id: userItemId("t1"),
      turnId: "t1",
      text: "list the files",
      createdAt: "2026-08-15T12:00:00.000Z",
      attachments: [],
    });
    expect(hydrateTurns).toHaveBeenCalledTimes(1);
    expect(sockets.map((socket) => socket.after)).toEqual([0, 7]);
  });

  it("retries initial turn hydration when the first open could not read it", async () => {
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => new FakeSocket(after, onFrame) as unknown as WebSocket;
    const hydrateTurns = vi
      .fn<() => Promise<[]>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([]);

    acquireCodeSession("s1", openSocket, undefined, hydrateTurns);
    await Promise.resolve();
    await Promise.resolve();
    releaseCodeSession("s1");

    acquireCodeSession("s1", openSocket, undefined, hydrateTurns);
    await Promise.resolve();
    await Promise.resolve();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
  });

  it("evicts the least recently parked store when the cache is full", () => {
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => new FakeSocket(after, onFrame) as unknown as WebSocket;

    for (let index = 0; index <= MAX_RETAINED_CODE_SESSIONS; index += 1) {
      const sessionId = `s${index}`;
      acquireCodeSession(sessionId, openSocket);
      releaseCodeSession(sessionId);
    }

    expect(peekCodeSession("s0")).toBeUndefined();
    expect(peekCodeSession("s1")?.refCount).toBe(0);
    expect(peekCodeSession(`s${MAX_RETAINED_CODE_SESSIONS}`)?.refCount).toBe(0);
  });

  it("reopen hydrates user prompts before the journal replays from after=0", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
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
          fast_mode: false,
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
    expect(
      store.getState().items.filter((item) => item.kind === "user"),
    ).toHaveLength(1);
    expect(
      store.getState().items.find((item) => item.kind === "assistant"),
    ).toMatchObject({
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

  it("keeps live turn timing when reconnect replays the terminal row", async () => {
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
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce("2026-08-15T12:00:00.000Z")
      .mockReturnValue("2026-08-15T12:00:02.500Z");
    const store = acquireCodeSession("s1", openSocket, {
      nextId: () => "id",
      now,
    });

    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    sockets[0]?.onclose?.();
    await vi.runOnlyPendingTimersAsync();

    expect(sockets.map((socket) => socket.after)).toEqual([0, 1]);
    sockets[1]?.emit({
      seq: 2,
      replayed: true,
      event: {
        type: "turn_completed",
        usage: {
          input_tokens: 10,
          output_tokens: 4,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
          context_tokens: 0,
        },
      },
    });
    await vi.advanceTimersByTimeAsync(40);

    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(2);
  });

  it("does not reopen a finished turn when delayed prompt hydration resolves", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const promptRead = deferred<CodeTurnSnapshot[]>();
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([])
      .mockImplementationOnce(() => promptRead.promise);
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce("2026-08-15T12:00:00.000Z")
      .mockReturnValue("2026-08-15T12:00:02.500Z");
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    sockets[0]?.emit({
      seq: 2,
      event: { type: "turn_completed", usage: NO_USAGE },
    });

    promptRead.resolve([
      turnSnapshot("t1", "running", "2026-08-15T12:00:00.000Z"),
    ]);
    await flushMicrotasks();

    expect(store.getState()).toMatchObject({
      busy: false,
      activeTurnId: null,
      turnStartedAt: null,
      lifecycle: "idle",
    });
    expect(store.getState().items).toContainEqual({
      kind: "user",
      id: userItemId("t1"),
      turnId: "t1",
      text: "list the files",
      createdAt: "2026-08-15T12:00:00.000Z",
      attachments: [],
    });
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
  });

  it("does not replace a newer active turn when an older prompt read resolves", async () => {
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const t1PromptRead = deferred<CodeTurnSnapshot[]>();
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([])
      .mockImplementationOnce(() => t1PromptRead.promise)
      .mockResolvedValueOnce([
        turnSnapshot("t2", "running", "2026-08-15T12:00:03.000Z"),
      ]);
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce("2026-08-15T12:00:00.000Z")
      .mockReturnValueOnce("2026-08-15T12:00:02.500Z")
      .mockReturnValue("2026-08-15T12:00:03.000Z");
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    sockets[0]?.emit({
      seq: 2,
      event: { type: "turn_completed", usage: NO_USAGE },
    });
    sockets[0]?.emit({
      seq: 3,
      event: { type: "turn_started", turn_id: "t2" },
    });
    await flushMicrotasks();

    expect(store.getState()).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
      turnStartedAt: "2026-08-15T12:00:03.000Z",
      lifecycle: "running",
    });

    t1PromptRead.resolve([
      turnSnapshot("t1", "running", "2026-08-15T12:00:00.000Z"),
    ]);
    await flushMicrotasks();

    expect(store.getState()).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
      turnStartedAt: "2026-08-15T12:00:03.000Z",
      lifecycle: "running",
    });
    expect(store.getState().items).toContainEqual({
      kind: "user",
      id: userItemId("t1"),
      turnId: "t1",
      text: "list the files",
      createdAt: "2026-08-15T12:00:00.000Z",
      attachments: [],
    });
  });

  it("refreshes exact duration after a replay terminal lacks a durable boundary", async () => {
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
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([
        turnSnapshot("t1", "running", "2026-08-15T20:41:23.000Z"),
      ])
      .mockResolvedValueOnce([
        turnSnapshot(
          "t1",
          "completed",
          "2026-08-15T20:41:23.000Z",
          "2026-08-15T20:42:28.000Z",
        ),
      ]);
    const now = vi.fn<() => string>();
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 1,
      replayed: true,
      event: { type: "turn_started", turn_id: "t1" },
    });
    sockets[0]?.emit({
      seq: 2,
      replayed: true,
      event: { type: "turn_completed", usage: NO_USAGE },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    expect(store.getState()).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 65_000,
    });
    expect(now).not.toHaveBeenCalled();
  });

  it("settles a hydrated turn when capped replay contains only its terminal", async () => {
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
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([
        turnSnapshot("t2", "running", "2026-08-15T12:00:00.000Z"),
      ])
      .mockResolvedValueOnce([
        turnSnapshot(
          "t2",
          "completed",
          "2026-08-15T12:00:00.000Z",
          "2026-08-15T12:00:02.500Z",
        ),
      ]);
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 2_001,
      replayed: true,
      truncated: true,
      event: { type: "turn_completed", usage: NO_USAGE },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    expect(store.getState()).toMatchObject({
      lastSeq: 2_001,
      busy: false,
      activeTurnId: null,
      journalTurnId: null,
      turnStartedAt: null,
      lifecycle: "idle",
      lastUsage: NO_USAGE,
    });
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      durationMs: 2_500,
    });
  });

  it("keeps a capped failure unassigned after initial hydration already saw it end", async () => {
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
    const failed = turnSnapshot(
      "t2",
      "failed",
      "2026-08-15T12:00:00.000Z",
      "2026-08-15T12:00:02.500Z",
    );
    const hydrateTurns = vi.fn(async () => [failed]);
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      error: null,
    });
    sockets[0]?.emit({
      seq: 2_001,
      replayed: true,
      truncated: true,
      event: {
        type: "turn_failed",
        error: { message: "compiler crashed: missing libssl" },
      },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    expect(store.getState().pendingTerminalReconciliations.size).toBe(1);
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
  });

  it("keeps a capped failure unassigned after initial hydration was unavailable", async () => {
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
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([
        turnSnapshot(
          "t2",
          "failed",
          "2026-08-15T12:00:00.000Z",
          "2026-08-15T12:00:02.500Z",
        ),
      ]);
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 2_001,
      replayed: true,
      truncated: true,
      event: {
        type: "turn_failed",
        error: { message: "compiler crashed: missing libssl" },
      },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    expect(store.getState().pendingTerminalReconciliations.size).toBe(1);
    expect(store.getState()).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
  });

  it("retries an unresolved terminal refresh with bounded backoff", async () => {
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
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([
        turnSnapshot("t2", "running", "2026-08-15T12:00:00.000Z"),
      ])
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([
        turnSnapshot(
          "t2",
          "failed",
          "2026-08-15T12:00:00.000Z",
          "2026-08-15T12:00:02.500Z",
        ),
      ]);
    const store = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 2_001,
      replayed: true,
      truncated: true,
      event: {
        type: "turn_failed",
        error: { message: "compiler crashed: missing libssl" },
      },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    expect(store.getState().pendingTerminalReconciliations.size).toBe(1);
    expect(store.getState()).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      lifecycle: "running",
    });

    await vi.advanceTimersByTimeAsync(249);
    expect(hydrateTurns).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    await flushMicrotasks();

    expect(hydrateTurns).toHaveBeenCalledTimes(3);
    expect(store.getState().pendingTerminalReconciliations.size).toBe(1);
    expect(store.getState()).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(store.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
  });

  it("keeps an unattributed capped terminal pending across a retained reconnect", async () => {
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
    const hydrateTurns = vi
      .fn<() => Promise<CodeTurnSnapshot[]>>()
      .mockResolvedValueOnce([
        turnSnapshot("t2", "running", "2026-08-15T12:00:00.000Z"),
      ])
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([
        turnSnapshot(
          "t2",
          "failed",
          "2026-08-15T12:00:00.000Z",
          "2026-08-15T12:00:02.500Z",
        ),
      ]);
    const first = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    sockets[0]?.emit({
      seq: 2_001,
      replayed: true,
      truncated: true,
      event: {
        type: "turn_failed",
        error: { message: "compiler crashed: missing libssl" },
      },
    });
    await vi.advanceTimersByTimeAsync(40);
    await flushMicrotasks();
    expect(first.getState().pendingTerminalReconciliations.size).toBe(1);

    releaseCodeSession("s1");
    const reopened = acquireCodeSession(
      "s1",
      openSocket,
      { nextId: () => "id", now: vi.fn<() => string>() },
      hydrateTurns,
    );
    await flushMicrotasks();

    expect(reopened).toBe(first);
    expect(hydrateTurns).toHaveBeenCalledTimes(3);
    expect(sockets.map((socket) => socket.after)).toEqual([0, 2_001]);
    expect(reopened.getState().pendingTerminalReconciliations.size).toBe(1);
    expect(reopened.getState()).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(reopened.getState().items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
  });

  it("fills in the prompt of a turn the socket announces", async () => {
    // A queued follow-up is promoted by the worker, so the client never sees
    // a turn snapshot for it; the same is true of any turn started elsewhere.
    const sockets: FakeSocket[] = [];
    const openSocket = (
      after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) => {
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
                fast_mode: false,
                user_input: "and run the tests",
                attachments: [],
                started_at: "2026-08-15T12:00:03.000Z",
              },
            ];
      },
    );
    await Promise.resolve();
    await Promise.resolve();

    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t2" },
    });
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

  it("stamps a live recap onto the session store so snapshot reconnect keeps it", () => {
    const sockets: FakeSocket[] = [];
    const store = acquireCodeSession("s1", (after, onFrame) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    sockets[0]?.emit({
      seq: 1,
      event: { type: "assistant_delta", text: "The original closing message." },
      transient: true,
    });
    applyLiveTurnRewrite("s1", "t1", "rewritten", "The recap.");
    const assistant = store
      .getState()
      .items.find((item) => item.kind === "assistant" && item.turnId === "t1");
    expect(assistant).toMatchObject({
      rewrite: "The recap.",
      rewriteState: "rewritten",
    });
    expect(store.getState().storedRewrites.t1).toBe("The recap.");
  });

  it("leaves rewriting notices off the session store", () => {
    const sockets: FakeSocket[] = [];
    const store = acquireCodeSession("s1", (after, onFrame) => {
      const socket = new FakeSocket(after, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    });
    sockets[0]?.emit({
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    sockets[0]?.emit({
      seq: 1,
      event: { type: "assistant_delta", text: "The original closing message." },
      transient: true,
    });
    applyLiveTurnRewrite("s1", "t1", "rewriting");
    const assistant = store
      .getState()
      .items.find((item) => item.kind === "assistant" && item.turnId === "t1");
    expect(assistant).toMatchObject({
      text: "The original closing message.",
    });
    expect(assistant).not.toMatchObject({ rewriteState: "rewriting" });
    expect(store.getState().storedRewrites.t1).toBeUndefined();
  });
});
