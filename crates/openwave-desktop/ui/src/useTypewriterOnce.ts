import { useEffect, useRef, useState } from "react";

const TYPE_INTERVAL_MS = 30;

/**
 * Types a label out once, the first time it appears while the phase is active,
 * then sets later changes instantly. A label that mounts already settled — a
 * rehydrated transcript — appears at once and never animates. This is what
 * keeps the tool rail's phase line from re-typing itself every time one of its
 * calls settles and nudges the wording.
 */
export function useTypewriterOnce(text: string, active: boolean): string {
  const [displayed, setDisplayed] = useState(() => (active ? "" : text));
  const timerRef = useRef<number | null>(null);
  const mountedRef = useRef(false);
  const hasTypedRef = useRef(false);
  const prevRef = useRef(text);

  useEffect(() => {
    const stop = () => {
      if (timerRef.current !== null) {
        window.clearInterval(timerRef.current);
        timerRef.current = null;
      }
    };
    const showImmediately = (value: string) => {
      stop();
      setDisplayed(value);
    };
    const typeOut = (target: string) => {
      stop();
      // A backgrounded tab never sees the motion, so skip straight to the
      // final label rather than animating into a pane no one is watching.
      if (document.visibilityState !== "visible" || target.length === 0) {
        setDisplayed(target);
        return;
      }
      let i = 0;
      setDisplayed("");
      timerRef.current = window.setInterval(() => {
        i += 1;
        setDisplayed(target.slice(0, i));
        if (i >= target.length) stop();
      }, TYPE_INTERVAL_MS);
    };

    if (!mountedRef.current) {
      mountedRef.current = true;
      prevRef.current = text;
      if (active) {
        hasTypedRef.current = true;
        typeOut(text);
      } else {
        showImmediately(text);
      }
      return;
    }
    if (text === prevRef.current) return;
    prevRef.current = text;
    if (active && !hasTypedRef.current) {
      hasTypedRef.current = true;
      typeOut(text);
    } else {
      showImmediately(text);
    }
  }, [text, active]);

  useEffect(
    () => () => {
      if (timerRef.current !== null) window.clearInterval(timerRef.current);
    },
    [],
  );

  return displayed;
}
