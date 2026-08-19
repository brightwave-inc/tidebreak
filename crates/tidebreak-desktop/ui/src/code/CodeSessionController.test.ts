import { afterEach, describe, expect, it, vi } from "vitest";
import type { SequencedCodeEventFrame } from "../api/types";
import { CodeSessionController } from "./CodeSessionController";

class FakeSocket {
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(
    readonly emit: (frame: SequencedCodeEventFrame) => void,
  ) {}

  close() {}
}

afterEach(() => {
  vi.useRealTimers();
});

describe("CodeSessionController", () => {
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
