import type { ChatFrame, ChatMetadataFrame, SequencedEvent } from "./api";

export const INITIAL_RECONNECT_DELAY_MS = 250;
export const MAX_RECONNECT_DELAY_MS = 5_000;

/**
 * How long the socket must stay quiet before a replay burst counts as done.
 * The same window code mode uses: long enough for a local journal to drain,
 * short enough that nobody notices the wait.
 */
export const CHAT_REPLAY_SETTLE_MS = 40;

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
  openSocket: (after: number, onFrame: (frame: ChatFrame) => void) => WebSocket;
  /** Read the resume cursor freshly on every (re)connect attempt. */
  getAfter: () => number;
  /**
   * Deliver an ordered run of events. A live frame arrives on its own, as
   * soon as it lands. Replayed history is held until the burst goes quiet and
   * then delivered in one call, with consecutive text fragments already
   * joined, so rebuilding a turn costs one render instead of one per frame.
   */
  onEvents: (events: readonly SequencedEvent[]) => void;
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
export function metadataFrame(frame: ChatFrame): ChatMetadataFrame | null {
  if (typeof frame !== "object" || frame === null) return null;
  const metadata = (frame as { metadata?: unknown }).metadata;
  if (metadata === "titled") {
    const { title } = frame as { title?: unknown };
    return typeof title === "string" ? { metadata, title } : null;
  }
  if (metadata === "sandbox_preparing") {
    const { preparing } = frame as { preparing?: unknown };
    return typeof preparing === "boolean" ? { metadata, preparing } : null;
  }
  if (metadata === "file_changes_recorded") {
    const { turn_id } = frame as { turn_id?: unknown };
    return typeof turn_id === "string" ? { metadata, turn_id } : null;
  }
  if (metadata === "memory_proposals_recorded") {
    const { turn_id } = frame as { turn_id?: unknown };
    return typeof turn_id === "string" ? { metadata, turn_id } : null;
  }
  return null;
}

/** A run of consecutive text fragments, held as chunks until it is flushed. */
type HeldText = {
  type: "text_delta" | "reasoning_delta";
  seq: number;
  chunks: string[];
};

export class ChatSessionController {
  private disposed = false;
  private socket: WebSocket | null = null;
  /** The socket opened and has not dropped since. */
  private live = false;
  /** Callers of {@link retryNow} waiting for the attempt to settle. */
  private settleWaiters: (() => void)[] = [];
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
  private replayFrames: SequencedEvent[] = [];
  private replayText: HeldText | null = null;
  private replayFlush: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly options: ChatSessionControllerOptions) {}

  start(): void {
    this.connect();
  }

  /**
   * Reconnect now instead of when the backoff next fires: the connection
   * notice's Retry now. The backoff starts over from its first step.
   *
   * Resolves once this attempt opens or fails, so the button can wait for
   * its answer. A live socket has nothing to retry, and an attempt already
   * under way is not doubled: the call waits for that one.
   */
  retryNow(): Promise<void> {
    if (this.disposed || this.live) return Promise.resolve();
    const settled = new Promise<void>((resolve) => {
      this.settleWaiters.push(resolve);
    });
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
      this.reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
      this.connect();
    }
    return settled;
  }

  private settle(): void {
    const waiters = this.settleWaiters;
    this.settleWaiters = [];
    for (const resolve of waiters) resolve();
  }

  /** Close the socket and silence every callback and pending timer, forever. */
  dispose(): void {
    this.disposed = true;
    this.settle();
    this.cancelReplayFlush();
    this.replayFrames = [];
    this.replayText = null;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      this.socket.close();
      this.socket = null;
    }
  }

  /**
   * A reconnect replays the active turn's journal, one socket task per frame.
   * Reducing and publishing each one re-rendered the transcript once per
   * frame, which on a long turn is thousands of renders before the reader
   * sees anything settle. Hold replayed frames until the burst goes quiet and
   * hand them over together. The protocol marks replayed frames but sends no
   * end marker, so a short quiet window is the boundary.
   */
  private queueReplay(frame: SequencedEvent): void {
    const event = frame.event;
    if (event.type === "text_delta" || event.type === "reasoning_delta") {
      // Held as chunks and joined once: appending to one growing string
      // makes a character-at-a-time replay quadratic.
      const held = this.replayText;
      if (held?.type === event.type && held.seq + 1 === frame.seq) {
        held.seq = frame.seq;
        held.chunks.push(event.text);
      } else {
        this.flushReplayText();
        this.replayText = {
          type: event.type,
          seq: frame.seq,
          chunks: [event.text],
        };
      }
    } else {
      this.flushReplayText();
      this.replayFrames.push(frame);
    }
    this.cancelReplayFlush();
    this.replayFlush = setTimeout(() => {
      this.replayFlush = null;
      this.flushReplay();
    }, CHAT_REPLAY_SETTLE_MS);
  }

  private cancelReplayFlush(): void {
    if (this.replayFlush !== null) clearTimeout(this.replayFlush);
    this.replayFlush = null;
  }

  private flushReplayText(): void {
    const held = this.replayText;
    if (!held) return;
    this.replayFrames.push({
      seq: held.seq,
      replayed: true,
      event: { type: held.type, text: held.chunks.join("") },
    });
    this.replayText = null;
  }

  /** Everything held so far, in order, leaving nothing behind. */
  private takeReplay(): SequencedEvent[] {
    this.cancelReplayFlush();
    this.flushReplayText();
    const frames = this.replayFrames;
    this.replayFrames = [];
    return frames;
  }

  private flushReplay(): void {
    const frames = this.takeReplay();
    if (!this.disposed && frames.length > 0) this.options.onEvents(frames);
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.reconnectTimer !== null) return;
    this.live = false;
    // The next socket resumes from the reducer's cursor, so anything held
    // back has to land first or the reconnect would skip past it.
    this.flushReplay();
    this.options.onConnectionState("reconnecting");
    // The attempt a Retry now waited for has failed; the next one is on the
    // backoff again.
    this.settle();
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
          // Metadata is never replayed. Deliver held history first so the
          // host sees frames in the order they arrived.
          this.flushReplay();
          this.options.onMetadata(metadata);
          return;
        }
        const event = frame as SequencedEvent;
        if (!isWellFormedFrame(event)) {
          console.error("dropping malformed event frame", event);
          return;
        }
        if (event.replayed === true) {
          this.queueReplay(event);
          return;
        }
        const replay = this.takeReplay();
        this.options.onEvents(replay.length > 0 ? [...replay, event] : [event]);
      });
    } catch {
      this.scheduleReconnect();
      return;
    }

    this.socket = socket;
    socket.onopen = () => {
      if (this.disposed || this.socket !== socket) return;
      this.live = true;
      this.reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
      this.options.onConnectionState("live");
      this.settle();
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
