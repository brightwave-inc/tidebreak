import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { prefersReducedMotion } from "./ChatScroll";

/**
 * How much of the remaining text each reveal closes, per 20 ms of elapsed
 * time: an eighteenth while the text is live, a fifth once it is finishing.
 * Scaled by the real time between reveals, so the pace holds at any frame
 * rate and across a skipped frame.
 */
const REVEAL_BASE_MS = 20;
const LIVE_SHARE = 1 / 18;
const FINISH_SHARE = 1 / 5;

/** A frame this long after the last one means the last reveal ran long. */
export const FRAME_OVER_BUDGET_MS = 24;

/** Elapsed time counted for one reveal, so a stalled tab does not dump text. */
const MAX_REVEAL_ELAPSED_MS = 100;

/**
 * How far the animation may trail its target before it stops animating and
 * snaps. Live streaming never opens a gap this size: deltas arrive in small
 * chunks and each frame closes part of what remains. A gap this large means
 * catch-up — a reconnect replaying the active turn's journal (#1716) — and
 * animating it re-types prose the reader already watched stream.
 */
const CATCH_UP_SNAP_CHARS = 600;

/**
 * Types subsequent live changes without reanimating content that was already
 * present when the component mounted. Transcript history therefore appears
 * immediately, while an active step gains motion only as it receives new
 * presentation text.
 *
 * Reveals run on animation frames. Each one re-renders the message, so a frame
 * that arrives late — because the last render ran over its budget — is given
 * back to the browser, and the next frame reveals the time both covered.
 */
export function useStreamingTypewriter(text: string, live: boolean): string {
  const [displayed, setDisplayed] = useState(text);
  const displayedRef = useRef(text);
  const targetRef = useRef(text);
  const liveRef = useRef(live);
  const mountedRef = useRef(false);

  const showImmediately = useCallback((value: string) => {
    displayedRef.current = value;
    setDisplayed(value);
  }, []);

  const animation = useMemo(() => {
    let frame: number | null = null;
    /** When the last frame ran, whether or not it revealed anything. */
    let lastFrameAt: number | null = null;
    /** When the last reveal ran. */
    let lastRevealAt: number | null = null;
    let skipped = false;

    function stop() {
      if (frame !== null) cancelAnimationFrame(frame);
      frame = null;
      lastFrameAt = null;
      lastRevealAt = null;
      skipped = false;
    }

    function schedule() {
      if (frame === null) frame = requestAnimationFrame(step);
    }

    function step(now: number) {
      frame = null;
      const target = targetRef.current;
      if (document.visibilityState !== "visible") {
        stop();
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
        stop();
        return;
      }
      if (remaining > CATCH_UP_SNAP_CHARS) {
        stop();
        showImmediately(target);
        return;
      }

      const previousFrameAt = lastFrameAt;
      lastFrameAt = now;
      if (
        previousFrameAt !== null &&
        now - previousFrameAt > FRAME_OVER_BUDGET_MS &&
        !skipped
      ) {
        // Never twice in a row: a machine that is slow on every frame still
        // sees the text move, on every other frame.
        skipped = true;
        schedule();
        return;
      }
      skipped = false;

      const elapsed =
        lastRevealAt === null
          ? REVEAL_BASE_MS
          : Math.min(now - lastRevealAt, MAX_REVEAL_ELAPSED_MS);
      lastRevealAt = now;
      const rate = liveRef.current ? LIVE_SHARE : FINISH_SHARE;
      const share = 1 - (1 - rate) ** (elapsed / REVEAL_BASE_MS);
      const nextLength = Math.min(
        target.length,
        current.length + Math.max(1, Math.ceil(remaining * share)),
      );
      showImmediately(target.slice(0, nextLength));
      if (nextLength < target.length) schedule();
      else stop();
    }

    return { schedule, stop };
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
      // A settled row must not wait on the next frame. If that frame never
      // comes, the prose stays blank until some later render — a send was
      // enough to unstick it. Historical updates and a stream that just ended
      // both render at once.
      animation.stop();
      showImmediately(text);
      return;
    }
    if (text === previousTarget) return;
    // A hidden document gets no animation frames at all, and a reader who
    // turns off motion gets the text as it arrives.
    if (document.visibilityState !== "visible" || prefersReducedMotion()) {
      animation.stop();
      showImmediately(text);
      return;
    }

    animation.schedule();
  }, [animation, live, showImmediately, text]);

  useEffect(() => animation.stop, [animation]);

  return displayed;
}
