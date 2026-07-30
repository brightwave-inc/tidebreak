import type { ChatFrame, ChatMetadataFrame, SequencedEvent } from "./api";

export const INITIAL_RECONNECT_DELAY_MS = 250;
export const MAX_RECONNECT_DELAY_MS = 5_000;

/** The next bounded backoff value after scheduling one reconnect attempt. */
export function nextReconnectDelay(delayMs: number): number {
  return Math.min(delayMs * 2, MAX_RECONNECT_DELAY_MS);
}

export type ChatConnectionState = "live" | "reconnecting";

export type ChatSessionControllerOptions = {
  /**
   * Open the chat's event socket resuming after the given seq. The callback
   * carries parsed frames; the controller decides whether they are current.
   */
  openSocket: (
    after: number,
    onFrame: (frame: ChatFrame) => void,
  ) => WebSocket;
  /** Read the resume cursor freshly on every (re)connect attempt. */
  getAfter: () => number;
  onEvent: (event: SequencedEvent) => void;
  /**
   * Chat metadata that arrived on the socket without being turn history.
   *
   * Separate from `onEvent` because it carries no sequence: it is not resumed,
   * not deduplicated, and never advances the cursor the session reducer keeps.
   */
  onMetadata: (metadata: ChatMetadataFrame) => void;
  onConnectionState: (state: ChatConnectionState) => void;
};

/**
 * Owns one chat's event-stream connection: connect, deliver, and reconnect
 * with bounded backoff. Instances are single-use — switching chats means
 * disposing this controller and constructing a new one, which is what fences
 * stale sockets and timers (no generation counters, no borrowed refs).
 */
/**
 * Minimal envelope check for a sequenced frame: a finite seq and an event
 * object with a string type. Event payloads are typed downstream; unknown
 * types are tolerated there, but a frame without this shape is undecodable.
 */
function isWellFormedFrame(frame: SequencedEvent): boolean {
  return (
    typeof frame === "object" &&
    frame !== null &&
    Number.isFinite(frame.seq) &&
    typeof frame.event === "object" &&
    frame.event !== null &&
    typeof (frame.event as { type?: unknown }).type === "string"
  );
}

/**
 * Whether a frame is a metadata notice rather than a journaled event.
 *
 * The two are told apart by the `metadata` discriminator, which is the only
 * thing they have in common with each other: a metadata frame has no sequence,
 * so any check based on one would classify it as malformed.
 */
function metadataFrame(frame: ChatFrame): ChatMetadataFrame | null {
  if (typeof frame !== "object" || frame === null) return null;
  const metadata = (frame as { metadata?: unknown }).metadata;
  if (metadata === "titled") {
    const { title } = frame as { title?: unknown };
    return typeof title === "string" ? { metadata, title } : null;
  }
  if (metadata === "file_changes_recorded") {
    const { turn_id } = frame as { turn_id?: unknown };
    return typeof turn_id === "string" ? { metadata, turn_id } : null;
  }
  return null;
}

export class ChatSessionController {
  private disposed = false;
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;

  constructor(private readonly options: ChatSessionControllerOptions) {}

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
        const metadata = metadataFrame(frame);
        if (metadata) {
          this.options.onMetadata(metadata);
          return;
        }
        const event = frame as SequencedEvent;
        if (!isWellFormedFrame(event)) {
          console.error("dropping malformed event frame", event);
          return;
        }
        this.options.onEvent(event);
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
      // Browser error events are not required to be followed by close. Close
      // this socket so the one reconnect path owns recovery in either case.
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
