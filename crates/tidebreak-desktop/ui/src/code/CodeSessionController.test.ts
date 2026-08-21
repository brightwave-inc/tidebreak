import { afterEach, describe, expect, it, vi } from "vitest";
import type { SequencedCodeEventFrame } from "../api/types";
import { CodeSessionController } from "./CodeSessionController";

class FakeSocket {
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(readonly emit: (frame: SequencedCodeEventFrame) => void) {}

  close() {}
}

afterEach(() => {
  vi.useRealTimers();
});

describe("CodeSessionController", () => {
  it("reveals an empty transcript after the initial socket stays quiet", async () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const batches: Array<{
      events: readonly SequencedCodeEventFrame[];
      settled: boolean;
    }> = [];
    const controller = new CodeSessionController({
      openSocket: (_after, onFrame) => {
        const socket = new FakeSocket(onFrame);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
      getAfter: () => 0,
      onEvents: (events, settled) => batches.push({ events, settled }),
      onConnectionState: () => undefined,
    });
    controller.start();

    sockets[0]?.onopen?.();
    expect(batches).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(39);
    expect(batches).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(batches).toEqual([{ events: [], settled: true }]);

    controller.dispose();
  });

  it("reveals replay as one settled view after the replay burst", async () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const batches: Array<{
      events: readonly SequencedCodeEventFrame[];
      settled: boolean;
    }> = [];
    const controller = new CodeSessionController({
      openSocket: (_after, onFrame) => {
        const socket = new FakeSocket(onFrame);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
      getAfter: () => 0,
      onEvents: (events, settled) => batches.push({ events, settled }),
      onConnectionState: () => undefined,
    });
    controller.start();
    sockets[0]?.onopen?.();
    sockets[0]?.emit({
      seq: 1,
      replayed: true,
      event: { type: "assistant_delta", text: "History" },
    });

    expect(batches).toHaveLength(0);
    await vi.runAllTimersAsync();
    expect(batches).toHaveLength(1);
    expect(batches[0]?.settled).toBe(true);
    expect(batches[0]?.events).toHaveLength(1);

    controller.dispose();
  });

  it("reveals the durable snapshot while reconnecting after an initial failure", () => {
    const batches: Array<{
      events: readonly SequencedCodeEventFrame[];
      settled: boolean;
    }> = [];
    const controller = new CodeSessionController({
      openSocket: () => {
        throw new Error("offline");
      },
      getAfter: () => 0,
      onEvents: (events, settled) => batches.push({ events, settled }),
      onConnectionState: () => undefined,
    });
    controller.start();

    expect(batches).toEqual([{ events: [], settled: true }]);
    controller.dispose();
  });

  it("compacts adjacent replay deltas without losing their final sequence", async () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const batches: (readonly SequencedCodeEventFrame[])[] = [];
    const controller = new CodeSessionController({
      openSocket: (_after, onFrame) => {
        const socket = new FakeSocket(onFrame);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
      getAfter: () => 0,
      onEvents: (events) => batches.push(events),
      onConnectionState: () => undefined,
    });
    controller.start();

    sockets[0]?.emit({
      seq: 1,
      replayed: true,
      event: { type: "turn_started", turn_id: "turn-1" },
    });
    for (let index = 0; index < 10_000; index += 1) {
      sockets[0]?.emit({
        seq: index + 2,
        replayed: true,
        event: { type: "reasoning_delta", text: "x" },
      });
    }
    sockets[0]?.emit({
      seq: 10_002,
      replayed: true,
      event: { type: "assistant_delta", text: "Done." },
    });

    await vi.runAllTimersAsync();

    expect(batches).toHaveLength(1);
    expect(batches[0]).toEqual([
      {
        seq: 1,
        replayed: true,
        event: { type: "turn_started", turn_id: "turn-1" },
      },
      {
        seq: 10_001,
        replayed: true,
        event: { type: "reasoning_delta", text: "x".repeat(10_000) },
      },
      {
        seq: 10_002,
        replayed: true,
        event: { type: "assistant_delta", text: "Done." },
      },
    ]);

    controller.dispose();
  });
});
