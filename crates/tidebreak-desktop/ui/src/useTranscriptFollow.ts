import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefCallback,
  type RefObject,
} from "react";

import {
  followScrollBehavior,
  isNearBottom,
  scrollToLatest,
} from "./ChatScroll";

export type TranscriptFollow = {
  /** Attach to the scrolling viewport. */
  scrollRef: RefCallback<HTMLDivElement>;
  /** Attach to the growing content inside the viewport. */
  contentRef: RefCallback<HTMLDivElement>;
  /** Attach to the viewport's `onScroll`. */
  onScroll: () => void;
  /** Whether the reader has moved off the tail of the transcript. */
  scrolledAway: boolean;
  /** Which edges have more content beyond them, as a wrapper class. */
  fadeClass: string | null;
  /** Jump to the latest content, without arming follow. */
  scrollToBottom: (behavior: ScrollBehavior) => void;
  /** Follow the latest again, and go there. */
  armFollow: (behavior?: ScrollBehavior) => void;
  /** Stop following: the reader has been sent somewhere specific. */
  disarmFollow: () => void;
  /**
   * Stop following without claiming the reader has scrolled away.
   *
   * For a reveal opened in place — a tool line, a disclosure — where dragging
   * the reader to the tail would move the very row they clicked. The next
   * content measurement reports where they actually ended up.
   */
  pauseFollow: () => void;
  /** Whether follow is armed, readable without a re-render. */
  isFollowing: () => boolean;
  /** The viewport element, once mounted, for consumers that re-render on it. */
  scrollElement: HTMLDivElement | null;
  /** The viewport, readable without a re-render. */
  viewportRef: RefObject<HTMLDivElement | null>;
  /** Suppress reader-intent handling while an imperative scroll runs. */
  beginProgrammaticScroll: () => void;
  endProgrammaticScroll: () => void;
  /** Ease to the tail once the next content growth lands. */
  requestSmoothFollow: () => void;
};

/**
 * The transcript's scroll-follow machine: stay pinned to the newest content
 * while the reader wants to be, get out of the way the moment they don't, and
 * report where the scroll sits so the frame can fade its edges.
 *
 * Follow is armed deliberately — by opening a conversation, sending, or asking
 * to return to the latest — and disarmed by drifting away from the tail. It is
 * never re-armed by scrolling back down, because a reader who scrolled to read
 * something is not asking to be dragged along by the next token.
 *
 * The edge fades are a class on the wrapper, painted as overlay
 * pseudo-elements. Never move them to a `mask-image` on the scrolling layer:
 * that is what WKWebView failed to repaint after the mount-time catch-up
 * scroll, leaving a laid-out transcript blank (see `styles.css`).
 */
