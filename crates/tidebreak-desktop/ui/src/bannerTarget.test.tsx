// @vitest-environment jsdom
import { act, cleanup, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  BANNER_TARGET_TTL_MS,
  forgetBannerTarget,
  rememberBannerTarget,
  takeBannerTarget,
  useBannerTargetNavigation,
} from "./bannerTarget";
import { renderWithRouter } from "./test/router";

function Shell() {
  useBannerTargetNavigation();
  return null;
}

afterEach(() => {
  cleanup();
  forgetBannerTarget();
});

describe("banner targets", () => {
  it("hands out the newest target once", () => {
    rememberBannerTarget("/c/chat-1", () => true, 1_000);
    rememberBannerTarget("/c/chat-2", () => true, 2_000);

    expect(takeBannerTarget(3_000)).toBe("/c/chat-2");
    expect(takeBannerTarget(3_000)).toBeNull();
  });

  it("drops a target that is too old to explain this activation", () => {
    rememberBannerTarget("/c/chat-1", () => true, 0);

    expect(takeBannerTarget(BANNER_TARGET_TTL_MS + 1)).toBeNull();
  });

  it("drops a target whose conversation no longer wants you", () => {
    rememberBannerTarget("/c/chat-1", () => false, 0);

    expect(takeBannerTarget(1)).toBeNull();
  });

  it("opens the banner's conversation when the app comes back", async () => {
    const { router } = await renderWithRouter(<Shell />, {
      initialUrl: "/code",
    });
    rememberBannerTarget("/code/w/ws-1");

    act(() => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-1"),
    );
  });

  it("leaves you where you are when no banner explains the return", async () => {
    const { router } = await renderWithRouter(<Shell />, {
      initialUrl: "/c/chat-1",
    });

    act(() => {
      window.dispatchEvent(new Event("focus"));
    });

    expect(router.state.location.pathname).toBe("/c/chat-1");
  });
});
