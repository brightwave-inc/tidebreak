import { describe, expect, it, vi } from "vitest";
import {
  AUTO_SCROLL_THRESHOLD_PX,
  followScrollBehavior,
  isNearBottom,
  prefersReducedMotion,
  scrollToLatest,
} from "./ChatScroll";

describe("chat scroll behavior", () => {
  it("only follows new activity within a hair of the bottom", () => {
    expect(
      isNearBottom({ scrollTop: 870, clientHeight: 100, scrollHeight: 1_000 }),
    ).toBe(true);
    expect(
      isNearBottom({ scrollTop: 869, clientHeight: 100, scrollHeight: 1_000 }),
    ).toBe(false);
    expect(AUTO_SCROLL_THRESHOLD_PX).toBe(30);
  });

  it("scrolls to the latest item when explicitly requested", () => {
    const scrollTo = vi.fn();
    scrollToLatest({ scrollHeight: 2_000, scrollTo });
    expect(scrollTo).toHaveBeenCalledWith({ top: 2_000, behavior: "smooth" });
  });
});

describe("followScrollBehavior", () => {
  it("jumps instantly while streaming and eases for discrete follows", () => {
    expect(followScrollBehavior(true, false)).toBe("auto");
    expect(followScrollBehavior(false, false)).toBe("smooth");
  });

  it("collapses to instant under prefers-reduced-motion", () => {
    expect(followScrollBehavior(false, true)).toBe("auto");
    expect(followScrollBehavior(true, true)).toBe("auto");
  });
});

describe("prefersReducedMotion", () => {
  it("reads the media query and defaults to no-preference without one", () => {
    expect(
      prefersReducedMotion((q) => ({ matches: q.includes("reduce") })),
    ).toBe(true);
    expect(prefersReducedMotion(() => ({ matches: false }))).toBe(false);
  });
});

describe("scrollToLatest", () => {
  it("passes the chosen behavior through", () => {
    const calls: ScrollToOptions[] = [];
    const element = {
      scrollHeight: 500,
      scrollTo: (options: ScrollToOptions) => calls.push(options),
    };
    scrollToLatest(element as never, "auto");
    expect(calls).toEqual([{ top: 500, behavior: "auto" }]);
  });
});
