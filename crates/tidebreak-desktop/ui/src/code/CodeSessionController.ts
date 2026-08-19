import type { CodeTurnSnapshot, SequencedCodeEventFrame } from "../api/types";
import {
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  nextReconnectDelay,
} from "../ChatSessionController";

export type CodeConnectionState = "live" | "reconnecting";

/** A brief quiet window means the initial journal burst has reached its tail. */
export const CODE_REPLAY_SETTLE_MS = 40;

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
  /**
   * Deliver an ordered journal chunk. Replayed history is coalesced to one
   * browser paint; live frames still arrive immediately.
   */
  onEvents: (events: readonly SequencedCodeEventFrame[]) => void;
  onConnectionState: (state: CodeConnectionState) => void;
  /**
   * Snapshot of durable turns, applied once before the first socket opens so
   * replay can fill assistant/tool events onto turn-keyed user items.
   */
  hydrateTurns?: () => Promise<CodeTurnSnapshot[]>;
  onHydrate?: (turns: CodeTurnSnapshot[]) => void;
  /**
   * The snapshot settled, whether it arrived or failed. The transcript hangs
   * its skeleton on this rather than on `onHydrate`, so a session whose history
   * could not be read still reaches a state the reader can send from.
   */
  onHydrateSettled?: () => void;
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
  private hydrated = false;
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
  private replayFrames: SequencedCodeEventFrame[] = [];
  private replayDelta:
    | {
        type: "assistant_delta" | "reasoning_delta";
        seq: number;
        chunks: string[];
      }
    | null = null;
  private replayFlush: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly options: CodeSessionControllerOptions) {}

  start(): void {
    void this.hydrateThenConnect();
  }

  private async hydrateThenConnect(): Promise<void> {
    if (this.options.hydrateTurns && !this.hydrated) {
      try {
        const turns = await this.options.hydrateTurns();
        if (this.disposed) return;
        this.options.onHydrate?.(turns);
        this.hydrated = true;
      } catch {
        if (this.disposed) return;
      }
    }
    this.options.onHydrateSettled?.();
    this.connect();
  }

  /** Close the socket and silence every callback and pending timer, forever. */
  dispose(): void {
    this.disposed = true;
    this.cancelReplayFlush();
    this.replayFrames = [];
    this.replayDelta = null;
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
   * WebSocket replay arrives as one task per historical frame. React's
   * external-store contract treats each task as a synchronous update, so a
   * large transcript can exceed its nested-update guard before history has
   * settled. Collect replay frames until the burst goes quiet and reduce them
   * once.
   */
  private queueReplay(frame: SequencedCodeEventFrame): void {
    // Keep text fragments as chunks until the run ends. Repeatedly appending
    // to one growing JS string makes a character-at-a-time replay quadratic.
    if (
      frame.event.type === "assistant_delta" ||
      frame.event.type === "reasoning_delta"
    ) {
      const previous = this.replayDelta;
      if (
        previous?.type === frame.event.type &&
        previous.seq + 1 === frame.seq
      ) {
        previous.seq = frame.seq;
        previous.chunks.push(frame.event.text);
      } else {
        this.flushReplayDelta();
        this.replayDelta = {
          type: frame.event.type,
          seq: frame.seq,
          chunks: [frame.event.text],
        };
      }
    } else {
      this.flushReplayDelta();
      this.replayFrames.push(frame);
    }
    // The protocol marks replay frames but has no separate end marker. Treat a
    // short quiet window as the boundary, resetting it for every frame. With
    // rendering withheld, even a large local journal drains inside this
    // window and appears as one settled transcript instead of typing itself
    // into view over several paints.
    this.cancelReplayFlush();
    this.replayFlush = setTimeout(() => {
      this.replayFlush = null;
      this.flushReplay();
    }, CODE_REPLAY_SETTLE_MS);
  }

  private cancelReplayFlush(): void {
    if (this.replayFlush !== null) clearTimeout(this.replayFlush);
    this.replayFlush = null;
  }

  private flushReplayDelta(): void {
    const delta = this.replayDelta;
    if (!delta) return;
    this.replayFrames.push({
      seq: delta.seq,
      replayed: true,
      event: { type: delta.type, text: delta.chunks.join("") },
    });
    this.replayDelta = null;
  }

  private takeReplay(): SequencedCodeEventFrame[] {
    this.cancelReplayFlush();
    this.flushReplayDelta();
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
        if (frame.replayed === true) {
          this.queueReplay(frame);
          return;
        }
        const replay = this.takeReplay();
        this.options.onEvents([...replay, frame]);
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
