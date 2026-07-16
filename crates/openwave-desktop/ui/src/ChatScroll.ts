export const AUTO_SCROLL_THRESHOLD_PX = 72;

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

export function scrollToLatest(element: Pick<HTMLElement, "scrollHeight" | "scrollTo">): void {
  element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
}
