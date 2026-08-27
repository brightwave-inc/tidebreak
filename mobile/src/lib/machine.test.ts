import { afterEach, describe, expect, it, vi } from "vitest";
import {
  MachineClient,
  MachineRequestError,
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
    this.onclose?.(new Event("close") as CloseEvent);
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

describe("MachineClient requests", () => {
  it("mints the attached resource and sends JSON without following redirects", async () => {
    const getAccessToken = vi.fn(async () => "machine-token");
    const abort = new AbortController();
    const fetchImpl = vi.fn(
      async (_url: string, _init?: RequestInit) =>
        new Response(JSON.stringify({ accepted: true }), { status: 202 }),
    );
    const client = new MachineClient({
      baseUrl: "https://machine.example",
      resource: "tidebreak:attached",
      tokens: { getAccessToken },
      fetchImpl,
    });

    await expect(
      client.requestJson("/code/sessions/s-1/turns", {
        method: "POST",
        body: { message: "continue" },
        expectedStatus: 202,
        signal: abort.signal,
      }),
    ).resolves.toEqual({ accepted: true });
    expect(getAccessToken).toHaveBeenCalledWith("tidebreak:attached");
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl.mock.calls[0]?.[0]).toBe(
      "https://machine.example/code/sessions/s-1/turns",
    );
    expect(fetchImpl.mock.calls[0]?.[1]).toMatchObject({
      method: "POST",
      redirect: "manual",
      body: JSON.stringify({ message: "continue" }),
      signal: abort.signal,
      headers: {
        Authorization: "Bearer machine-token",
        "Content-Type": "application/json",
      },
    });
  });

  it("preserves stable server errors without reflecting an HTML body", async () => {
    const client = new MachineClient({
      baseUrl: "https://machine.example",
      resource: "tidebreak:attached",
      tokens: { getAccessToken: async () => "machine-token" },
      fetchImpl: async () =>
        new Response(
          JSON.stringify({
            kind: "steering_unavailable",
            message: "This harness cannot steer.",
          }),
          { status: 422 },
        ),
    });
    await expect(
      client.requestJson("/code/sessions/s-1/steer"),
    ).rejects.toMatchObject({
      status: 422,
      kind: "steering_unavailable",
      message: "This harness cannot steer. (HTTP 422)",
    } satisfies Partial<MachineRequestError>);

    const proxyClient = new MachineClient({
      baseUrl: "https://machine.example",
      resource: "tidebreak:attached",
      tokens: { getAccessToken: async () => "machine-token" },
      fetchImpl: async () =>
        new Response("<h1>private proxy detail</h1>", { status: 502 }),
    });
    await expect(proxyClient.getJson("/code/approvals")).rejects.toThrow(
      "Machine request failed. (HTTP 502)",
    );
  });

  it("accepts empty successful responses and refuses malformed JSON", async () => {
    const responses = [
      new Response(null, { status: 202 }),
      new Response(null, { status: 200 }),
      new Response("not-json", { status: 200 }),
    ];
    const client = new MachineClient({
      baseUrl: "https://machine.example",
      resource: "tidebreak:attached",
      tokens: { getAccessToken: async () => "machine-token" },
      fetchImpl: async () => responses.shift()!,
    });

    await expect(
      client.requestJson("/code/sessions/s-1/interrupt", {
        method: "POST",
        expectedStatus: 202,
      }),
    ).resolves.toBeUndefined();
    await expect(
      client.requestJson("/unexpected-status", { expectedStatus: 202 }),
    ).rejects.not.toBeInstanceOf(MachineRequestError);
    await expect(client.getJson("/bad-json")).rejects.toThrow(/valid JSON/);
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

  it("refresh closes the live socket and opens another immediately", async () => {
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
    sockets[0]?.onopen?.(new Event("open"));
    conn.refresh();
    await Promise.resolve();
    expect(sockets[0]?.closed).toBe(true);
    expect(sockets).toHaveLength(2);
    conn.dispose();
  });
});
