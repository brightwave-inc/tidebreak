import { describe, expect, it } from "vitest";

import { stillFollowing, updateCardFor, updateNoticeKey } from "./updateCard";
import type { DesktopUpdateState } from "./updates";

const idle: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

function cardAt(
  state: DesktopUpdateState,
  overrides: Partial<Parameters<typeof updateCardFor>[0]> = {},
) {
  return updateCardFor({
    state,
    explicitCheck: null,
    followingDownload: false,
    dismissedKey: null,
    appVersion: "0.114.0",
    ...overrides,
  });
}

describe("updateCardFor", () => {
  it("keeps a card up through a download you start from the card", () => {
    const available: DesktopUpdateState = {
      ...idle,
      status: "available",
      version: "0.115.0",
    };
    expect(cardAt(available)).toEqual({
      kind: "available",
      version: "0.115.0",
      error: null,
    });

    // You choose Download: the card follows the download while it runs.
    const downloading: DesktopUpdateState = {
      ...available,
      status: "downloading",
    };
    expect(stillFollowing(downloading.status)).toBe(true);
    expect(cardAt(downloading, { followingDownload: true })).toEqual({
      kind: "progress",
      status: "downloading",
      version: "0.115.0",
    });

    // It settles on the ready card…
    const ready: DesktopUpdateState = { ...available, status: "ready" };
    expect(stillFollowing(ready.status)).toBe(false);
    expect(cardAt(ready)).toEqual({
      kind: "ready",
      version: "0.115.0",
      error: null,
    });

    // …or on the available card again, with the reason it failed.
    const failed: DesktopUpdateState = {
      ...available,
      error:
        "Not enough disk space to download the update. Free up space, then try again.",
    };
    expect(stillFollowing(failed.status)).toBe(false);
    expect(cardAt(failed)).toEqual({
      kind: "available",
      version: "0.115.0",
      error:
        "Not enough disk space to download the update. Free up space, then try again.",
    });
  });

  it("stays quiet while a background download runs", () => {
    expect(
      cardAt({ ...idle, status: "downloading", version: "0.115.0" }),
    ).toBeNull();
  });

  it("leaves the result of a check you asked for on screen", () => {
    expect(cardAt(idle, { explicitCheck: "settled" })).toEqual({
      kind: "up-to-date",
      version: "0.114.0",
    });
    expect(
      cardAt(
        { ...idle, error: "Could not check for updates. Try again later." },
        { explicitCheck: "settled" },
      ),
    ).toEqual({
      kind: "failed",
      message: "Could not check for updates. Try again later.",
    });
    expect(cardAt(idle)).toBeNull();
  });

  it("hides only the notice you dismissed", () => {
    const available: DesktopUpdateState = {
      ...idle,
      status: "available",
      version: "0.115.0",
    };
    const ready: DesktopUpdateState = { ...available, status: "ready" };
    const dismissedKey = updateNoticeKey(available);

    expect(cardAt(available, { dismissedKey })).toBeNull();
    expect(cardAt(ready, { dismissedKey })?.kind).toBe("ready");
  });
});
