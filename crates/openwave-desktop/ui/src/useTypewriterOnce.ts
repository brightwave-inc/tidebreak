import { useEffect, useRef, useState } from "react";

/** How long a whole label takes to type, however long the label is. */
const TYPE_DURATION_MS = 360;

/** One tick per frame; longer labels advance further per tick, not slower. */
const TYPE_INTERVAL_MS = 16;

/**
 * Types a label out once, the first time it appears while the phase is active,
 * then sets later changes instantly. A label that mounts already settled — a
 * rehydrated transcript — appears at once and never animates. This is what
 * keeps the tool rail's phase line from re-typing itself every time one of its
 * calls settles and nudges the wording.
 */
export function useTypewriterOnce(text: string, active: boolean): string {
  const [displayed, setDisplayed] = useState(() =>
    active ? text.slice(0, 1) : text,
  );
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
      // Never a blank frame. The first character lands synchronously, because
      // an empty row that is also pulsing does not read as text arriving — it
      // reads as something that failed to load.
      let i = 1;
      setDisplayed(target.slice(0, 1));
      if (target.length === 1) return;
      // Paced by the whole label rather than per character, so a phase line
      // that grows — "Checking connected folders and 1 other task" — takes the
      // same moment to appear as a short one instead of crawling. A longer
      // label advances further each tick; slowing the tick instead would just
      // trade a crawl for a stutter.
      const step = Math.max(
        1,
        Math.ceil(target.length / (TYPE_DURATION_MS / TYPE_INTERVAL_MS)),
      );
      timerRef.current = window.setInterval(() => {
        i += step;
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
