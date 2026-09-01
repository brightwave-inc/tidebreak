import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

export const PR_ROW_HEIGHT = 62;

export const PR_GROUP_HEIGHT = 38;

export const RUN_ROW_HEIGHT = 62;

export const PR_GRID =
  "grid-cols-[minmax(280px,1fr)_150px_110px_105px_minmax(8.75rem,auto)_95px]";

export const RUN_GRID = "grid-cols-[minmax(260px,1fr)_150px_140px_110px]";

/**
 * A windowed row list.
 *
 * Delivery reads every tracked repository, and a cross-repository "All" view
 * runs into the thousands of rows once a handful of repos are tracked.
 * Mounting all of them made selecting a row feel like the app had stalled, so
 * only the visible window is mounted and the rest is spacer height.
 *
 * Rows are spacer-positioned rather than absolutely positioned, so the sticky
 * column header and the "Load more" footer stay in normal flow. Whatever sits
 * above the list inside the same scroller — a partial-failure banner, a load
 * error — shifts the rows down, so the scroll offset that banner occupies is
 * measured and handed to the virtualizer as `scrollMargin`. Without it the
 * window is wrong by exactly the banner's height.
 */
export function VirtualRows<T extends { id: string }>({
  items,
  scrollRef,
  estimateSize,
  scrollToId,
  children,
}: {
  items: readonly T[];
  scrollRef: React.RefObject<HTMLDivElement | null>;
  estimateSize: number | ((item: T) => number);
  scrollToId?: string | null;
  children: (item: T) => React.ReactNode;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const [scrollMargin, setScrollMargin] = useState(0);

  useLayoutEffect(() => {
    const measure = () => {
      const scroller = scrollRef.current;
      const list = listRef.current;
      if (!scroller || !list) return;
      const offset =
        list.getBoundingClientRect().top -
        scroller.getBoundingClientRect().top +
        scroller.scrollTop;
      setScrollMargin((current) =>
        Math.abs(current - offset) > 0.5 ? offset : current,
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    if (scrollRef.current) observer.observe(scrollRef.current);
    return () => observer.disconnect();
  }, [scrollRef, items.length]);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => {
      const item = items[index];
      if (typeof estimateSize === "number") return estimateSize;
      return item ? estimateSize(item) : 0;
    },
    // Group headers add several short virtual rows. Keep enough surrounding
    // rows mounted that a compact list remains fully searchable and a fast
    // wheel gesture never reveals an empty gap.
    overscan: 16,
    scrollMargin,
    getItemKey: (index) => items[index]?.id ?? index,
  });

  useEffect(() => {
    if (!scrollToId) return;
    const index = items.findIndex((item) => item.id === scrollToId);
    if (index >= 0) virtualizer.scrollToIndex(index, { align: "auto" });
  }, [items, scrollToId, virtualizer]);

  const rows = virtualizer.getVirtualItems();
  const paddingTop = (rows[0]?.start ?? scrollMargin) - scrollMargin;
  const paddingBottom =
    virtualizer.getTotalSize() -
    ((rows[rows.length - 1]?.end ?? scrollMargin) - scrollMargin);

  return (
    <div ref={listRef}>
      {paddingTop > 0 && <div style={{ height: paddingTop }} aria-hidden />}
      {rows.map((row) => {
        const item = items[row.index];
        if (!item) return null;
        return (
          <div
            key={row.key}
            data-index={row.index}
            ref={virtualizer.measureElement}
          >
            {children(item)}
          </div>
        );
      })}
      {paddingBottom > 0 && (
        <div style={{ height: paddingBottom }} aria-hidden />
      )}
    </div>
  );
}
