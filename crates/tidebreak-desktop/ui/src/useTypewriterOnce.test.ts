// @vitest-environment jsdom
import { renderHook, act } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useTypewriterOnce } from "./useTypewriterOnce";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("phase label typing", () => {
  it("never renders a blank frame", () => {
    // The row it labels also shimmers while its phase is live, so an empty frame
    // does not read as text arriving — it reads as something that failed to
    // load. This was visible for most of a second on a real phase line.
    const { result } = renderHook(() =>
      useTypewriterOnce("Requesting folder access", true),
    );
    expect(result.current).toBe("R");
  });

  it("takes about as long for a long label as a short one", () => {
    // Paced per character, a phase line that grew as its calls settled crawled
    // for over a second while a short one appeared at once.
    const long = "Checking connected folders and 1 other task";
    const { result } = renderHook(() => useTypewriterOnce(long, true));

    act(() => void vi.advanceTimersByTime(400));
    expect(result.current).toBe(long);
  });

  it("finishes the label when Strict Mode remounts its effects", () => {
    const label = "Waiting for your answer";
    const { result } = renderHook(() => useTypewriterOnce(label, true), {
      wrapper: StrictMode,
    });

    expect(result.current).toBe("W");
    act(() => void vi.advanceTimersByTime(400));
    expect(result.current).toBe(label);
  });

  it("shows a settled label at once and never animates it", () => {
    const { result } = renderHook(() =>
      useTypewriterOnce("Browsed files", false),
    );
    expect(result.current).toBe("Browsed files");
  });
});
