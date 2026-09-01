import { useEffect, useRef } from "react";

/**
 * A refresh cadence that respects whether anyone can see the result.
 *
 * Every poller in the shell has the same shape: read now, read again every N
 * seconds, and read at once when the event stream says the state moved on.
 * The stream is the primary signal; the timer is the safety net for a nudge
 * that fired before the subscriber existed or a socket that dropped a frame.
 * Nothing needs a fast timer to feel live, so the net runs slowly, and it
 * stops (or slows) while the document is hidden — a minimized window has no
 * reader to keep current, and the immediate read on return is what it sees.
 *
 * A signal-driven read always runs, hidden or not. Signals carry the changes a
 * hidden window still acts on: a native notification or a dock bounce for a
 * question that just parked.
 */
export type VisibilityGatedPollOptions = {
  /**
   * Cadence while the document is hidden. `null` (the default) pauses the
   * timer entirely; a number keeps a slower one running for readers whose
   * poll is what raises the out-of-app notice.
   */
  hiddenIntervalMs?: number | null;
};

export function documentHidden(): boolean {
  return typeof document !== "undefined" && document.hidden;
}

/**
 * Start the timer half of a poll outside React. Returns the stop function.
 *
 * The visible cadence arms while the document is visible; on hide it either
 * pauses or swaps to the hidden cadence; on show it runs `poll` at once and
 * re-arms. `poll` is never run from here at start — callers decide whether
 * they already have a first read in flight.
 */
export function startVisibilityGatedPoll(
  poll: () => void,
  intervalMs: number,
  options: VisibilityGatedPollOptions = {},
): () => void {
  const hiddenIntervalMs = options.hiddenIntervalMs ?? null;
  let timer: number | null = null;

  const disarm = () => {
    if (timer !== null) window.clearInterval(timer);
    timer = null;
  };
  const arm = () => {
    disarm();
    const cadence = documentHidden() ? hiddenIntervalMs : intervalMs;
    if (cadence === null) return;
    timer = window.setInterval(poll, cadence);
  };
  const onVisibilityChange = () => {
    if (!documentHidden()) poll();
    arm();
  };

  arm();
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", onVisibilityChange);
  }
  return () => {
    disarm();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
  };
}

/**
 * Poll `poll` on a visibility-gated cadence and on every change of `revision`.
 *
 * `revision` is a refresh-signal counter (or a sum of several — counters only
 * grow, so any bump changes the sum). Its value on mount is not a signal;
 * only later changes trigger a read. `enabled` false disarms everything.
 * `poll` may change identity freely; the latest one is called.
 */
export function useVisibilityGatedPoll(
  poll: () => void,
  intervalMs: number,
  options: VisibilityGatedPollOptions & {
    enabled?: boolean;
    revision?: number;
  } = {},
): void {
  const { enabled = true, revision = 0, hiddenIntervalMs = null } = options;
  const pollRef = useRef(poll);
  pollRef.current = poll;

  useEffect(() => {
    if (!enabled) return;
    return startVisibilityGatedPoll(() => pollRef.current(), intervalMs, {
      hiddenIntervalMs,
    });
  }, [enabled, intervalMs, hiddenIntervalMs]);

  const lastRevisionRef = useRef(revision);
  useEffect(() => {
    if (lastRevisionRef.current === revision) return;
    lastRevisionRef.current = revision;
    if (enabled) pollRef.current();
  }, [enabled, revision]);
}
