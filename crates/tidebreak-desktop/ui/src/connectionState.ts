import { useEffect, useState } from "react";
import { create } from "zustand";

/** What an event socket reports: open, or dropped and retrying. */
export type SocketConnectionState = "live" | "reconnecting";

/**
 * How long a dropped connection reads as a blip before it becomes a notice.
 * Sleep and wake, a network change, or a remote machine restarting usually
 * come back well inside it; a connection still down after it is one the
 * reader should hear about, with something to do.
 */
export const CONNECTION_ESCALATE_AFTER_MS = 30_000;

/** What the connection indicator shows for a socket. */
export type ConnectionIndicator = "live" | "reconnecting" | "escalated";

/**
 * Follow a socket's state, escalating a drop that outlasts
 * {@link CONNECTION_ESCALATE_AFTER_MS}.
 *
 * The clock starts when the socket drops and stops when it is live again,
 * so a connection that flaps restarts the wait instead of escalating on the
 * sum of its drops. Retry now does not restart it: the socket is still down.
 */
export function useConnectionIndicator(
  state: SocketConnectionState,
  escalateAfterMs: number = CONNECTION_ESCALATE_AFTER_MS,
): ConnectionIndicator {
  const [escalated, setEscalated] = useState(false);
  useEffect(() => {
    setEscalated(false);
    if (state !== "reconnecting") return;
    const timer = window.setTimeout(() => setEscalated(true), escalateAfterMs);
    return () => window.clearTimeout(timer);
  }, [state, escalateAfterMs]);
  if (state === "live") return "live";
  return escalated ? "escalated" : "reconnecting";
}

/** Raised by the shell when the local server stopped after it started. */
export const SERVER_STOPPED_EVENT = "desktop-server-stopped";

type ServerHealth = {
  /**
   * The local server's accept loop stopped after it bound. Its sockets never
   * come back on their own, so every connection indicator offers a restart.
   */
  stopped: boolean;
  markStopped: () => void;
};

/** Whether the local server stopped, as the shell reported it. */
export const useServerHealth = create<ServerHealth>()((set) => ({
  stopped: false,
  markStopped: () => set({ stopped: true }),
}));

/** The machine's host, for a notice that names it. */
export function machineHost(baseUrl: string): string {
  try {
    return new URL(baseUrl).host || baseUrl;
  } catch {
    return baseUrl;
  }
}
