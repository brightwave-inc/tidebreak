export const AUTO_SCROLL_THRESHOLD_PX = 30;

type ScrollMetrics = {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
};

/** Whether a new streamed item should keep the reader pinned to the latest. */
export function isNearBottom(
  { scrollTop, clientHeight, scrollHeight }: ScrollMetrics,
  thresholdPx = AUTO_SCROLL_THRESHOLD_PX,
): boolean {
  return scrollHeight - (scrollTop + clientHeight) <= thresholdPx;
}

/**
 * Whether the environment asks for reduced motion. Guarded so the node test
 * environment (no matchMedia) and older webviews behave as "no preference".
 */
export function prefersReducedMotion(
  query: (q: string) => { matches: boolean } = (q) =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(q)
      : { matches: false },
): boolean {
  return query("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Pick the scroll behavior for following the transcript. Streaming follows
 * happen every few frames — easing them stacks animations that fight the
 * reader — so they jump instantly; discrete jumps (the "New activity" pill)
 * ease, unless the OS asks for reduced motion.
 */
export function followScrollBehavior(
  streaming: boolean,
  reducedMotion: boolean = prefersReducedMotion(),
): ScrollBehavior {
  return streaming || reducedMotion ? "auto" : "smooth";
}

export function scrollToLatest(
  element: Pick<HTMLElement, "scrollHeight" | "scrollTo">,
  behavior: ScrollBehavior = "smooth",
): void {
  element.scrollTo({ top: element.scrollHeight, behavior });
}
