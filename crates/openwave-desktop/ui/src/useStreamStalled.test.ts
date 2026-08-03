// @vitest-environment jsdom
import { renderHook, act } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { STREAM_STALL_MS, useStreamStalled } from "./useStreamStalled";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("useStreamStalled", () => {
  it("stalls after the quiet period, clears on activity, and ends with the turn", () => {
    const { result, rerender } = renderHook(
      ({ busy, seq }) => useStreamStalled(busy, seq),
      { initialProps: { busy: true, seq: 1 } },
    );

    expect(result.current).toBe(false);
    act(() => void vi.advanceTimersByTime(STREAM_STALL_MS));
    expect(result.current).toBe(true);

    // A delta arrives: the stall clears immediately and the timer restarts.
    rerender({ busy: true, seq: 2 });
    expect(result.current).toBe(false);
    act(() => void vi.advanceTimersByTime(STREAM_STALL_MS - 1));
    expect(result.current).toBe(false);
    act(() => void vi.advanceTimersByTime(1));
    expect(result.current).toBe(true);

    // The turn closes: never stalled while idle, however long it sits.
    rerender({ busy: false, seq: 2 });
    expect(result.current).toBe(false);
    act(() => void vi.advanceTimersByTime(STREAM_STALL_MS * 2));
    expect(result.current).toBe(false);
  });
});
