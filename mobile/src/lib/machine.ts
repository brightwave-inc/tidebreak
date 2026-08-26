import { fetchRefusingRedirects, type HttpFetch } from "./http";
import {
  INITIAL_RECONNECT_DELAY_MS,
  jitteredDelay,
  nextReconnectDelay,
} from "./reconnect";

export const WS_HANDSHAKE = "tidebreak-v1";
export const WS_TOKEN_PREFIX = "tidebreak-token.";

export type TokenSource = {
  getAccessToken: (resource: string) => Promise<string>;
};

export type MachineClientOptions = {
  baseUrl: string;
  resource: string;
  tokens: TokenSource;
  fetchImpl?: HttpFetch;
  webSocket?: typeof WebSocket;
  now?: () => number;
  random?: () => number;
  setTimeoutFn?: typeof setTimeout;
  clearTimeoutFn?: typeof clearTimeout;
};

function httpToWs(url: string): string {
  if (url.startsWith("https://")) return `wss://${url.slice("https://".length)}`;
  if (url.startsWith("http://")) return `ws://${url.slice("http://".length)}`;
  return url;
}

export function machineWsUrl(baseUrl: string, path: string): string {
  const trimmed = baseUrl.replace(/\/$/, "");
  return `${httpToWs(trimmed)}${path.startsWith("/") ? path : `/${path}`}`;
}

export class MachineClient {
  constructor(private readonly options: MachineClientOptions) {}

  async getJson(path: string): Promise<unknown> {
    const token = await this.options.tokens.getAccessToken(
      this.options.resource,
    );
    const url = `${this.options.baseUrl.replace(/\/$/, "")}${path.startsWith("/") ? path : `/${path}`}`;
    const response = await fetchRefusingRedirects(
      url,
      { headers: { Authorization: `Bearer ${token}` } },
      this.options.fetchImpl,
    );
    if (!response.ok) {
      throw new Error(`Machine request failed (HTTP ${response.status})`);
    }
    return response.json();
  }

  /**
   * Open a machine WebSocket. The token is reminted on every connect because
   * access tokens expire in ~10 minutes and the server revalidates every 60s.
   */
  async openSocket(path: string): Promise<WebSocket> {
    const token = await this.options.tokens.getAccessToken(
      this.options.resource,
    );
    const url = machineWsUrl(this.options.baseUrl, path);
    const Ctor = this.options.webSocket ?? WebSocket;
    return new Ctor(url, [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${token}`]);
  }
}

export type ReconnectingSocketHandlers = {
  onOpen?: () => void;
  onMessage: (data: string) => void;
  onConnectionState?: (state: "live" | "reconnecting") => void;
};

export type ReconnectingSocket = {
  start: () => void;
  refresh: () => void;
  dispose: () => void;
};

/**
 * Connect, treat every close as a normal reconnect, remint the token each
 * time, and back off with cap + jitter.
 */
export function connectWithBackoff(
  open: () => Promise<WebSocket>,
  handlers: ReconnectingSocketHandlers,
  timing: {
    setTimeoutFn?: typeof setTimeout;
    clearTimeoutFn?: typeof clearTimeout;
    random?: () => number;
  } = {},
): ReconnectingSocket {
  const setTimer = timing.setTimeoutFn ?? setTimeout;
  const clearTimer = timing.clearTimeoutFn ?? clearTimeout;
  const random = timing.random ?? Math.random;

  let disposed = false;
  let socket: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let delayMs = INITIAL_RECONNECT_DELAY_MS;
  let generation = 0;

  function clearReconnect(): void {
    if (timer !== null) {
      clearTimer(timer);
      timer = null;
    }
  }

  function scheduleReconnect(): void {
    if (disposed || timer !== null) return;
    handlers.onConnectionState?.("reconnecting");
    const wait = jitteredDelay(delayMs, random);
    delayMs = nextReconnectDelay(delayMs);
    timer = setTimer(() => {
      timer = null;
      void connect();
    }, wait);
  }

  async function connect(): Promise<void> {
    if (disposed) return;
    const born = ++generation;
    let next: WebSocket;
    try {
      next = await open();
    } catch {
      scheduleReconnect();
      return;
    }
    if (disposed || born !== generation) {
      next.close();
      return;
    }
    socket = next;
    next.onopen = () => {
      if (disposed || socket !== next) return;
      delayMs = INITIAL_RECONNECT_DELAY_MS;
      handlers.onConnectionState?.("live");
      handlers.onOpen?.();
    };
    next.onmessage = (event) => {
      if (disposed || socket !== next) return;
      const data =
        typeof event.data === "string" ? event.data : String(event.data);
      handlers.onMessage(data);
    };
    next.onerror = () => {
      if (socket !== next) return;
      next.close();
    };
    next.onclose = () => {
      if (socket !== next) return;
      socket = null;
      scheduleReconnect();
    };
  }

  return {
    start() {
      void connect();
    },
    refresh() {
      clearReconnect();
      delayMs = INITIAL_RECONNECT_DELAY_MS;
      if (socket) {
        const current = socket;
        socket = null;
        current.close();
      }
      void connect();
    },
    dispose() {
      disposed = true;
      generation += 1;
      clearReconnect();
      if (socket) {
        socket.onopen = null;
        socket.onmessage = null;
        socket.onerror = null;
        socket.onclose = null;
        socket.close();
        socket = null;
      }
    },
  };
}
