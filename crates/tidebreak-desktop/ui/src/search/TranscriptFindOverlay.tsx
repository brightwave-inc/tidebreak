import { type ReactNode, useLayoutEffect, useRef } from "react";

/** The room a transcript keeps above its first row for the find bar. */
const FIND_ROOM = "--transcript-find-room";

/** The scroller's top padding, in pixels, as laid out now. */
function paddingTop(scroller: HTMLElement): number {
  return Number.parseFloat(getComputedStyle(scroller).paddingTop) || 0;
}

/**
 * Where a transcript pane hosts its find bar: floating over the transcript's
 * top edge, the way a browser's find bar does.
 *
 * While the bar is open, the transcript's first row starts below it, so a
 * short conversation is never hidden under the bar or its notes. The room
 * goes above everything, so a reader part way down would see the transcript
 * shift; their scroll moves by the same amount instead, and what they were
 * reading stays where it was. Only a transcript scrolled to its top moves,
 * which is what brings its first row out from under the bar.
 */
export function TranscriptFindOverlay({
  scrollElement,
  children,
}: {
  /** The transcript's scrolling viewport. */
  scrollElement: HTMLElement | null;
  children: ReactNode;
}) {
  const overlay = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const bar = overlay.current;
    const scroller = scrollElement;
    if (!bar || !scroller) return;
    const setRoom = (room: number | null) => {
      const before = paddingTop(scroller);
      if (room === null) scroller.style.removeProperty(FIND_ROOM);
      else scroller.style.setProperty(FIND_ROOM, `${room}px`);
      const moved = paddingTop(scroller) - before;
      if (moved !== 0 && scroller.scrollTop > 0) scroller.scrollTop += moved;
    };
    const place = () => {
      const bottom =
        bar.getBoundingClientRect().bottom -
        scroller.getBoundingClientRect().top;
      setRoom(Math.max(0, Math.ceil(bottom)));
    };
    place();
    // A note under the field, or an error, makes the bar taller.
    const observer = new ResizeObserver(place);
    observer.observe(bar);
    return () => {
      observer.disconnect();
      setRoom(null);
    };
  }, [scrollElement]);

  return (
    <div
      ref={overlay}
      className="pointer-events-none absolute inset-x-0 top-2 z-[3] flex justify-end px-3"
    >
      {children}
    </div>
  );
}
