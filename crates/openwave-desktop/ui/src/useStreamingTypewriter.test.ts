// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useStreamingTypewriter } from "./useStreamingTypewriter";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("useStreamingTypewriter", () => {
  it("shows historical transcript content immediately", () => {
    const { result, rerender } = renderHook(
      ({ text }) => useStreamingTypewriter(text, false),
      { initialProps: { text: "Searched the web" } },
    );

    expect(result.current).toBe("Searched the web");
    rerender({ text: "Read a file" });
    expect(result.current).toBe("Read a file");
  });

  it("types a later live update", async () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "Searching the web", live: true } },
    );

    rerender({ text: "Searching the web and 1 other task", live: true });
    expect(result.current).not.toBe("Searching the web and 1 other task");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(result.current).toBe("Searching the web and 1 other task");
  });

  it("drains the remaining buffer smoothly when a live step settles", async () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "Searching the web", live: true } },
    );

    rerender({ text: "Searching the web and 1 other task", live: true });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(20);
    });
    rerender({ text: "Searching the web and 1 other task", live: false });

    expect(result.current).not.toBe("Searching the web and 1 other task");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(result.current).toBe("Searching the web and 1 other task");
  });

  // A reconnect replays the active turn's whole journal in a burst (#1716);
  // the animation must not re-type prose the reader already watched stream.
  it("snaps instead of animating when catch-up puts it far behind", async () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "", live: true } },
    );

    const replayed = "already-streamed prose ".repeat(50).trim();
    rerender({ text: replayed, live: true });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(20);
    });
    expect(result.current).toBe(replayed);
  });
});
