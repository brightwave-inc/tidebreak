import { useCallback, useEffect, useRef, useState } from "react";

const TICK_INTERVAL_MS = 20;
const SMOOTHING_FACTOR = 18;
const FINISH_SMOOTHING_FACTOR = 5;

/**
 * How far the animation may trail its target before it stops animating and
 * snaps. Live streaming never opens a gap this size: deltas arrive in small
 * chunks and each tick closes most of what remains. A gap this large means
 * catch-up — a reconnect replaying the active turn's journal (#1716) — and
 * animating it re-types prose the reader already watched stream.
 */
const CATCH_UP_SNAP_CHARS = 600;

/**
 * Types subsequent live changes without reanimating content that was already
 * present when the component mounted. Transcript history therefore appears
 * immediately, while an active step gains motion only as it receives new
 * presentation text.
 */
export function useStreamingTypewriter(text: string, live: boolean): string {
  const [displayed, setDisplayed] = useState(text);
  const displayedRef = useRef(text);
  const targetRef = useRef(text);
  const liveRef = useRef(live);
  const mountedRef = useRef(false);
  // Spelled out rather than inferred from `window.setTimeout`: a dependency
  // that references Node's types puts a competing `setTimeout` declaration in
  // scope, and the inferred return type then picks the wrong one.
  const timerRef = useRef<number | null>(null);

  const showImmediately = useCallback((value: string) => {
    displayedRef.current = value;
    setDisplayed(value);
  }, []);

  const stop = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const tick = useCallback(() => {
    const target = targetRef.current;
    if (document.visibilityState !== "visible") {
      timerRef.current = null;
      showImmediately(target);
      return;
    }

    let current = displayedRef.current;
    // A summary can switch from one tool's wording to another rather than
    // merely append. Restarting avoids showing a hybrid of the two titles.
    if (!target.startsWith(current)) {
      current = "";
      showImmediately(current);
    }
    const remaining = target.length - current.length;
    if (remaining <= 0) {
      timerRef.current = null;
      return;
    }
    if (remaining > CATCH_UP_SNAP_CHARS) {
      timerRef.current = null;
      showImmediately(target);
      return;
    }

    const nextLength = Math.min(
      target.length,
      current.length +
        Math.max(
          1,
          Math.ceil(
            remaining /
              (liveRef.current ? SMOOTHING_FACTOR : FINISH_SMOOTHING_FACTOR),
          ),
        ),
    );
    showImmediately(target.slice(0, nextLength));
    timerRef.current = window.setTimeout(tick, TICK_INTERVAL_MS);
  }, [showImmediately]);

  useEffect(() => {
    const previousTarget = targetRef.current;
    const wasLive = liveRef.current;
    targetRef.current = text;
    liveRef.current = live;

    // The first value may be a rehydrated transcript. Never make someone
    // wait for history to render, even if React mounts this row mid-turn.
    if (!mountedRef.current) {
      mountedRef.current = true;
      showImmediately(text);
      return;
    }
    if (!live) {
      // A stream that just ended may still have a small presentation buffer.
      // Drain it quickly instead of snapping the final characters into place.
      // Ordinary historical updates still render immediately.
      const canFinishSmoothly =
        wasLive && text.startsWith(displayedRef.current);
      if (!canFinishSmoothly) {
        stop();
        showImmediately(text);
      } else if (timerRef.current === null) {
        tick();
      }
      return;
    }
    if (text === previousTarget) return;

    if (timerRef.current === null) tick();
  }, [live, showImmediately, stop, text, tick]);

  useEffect(() => stop, [stop]);

  return displayed;
}
