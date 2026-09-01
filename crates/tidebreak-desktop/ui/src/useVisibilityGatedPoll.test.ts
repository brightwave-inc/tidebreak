// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  startVisibilityGatedPoll,
  useVisibilityGatedPoll,
} from "./useVisibilityGatedPoll";

function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => hidden,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  setHidden(false);
});
afterEach(() => {
  vi.useRealTimers();
  setHidden(false);
});

describe("useVisibilityGatedPoll", () => {
  it("polls on the visible cadence, pauses hidden, and reads at once on return", () => {
    const poll = vi.fn();
    renderHook(() => useVisibilityGatedPoll(poll, 1_000));
    expect(poll).not.toHaveBeenCalled();

    act(() => void vi.advanceTimersByTime(2_000));
    expect(poll).toHaveBeenCalledTimes(2);

    act(() => setHidden(true));
    act(() => void vi.advanceTimersByTime(10_000));
    expect(poll).toHaveBeenCalledTimes(2);

    act(() => setHidden(false));
    expect(poll).toHaveBeenCalledTimes(3);
    act(() => void vi.advanceTimersByTime(1_000));
    expect(poll).toHaveBeenCalledTimes(4);
  });

  it("keeps a slower cadence hidden when one is given", () => {
    const poll = vi.fn();
    renderHook(() =>
      useVisibilityGatedPoll(poll, 1_000, { hiddenIntervalMs: 5_000 }),
    );
    act(() => setHidden(true));
    act(() => void vi.advanceTimersByTime(4_999));
    expect(poll).not.toHaveBeenCalled();
    act(() => void vi.advanceTimersByTime(1));
    expect(poll).toHaveBeenCalledTimes(1);
  });

  it("reads on a signal after mount, hidden or not, but not on the mount value", () => {
    const poll = vi.fn();
    const { rerender } = renderHook(
      ({ revision }: { revision: number }) =>
        useVisibilityGatedPoll(poll, 60_000, { revision }),
      { initialProps: { revision: 7 } },
    );
    expect(poll).not.toHaveBeenCalled();

    rerender({ revision: 8 });
    expect(poll).toHaveBeenCalledTimes(1);

    act(() => setHidden(true));
    rerender({ revision: 9 });
    expect(poll).toHaveBeenCalledTimes(2);
  });

  it("does nothing while disabled and calls the latest poll function", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(
      ({ poll, enabled }: { poll: () => void; enabled: boolean }) =>
        useVisibilityGatedPoll(poll, 1_000, { enabled, revision: 0 }),
      { initialProps: { poll: first, enabled: false } },
    );
    act(() => void vi.advanceTimersByTime(3_000));
    expect(first).not.toHaveBeenCalled();

    rerender({ poll: second, enabled: true });
    act(() => void vi.advanceTimersByTime(1_000));
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });
});

describe("startVisibilityGatedPoll", () => {
  it("stops cleanly and ignores visibility changes afterwards", () => {
    const poll = vi.fn();
    const stop = startVisibilityGatedPoll(poll, 1_000);
    vi.advanceTimersByTime(1_000);
    expect(poll).toHaveBeenCalledTimes(1);
    stop();
    vi.advanceTimersByTime(5_000);
    setHidden(true);
    setHidden(false);
    expect(poll).toHaveBeenCalledTimes(1);
  });
});
