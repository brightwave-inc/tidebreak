import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatFrame, ChatMetadataFrame, SequencedEvent } from "./api";
import {
  ChatSessionController,
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  nextReconnectDelay,
  type ChatConnectionState,
} from "./ChatSessionController";

class FakeSocket {
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  closed = false;
  constructor(
    public readonly after: number,
    public readonly emit: (frame: ChatFrame) => void,
  ) {}
  close() {
    this.closed = true;
    // Browsers deliver close asynchronously; synchronous delivery here would
    // recurse into the controller mid-teardown. Emitting nothing matches the
    // stricter case (a close event that never arrives).
  }
}

function harness() {
  const sockets: FakeSocket[] = [];
  const events: SequencedEvent[] = [];
  const metadata: ChatMetadataFrame[] = [];
  const states: ChatConnectionState[] = [];
  let after = 0;
  let failNextOpen = false;
  const controller = new ChatSessionController({
    openSocket: (cursor, onFrame) => {
      if (failNextOpen) {
        failNextOpen = false;
        throw new Error("boom");
      }
      const socket = new FakeSocket(cursor, onFrame);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
    getAfter: () => after,
    onEvent: (event) => events.push(event),
    onMetadata: (notice) => metadata.push(notice),
    onConnectionState: (state) => states.push(state),
  });
  return {
    controller,
    sockets,
    events,
    metadata,
    states,
    setAfter: (value: number) => (after = value),
    failNext: () => (failNextOpen = true),
    latest: () => sockets[sockets.length - 1],
  };
}

const FRAME: SequencedEvent = {
  seq: 1,
  event: { type: "turn_started", turn_id: "t" },
};

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("nextReconnectDelay", () => {
  it("doubles up to the cap", () => {
    expect(nextReconnectDelay(INITIAL_RECONNECT_DELAY_MS)).toBe(500);
    expect(nextReconnectDelay(4_000)).toBe(MAX_RECONNECT_DELAY_MS);
    expect(nextReconnectDelay(MAX_RECONNECT_DELAY_MS)).toBe(
      MAX_RECONNECT_DELAY_MS,
    );
  });
});

describe("ChatSessionController", () => {
  it("connects, reports live, and forwards events from the current socket", () => {
    const h = harness();
    h.controller.start();
    expect(h.sockets).toHaveLength(1);
    h.latest().onopen?.();
    expect(h.states).toEqual(["live"]);
    h.latest().emit(FRAME);
    expect(h.events).toEqual([FRAME]);
  });

  it("reconnects after close with exponential backoff and a fresh cursor", () => {
    const h = harness();
    h.controller.start();
    h.latest().onopen?.();

    h.setAfter(42);
    h.latest().onclose?.();
    expect(h.states).toEqual(["live", "reconnecting"]);
    expect(h.sockets).toHaveLength(1);

    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    expect(h.sockets).toHaveLength(2);
    expect(h.latest().after).toBe(42);

    // Second drop before opening: the delay doubles.
    h.latest().onclose?.();
    vi.advanceTimersByTime(499);
    expect(h.sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(h.sockets).toHaveLength(3);
  });

  it("resets the backoff once a connection opens", () => {
    const h = harness();
    h.controller.start();
    h.latest().onclose?.();
    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    h.latest().onclose?.();
    vi.advanceTimersByTime(500);
    expect(h.sockets).toHaveLength(3);

    h.latest().onopen?.();
    h.latest().onclose?.();
    // Delay is back to the initial value, not 1s.
    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    expect(h.sockets).toHaveLength(4);
  });

  it("treats error as close-and-recover even when close never fires", () => {
    const h = harness();
    h.controller.start();
    h.latest().onerror?.();
    expect(h.latest().closed).toBe(true);
    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    expect(h.sockets).toHaveLength(2);
  });

  it("schedules a retry when opening the socket throws synchronously", () => {
    const h = harness();
    h.failNext();
    h.controller.start();
    expect(h.sockets).toHaveLength(0);
    expect(h.states).toEqual(["reconnecting"]);
    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    expect(h.sockets).toHaveLength(1);
  });

  it("dispose closes the socket and silences events, timers, and callbacks", () => {
    const h = harness();
    h.controller.start();
    const socket = h.latest();
    h.controller.dispose();
    expect(socket.closed).toBe(true);

    socket.emit(FRAME);
    socket.onopen?.();
    socket.onclose?.();
    vi.runAllTimers();
    expect(h.events).toEqual([]);
    expect(h.states).toEqual([]);
    expect(h.sockets).toHaveLength(1);
  });

  it("ignores frames from a superseded socket", () => {
    const h = harness();
    h.controller.start();
    const stale = h.latest();
    stale.onerror?.();
    vi.advanceTimersByTime(INITIAL_RECONNECT_DELAY_MS);
    expect(h.sockets).toHaveLength(2);

    stale.emit(FRAME);
    expect(h.events).toEqual([]);
    h.latest().emit(FRAME);
    expect(h.events).toEqual([FRAME]);
  });
});

describe("frame validation", () => {
  it("drops undecodable frames instead of forwarding them", () => {
    const h = harness();
    h.controller.start();
    h.latest().emit(null as unknown as SequencedEvent);
    h.latest().emit({ seq: Number.NaN, event: FRAME.event });
    h.latest().emit({ seq: 2, event: "nope" } as unknown as SequencedEvent);
    h.latest().emit({
      seq: 3,
      event: { type: 42 },
    } as unknown as SequencedEvent);
    expect(h.events).toEqual([]);

    h.latest().emit({
      seq: 4,
      event: { type: "future_thing" },
    } as unknown as SequencedEvent);
    expect(h.events).toHaveLength(1);
  });

  /**
   * A metadata frame has no sequence, so the sequenced-frame check would call it
   * malformed and drop it. Routing it separately is what keeps the chat's name
   * off the cursor the reducer resumes from.
   */
  it("routes a metadata frame away from the sequenced stream", () => {
    const h = harness();
    h.controller.start();
    h.latest().emit({ metadata: "titled", title: "Q3 revenue reconciliation" });
    h.latest().emit({
      metadata: "file_changes_recorded",
      turn_id: "turn-1",
    });
    h.latest().emit({
      metadata: "memory_proposals_recorded",
      turn_id: "turn-1",
    });

    expect(h.metadata).toEqual([
      { metadata: "titled", title: "Q3 revenue reconciliation" },
      { metadata: "file_changes_recorded", turn_id: "turn-1" },
      { metadata: "memory_proposals_recorded", turn_id: "turn-1" },
    ]);
    expect(h.events).toEqual([]);
  });

  it("drops a metadata frame that carries no title", () => {
    const h = harness();
    h.controller.start();
    h.latest().emit({ metadata: "titled" } as unknown as ChatFrame);
    h.latest().emit({ metadata: "titled", title: 7 } as unknown as ChatFrame);

    expect(h.metadata).toEqual([]);
    expect(h.events).toEqual([]);
  });
});
