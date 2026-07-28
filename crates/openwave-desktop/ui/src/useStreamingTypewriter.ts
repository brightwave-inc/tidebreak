import { useCallback, useEffect, useRef, useState } from "react";

const TICK_INTERVAL_MS = 20;
const SMOOTHING_FACTOR = 18;

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
    if (!liveRef.current || document.visibilityState !== "visible") {
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

    const nextLength = Math.min(
      target.length,
      current.length + Math.max(1, Math.ceil(remaining / SMOOTHING_FACTOR)),
    );
    showImmediately(target.slice(0, nextLength));
    timerRef.current = window.setTimeout(tick, TICK_INTERVAL_MS);
  }, [showImmediately]);

  useEffect(() => {
    const previousTarget = targetRef.current;
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
      stop();
      showImmediately(text);
      return;
    }
    if (text === previousTarget) return;

    if (timerRef.current === null) tick();
  }, [live, showImmediately, stop, text, tick]);

  useEffect(() => stop, [stop]);

  return displayed;
}
