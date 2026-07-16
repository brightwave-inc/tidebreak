import { useEffect, useRef, type MutableRefObject } from "react";
import type { ApiClient, SequencedEvent } from "./api";

export const INITIAL_RECONNECT_DELAY_MS = 250;
export const MAX_RECONNECT_DELAY_MS = 5_000;

/** The next bounded backoff value after scheduling one reconnect attempt. */
export function nextReconnectDelay(delayMs: number): number {
  return Math.min(delayMs * 2, MAX_RECONNECT_DELAY_MS);
}

/** Guards callbacks from a chat that was explicitly replaced or disposed. */
export function isCurrentConnection(
  disposed: boolean,
  expectedGeneration: number,
  currentGeneration: number,
): boolean {
  return !disposed && expectedGeneration === currentGeneration;
}

type ConnectionState = "live" | "reconnecting";

type ChatEventStreamOptions = {
  client: ApiClient | null;
  chatId: string | null;
  ready: boolean;
  afterRef: MutableRefObject<number>;
  socketRef: MutableRefObject<WebSocket | null>;
  generationRef: MutableRefObject<number>;
  onEvent: (event: SequencedEvent) => void;
  onConnectionState: (state: ConnectionState) => void;
};

/**
 * Keeps a selected chat's event stream alive without allowing an old chat or
 * an intentionally closed socket to reconnect over the current selection.
 */
export function useChatEventStream({
  client,
  chatId,
  ready,
  afterRef,
  socketRef,
  generationRef,
  onEvent,
  onConnectionState,
}: ChatEventStreamOptions): void {
  const onEventRef = useRef(onEvent);
  const onConnectionStateRef = useRef(onConnectionState);
  onEventRef.current = onEvent;
  onConnectionStateRef.current = onConnectionState;

  useEffect(() => {
    if (!client || !chatId || !ready) return;

    socketRef.current?.close();
    const generation = ++generationRef.current;
    let disposed = false;
    let reconnectTimer: number | null = null;
    let reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;

    const isCurrent = () =>
      isCurrentConnection(disposed, generation, generationRef.current);

    const scheduleReconnect = () => {
      if (!isCurrent() || reconnectTimer !== null) return;
      onConnectionStateRef.current("reconnecting");
      const delay = reconnectDelayMs;
      reconnectDelayMs = nextReconnectDelay(reconnectDelayMs);
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    const connect = () => {
      if (!isCurrent()) return;
      let socket: WebSocket;
      try {
        socket = client.openEvents(chatId, afterRef.current, (event) => {
          if (isCurrent() && socketRef.current === socket) {
            onEventRef.current(event);
          }
        });
      } catch {
        scheduleReconnect();
        return;
      }

      socketRef.current = socket;
      socket.onopen = () => {
        if (!isCurrent() || socketRef.current !== socket) return;
        reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
        onConnectionStateRef.current("live");
      };
      socket.onerror = () => {
        if (!isCurrent() || socketRef.current !== socket) return;
        // Browser error events are not required to be followed by close. Close
        // this socket so the one reconnect path owns recovery in either case.
        socket.close();
        scheduleReconnect();
      };
      socket.onclose = () => {
        if (socketRef.current !== socket) return;
        socketRef.current = null;
        scheduleReconnect();
      };
    };

    connect();
    return () => {
      disposed = true;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      if (socketRef.current) {
        socketRef.current.close();
        socketRef.current = null;
      }
      if (generationRef.current === generation) generationRef.current += 1;
    };
  }, [afterRef, chatId, client, generationRef, ready, socketRef]);
}
