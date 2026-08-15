import type { SequencedCodeEventFrame } from "../api/types";
import {
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  nextReconnectDelay,
} from "../ChatSessionController";

export type CodeConnectionState = "live" | "reconnecting";

export type CodeSessionControllerOptions = {
  /**
   * Open the session's event socket resuming after the given seq. The callback
   * carries parsed frames; the controller decides whether they are current.
   */
  openSocket: (
    after: number,
    onFrame: (frame: SequencedCodeEventFrame) => void,
  ) => WebSocket;
  /** Read the resume cursor freshly on every (re)connect attempt. */
  getAfter: () => number;
  onEvent: (event: SequencedCodeEventFrame) => void;
  onConnectionState: (state: CodeConnectionState) => void;
};

/**
 * Owns one code session's event-stream connection: connect, deliver, and
 * reconnect with the same bounded backoff as chat. Instances are single-use —
 * releasing a registry entry disposes this controller and constructs a new
 * one on the next acquire.
 */
function isWellFormedFrame(frame: SequencedCodeEventFrame): boolean {
  return (
    typeof frame === "object" &&
    frame !== null &&
    Number.isFinite(frame.seq) &&
    typeof frame.event === "object" &&
    frame.event !== null &&
    typeof (frame.event as { type?: unknown }).type === "string"
  );
}

export class CodeSessionController {
  private disposed = false;
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;

  constructor(private readonly options: CodeSessionControllerOptions) {}

  start(): void {
    this.connect();
  }

  /** Close the socket and silence every callback and pending timer, forever. */
  dispose(): void {
    this.disposed = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      this.socket.close();
      this.socket = null;
    }
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.reconnectTimer !== null) return;
    this.options.onConnectionState("reconnecting");
    const delay = this.reconnectDelayMs;
    this.reconnectDelayMs = nextReconnectDelay(this.reconnectDelayMs);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private connect(): void {
    if (this.disposed) return;
    let socket: WebSocket;
    try {
      socket = this.options.openSocket(this.options.getAfter(), (frame) => {
        if (this.disposed || this.socket !== socket) return;
        if (!isWellFormedFrame(frame)) {
          console.error("dropping malformed code event frame", frame);
          return;
        }
        this.options.onEvent(frame);
      });
    } catch {
      this.scheduleReconnect();
      return;
    }

    this.socket = socket;
    socket.onopen = () => {
      if (this.disposed || this.socket !== socket) return;
      this.reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
      this.options.onConnectionState("live");
    };
    socket.onerror = () => {
      if (this.disposed || this.socket !== socket) return;
      socket.close();
      this.scheduleReconnect();
    };
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.scheduleReconnect();
    };
  }
}

export { INITIAL_RECONNECT_DELAY_MS, MAX_RECONNECT_DELAY_MS, nextReconnectDelay };
