import { useCallback, useEffect, useRef } from "react";

interface UseWheelPageNavigationOptions {
  containerRef: HTMLElement | null;
  currentPage: number;
  numPages: number;
  setCurrentPage: (page: number | ((prev: number) => number)) => void;
}

const SCROLL_EPSILON = 1;
const DELTA_THRESHOLD = 100;
const PAGE_CHANGE_COOLDOWN_MS = 100;

/**
 * Scrolling past the bottom of a page turns to the next one, and past the top
 * turns back — but only after a deliberate extra push at the boundary, so a
 * fast flick through one page does not overshoot into the next.
 */
export function useWheelPageNavigation({
  containerRef,
  currentPage,
  numPages,
  setCurrentPage,
}: UseWheelPageNavigationOptions): void {
  const deltaAccumulatorRef = useRef<number>(0);
  const lastPageChangeRef = useRef<number>(0);
  const scrollDirectionRef = useRef<"up" | "down" | null>(null);
  const wasAtBoundaryRef = useRef<boolean>(false);

  const handleWheel = useCallback(
    (e: WheelEvent) => {
      if (!containerRef || numPages === 0) return;

      const now = Date.now();
      if (now - lastPageChangeRef.current < PAGE_CHANGE_COOLDOWN_MS) return;

      const { scrollTop, scrollHeight, clientHeight } = containerRef;
      const isAtTop = scrollTop <= SCROLL_EPSILON;
      const isAtBottom =
        scrollHeight - scrollTop - clientHeight <= SCROLL_EPSILON;

      // Normalize deltaMode: 0=pixel, 1=line, 2=page
      let normalizedDelta = e.deltaY;
      if (e.deltaMode === 1) {
        normalizedDelta *= 16;
      } else if (e.deltaMode === 2) {
        normalizedDelta *= clientHeight;
      }

      const currentlyAtBoundary = isAtTop || isAtBottom;
      if (!currentlyAtBoundary || !wasAtBoundaryRef.current) {
        deltaAccumulatorRef.current = 0;
      }
      wasAtBoundaryRef.current = currentlyAtBoundary;

      if (isAtBottom && normalizedDelta > 0 && currentPage < numPages) {
        e.preventDefault();
        deltaAccumulatorRef.current += normalizedDelta;

        if (deltaAccumulatorRef.current >= DELTA_THRESHOLD) {
          lastPageChangeRef.current = now;
          scrollDirectionRef.current = "down";
          deltaAccumulatorRef.current = 0;
          setCurrentPage((prev) => Math.min(numPages, prev + 1));
        }
      } else if (isAtTop && normalizedDelta < 0 && currentPage > 1) {
        e.preventDefault();
        deltaAccumulatorRef.current += normalizedDelta;

        if (deltaAccumulatorRef.current <= -DELTA_THRESHOLD) {
          lastPageChangeRef.current = now;
          scrollDirectionRef.current = "up";
          deltaAccumulatorRef.current = 0;
          setCurrentPage((prev) => Math.max(1, prev - 1));
        }
      }
    },
    [containerRef, numPages, currentPage, setCurrentPage],
  );

  useEffect(() => {
    if (!containerRef) return;

    containerRef.addEventListener("wheel", handleWheel, { passive: false });
    return () => {
      containerRef.removeEventListener("wheel", handleWheel);
    };
  }, [containerRef, handleWheel]);

  // Land at the edge the reader was travelling towards: paging down starts the
  // next page at its top, paging up starts the previous page at its bottom.
  useEffect(() => {
    if (!containerRef || !scrollDirectionRef.current) return;

    const direction = scrollDirectionRef.current;
    scrollDirectionRef.current = null;

    const timeoutId = setTimeout(() => {
      const { scrollHeight, clientHeight } = containerRef;

      if (direction === "down") {
        containerRef.scrollTo({ top: 0, behavior: "auto" });
      } else if (direction === "up") {
        containerRef.scrollTo({
          top: scrollHeight - clientHeight,
          behavior: "auto",
        });
      }
    }, 50);

    return () => clearTimeout(timeoutId);
  }, [currentPage, containerRef]);
}