export function useTranscriptFollow({
  visible = true,
}: {
  /** False while another surface covers the transcript's slot. */
  visible?: boolean;
} = {}): TranscriptFollow {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(
    null,
  );
  const followsLatestRef = useRef(true);
  const isProgrammaticRef = useRef(false);
  const isSmoothScrollingRef = useRef(false);
  const smoothScrollRequestedRef = useRef(false);
  const scrollObserverRef = useRef<ResizeObserver | null>(null);
  const contentObserverRef = useRef<ResizeObserver | null>(null);
  const [scrolledAway, setScrolledAway] = useState(false);
  const [fadeClass, setFadeClass] = useState<string | null>(null);

  // Reflect where the scroll sits onto the edge-fade masks: fade the top once
  // there is content above, the bottom while there is content below.
  const updateEdges = useCallback(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    const fromTop = scroll.scrollTop > 0;
    const fromBottom =
      scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight > 1;
    setFadeClass(
      fromTop && fromBottom
        ? "is-faded-both"
        : fromTop
          ? "is-faded-top"
          : fromBottom
            ? "is-faded-bottom"
            : null,
    );
  }, []);

  const instantScrollToBottom = useCallback(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    isProgrammaticRef.current = true;
    setScrolledAway(false);
    scrollToLatest(scroll, "auto");
    requestAnimationFrame(() => {
      isProgrammaticRef.current = false;
    });
  }, []);

  const smoothScrollToBottom = useCallback(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    isSmoothScrollingRef.current = true;
    isProgrammaticRef.current = true;
    setScrolledAway(false);
    scrollToLatest(scroll, followScrollBehavior(false));

    const done = () => {
      isSmoothScrollingRef.current = false;
      isProgrammaticRef.current = false;
      // Content may have grown while the animation was targeting an older
      // bottom. Catch up once, without starting another animation.
      if (followsLatestRef.current) instantScrollToBottom();
    };
    let timeout: ReturnType<typeof setTimeout>;
    const onScrollEnd = () => {
      clearTimeout(timeout);
      done();
    };
    scroll.addEventListener("scrollend", onScrollEnd, { once: true });
    timeout = setTimeout(() => {
      scroll.removeEventListener("scrollend", onScrollEnd);
      done();
    }, 800);
  }, [instantScrollToBottom]);

  // Jump to the latest message. Marked programmatic so the resulting scroll
  // events don't read as the reader deliberately scrolling away.
  const scrollToBottom = useCallback(
    (behavior: ScrollBehavior) => {
      if (behavior === "smooth") smoothScrollToBottom();
      else instantScrollToBottom();
    },
    [instantScrollToBottom, smoothScrollToBottom],
  );

  const armFollow = useCallback(
    (behavior: ScrollBehavior = "auto") => {
      followsLatestRef.current = true;
      scrollToBottom(behavior);
    },
    [scrollToBottom],
  );

  const disarmFollow = useCallback(() => {
    followsLatestRef.current = false;
    setScrolledAway(true);
  }, []);

  const pauseFollow = useCallback(() => {
    followsLatestRef.current = false;
  }, []);

  const isFollowing = useCallback(() => followsLatestRef.current, []);

  const beginProgrammaticScroll = useCallback(() => {
    isProgrammaticRef.current = true;
  }, []);

  const endProgrammaticScroll = useCallback(() => {
    isProgrammaticRef.current = false;
  }, []);

  const requestSmoothFollow = useCallback(() => {
    smoothScrollRequestedRef.current = true;
  }, []);

  const onScroll = useCallback(() => {
    updateEdges();
    if (isProgrammaticRef.current) return;
    const scroll = scrollRef.current;
    if (!scroll) return;
    const away = !isNearBottom(scroll);
    setScrolledAway(away);
    // Drifting away disarms follow; re-arming is deliberate (the button, or a
    // send), never a side effect of scrolling back toward the bottom.
    if (away && followsLatestRef.current) followsLatestRef.current = false;
  }, [updateEdges]);

  // Track the scroll viewport height in a CSS variable so a pinned turn can
  // reserve roughly a screenful, and keep the edge fades honest across resizes.
  const attachScrollRef = useCallback(
    (element: HTMLDivElement | null) => {
      scrollObserverRef.current?.disconnect();
      scrollRef.current = element;
      setScrollElement(element);
      if (!element) return;
      const observer = new ResizeObserver(() => {
        element.style.setProperty(
          "--transcript-viewport",
          `${element.clientHeight}px`,
        );
        updateEdges();
      });
      observer.observe(element);
      scrollObserverRef.current = observer;
    },
    [updateEdges],
  );

  // Follow asynchronous layout growth (image loads, markdown reflow) that no
  // React state change announces, so a following reader stays pinned to the end.
  const attachContentRef = useCallback(
    (element: HTMLDivElement | null) => {
      contentObserverRef.current?.disconnect();
      if (!element) return;
      const observer = new ResizeObserver(() => {
        if (!visible) return;
        if (smoothScrollRequestedRef.current) {
          smoothScrollRequestedRef.current = false;
          scrollToBottom(followScrollBehavior(false));
        } else if (followsLatestRef.current && !isSmoothScrollingRef.current) {
          scrollToBottom("auto");
        }
        const scroll = scrollRef.current;
        if (scroll) setScrolledAway(!isNearBottom(scroll));
        updateEdges();
      });
      observer.observe(element);
      contentObserverRef.current = observer;
    },
    [visible, scrollToBottom, updateEdges],
  );

  // The transcript can remain mounted while a neighboring panel takes the
  // space. Restore a following reader to the tail when it becomes visible.
  useEffect(() => {
    if (visible && followsLatestRef.current) scrollToBottom("auto");
    updateEdges();
  }, [visible, scrollToBottom, updateEdges]);

  return {
    scrollRef: attachScrollRef,
    contentRef: attachContentRef,
    onScroll,
    scrolledAway,
    fadeClass,
    scrollToBottom,
    armFollow,
    disarmFollow,
    pauseFollow,
    isFollowing,
    scrollElement,
    viewportRef: scrollRef,
    beginProgrammaticScroll,
    endProgrammaticScroll,
    requestSmoothFollow,
  };
}
