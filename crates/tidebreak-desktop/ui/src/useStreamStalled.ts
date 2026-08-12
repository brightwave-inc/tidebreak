import { useEffect, useState } from "react";

/** How long the live stream must go quiet before the turn counts as stalled. */
export const STREAM_STALL_MS = 2000;

/**
 * True while a live turn's event stream has gone quiet for [`STREAM_STALL_MS`].
 *
 * `activitySeq` is the session's applied-event cursor: every stream event a
 * turn produces (text and reasoning deltas, tool activity, boundaries)
 * advances it, so any advance restarts the quiet-period timer and clears the
 * stall. The timer lives here rather than in the session reducer so the
 * reducer stays a pure, clock-free state machine.
 */
export function useStreamStalled(busy: boolean, activitySeq: number): boolean {
  const [stalled, setStalled] = useState(false);
  useEffect(() => {
    setStalled(false);
    if (!busy) return;
    const timer = setTimeout(() => setStalled(true), STREAM_STALL_MS);
    return () => clearTimeout(timer);
  }, [busy, activitySeq]);
  return busy && stalled;
}
