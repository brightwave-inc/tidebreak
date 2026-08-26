import { afterEach, describe, expect, it, vi } from "vitest";
import {
  WS_HANDSHAKE,
  WS_TOKEN_PREFIX,
  connectWithBackoff,
  machineWsUrl,
} from "./machine";
import { INITIAL_RECONNECT_DELAY_MS } from "./reconnect";

afterEach(() => {
  vi.useRealTimers();
});

class FakeSocket {
  onopen: ((event: Event) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  closed = false;
  protocols: string[];

  constructor(_url: string, protocols?: string | string[]) {
    this.protocols = Array.isArray(protocols)
      ? protocols
      : protocols
        ? [protocols]
        : [];
  }

  close() {
    this.closed = true;
    this.onclose?.(new CloseEvent("close"));
  }
}

describe("machineWsUrl", () => {
  it("maps http(s) to ws(s)", () => {
    expect(machineWsUrl("https://machine.example/app", "/code/updates")).toBe(
      "wss://machine.example/app/code/updates",
    );
    expect(machineWsUrl("http://127.0.0.1:8080", "/code/updates")).toBe(
      "ws://127.0.0.1:8080/code/updates",
    );
  });
});

describe("connectWithBackoff", () => {
  it("remints a token on each connect and offers both subprotocols", async () => {
    const sockets: FakeSocket[] = [];
    let tokens = 0;
    const conn = connectWithBackoff(
      async () => {
        tokens += 1;
        const socket = new FakeSocket("wss://example/code/updates", [
          WS_HANDSHAKE,
          `${WS_TOKEN_PREFIX}tok-${tokens}`,
        ]);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
      { onMessage: () => undefined },
      { random: () => 0.5 },
    );
    conn.start();
    await Promise.resolve();
    expect(sockets).toHaveLength(1);
    expect(sockets[0]?.protocols).toEqual([
      WS_HANDSHAKE,
      `${WS_TOKEN_PREFIX}tok-1`,
    ]);
    sockets[0]?.onopen?.(new Event("open"));
    sockets[0]?.close();
    vi.useFakeTimers();
    conn.dispose();
  });

  it("reconnects after close using capped backoff", async () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    const conn = connectWithBackoff(
      async () => {
        const socket = new FakeSocket("wss://example/code/updates");
        sockets.push(socket);
        return socket as unknown as WebSocket;
      },
      { onMessage: () => undefined },
      { random: () => 0.5 },
    );
    conn.start();
    await Promise.resolve();
    expect(sockets).toHaveLength(1);
    sockets[0]?.onopen?.(new Event("open"));
    sockets[0]?.close();
    await vi.advanceTimersByTimeAsync(INITIAL_RECONNECT_DELAY_MS);
    await Promise.resolve();
    expect(sockets.length).toBeGreaterThanOrEqual(2);
    conn.dispose();
  });
});
