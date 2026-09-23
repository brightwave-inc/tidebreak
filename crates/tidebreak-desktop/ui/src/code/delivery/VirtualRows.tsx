import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

export const PR_ROW_HEIGHT = 62;

export const PR_GROUP_HEIGHT = 38;

export const RUN_ROW_HEIGHT = 62;

export const PR_GRID =
  "grid-cols-[minmax(0,1fr)_auto] [&>*:nth-child(2)]:hidden [&>*:nth-child(3)]:hidden [&>*:nth-child(4)]:hidden [&>*:nth-child(5)]:hidden @[57rem]:grid-cols-[minmax(260px,1fr)_130px_105px_minmax(8rem,auto)_85px] @[57rem]:[&>*:nth-child(2)]:flex @[57rem]:[&>*:nth-child(3)]:flex @[57rem]:[&>*:nth-child(5)]:flex";

export const RUN_GRID =
  "grid-cols-[minmax(0,1fr)_auto] [&>*:nth-child(2)]:hidden [&>*:nth-child(3)]:hidden @[48rem]:grid-cols-[minmax(220px,1fr)_140px_120px_90px] @[48rem]:[&>*:nth-child(2)]:flex @[48rem]:[&>*:nth-child(3)]:flex";

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
    if (listRef.current) observer.observe(listRef.current);
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
