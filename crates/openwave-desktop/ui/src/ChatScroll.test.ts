import { describe, expect, it, vi } from "vitest";
import {
  AUTO_SCROLL_THRESHOLD_PX,
  isNearBottom,
  scrollToLatest,
} from "./ChatScroll";

describe("chat scroll behavior", () => {
  it("only follows new activity near the bottom", () => {
    expect(
      isNearBottom({ scrollTop: 828, clientHeight: 100, scrollHeight: 1_000 }),
    ).toBe(true);
    expect(
      isNearBottom({ scrollTop: 827, clientHeight: 100, scrollHeight: 1_000 }),
    ).toBe(false);
    expect(AUTO_SCROLL_THRESHOLD_PX).toBe(72);
  });

  it("scrolls to the latest item when explicitly requested", () => {
    const scrollTo = vi.fn();
    scrollToLatest({ scrollHeight: 2_000, scrollTo });
    expect(scrollTo).toHaveBeenCalledWith({ top: 2_000, behavior: "smooth" });
  });
});
