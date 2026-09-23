// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useStreamingTypewriter } from "./useStreamingTypewriter";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** Animation frames that run only when the test says, at the time it names. */
function manualFrames() {
  let nextId = 0;
  const pending = new Map<number, FrameRequestCallback>();
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    nextId += 1;
    pending.set(nextId, callback);
    return nextId;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    pending.delete(id);
  });
  return {
    run(now: number) {
      const due = [...pending.values()];
      pending.clear();
      act(() => {
        for (const callback of due) callback(now);
      });
    },
  };
}

describe("useStreamingTypewriter", () => {
  it("reveals live text on animation frames", () => {
    const frames = manualFrames();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "Searching", live: true } },
    );
    rerender({ text: "Searching the web", live: true });
    expect(result.current).toBe("Searching");

    frames.run(0);
    expect(result.current.length).toBeGreaterThan("Searching".length);
    expect("Searching the web".startsWith(result.current)).toBe(true);
  });

  // Each reveal re-renders the message. A frame that comes late means the
  // last render ran over its budget; revealing again right away would stack
  // another long render on a main thread that is already behind.
  it("gives back a late frame and reveals its time on the next one", () => {
    const frames = manualFrames();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "", live: true } },
    );
    rerender({ text: "a".repeat(500), live: true });
    frames.run(0);
    const first = result.current.length;
    frames.run(16);
    const second = result.current.length;
    expect(second).toBeGreaterThan(first);

    frames.run(16 + 60);
    expect(result.current.length).toBe(second);

    frames.run(16 + 60 + 16);
    expect(result.current.length - second).toBeGreaterThan(second - first);
  });

  it("never gives back two frames in a row", () => {
    const frames = manualFrames();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "", live: true } },
    );
    rerender({ text: "a".repeat(500), live: true });
    frames.run(0);
    frames.run(60);
    const afterSkip = result.current.length;
    frames.run(120);
    expect(result.current.length).toBeGreaterThan(afterSkip);
  });

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

  it("shows the full text when a live step settles", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "Searching the web", live: true } },
    );

    rerender({ text: "Searching the web and 1 other task", live: true });
    rerender({ text: "Searching the web and 1 other task", live: false });

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

  it("shows the full text immediately when reduced motion is on", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query.includes("prefers-reduced-motion"),
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));
    const { result, rerender } = renderHook(
      ({ text, live }) => useStreamingTypewriter(text, live),
      { initialProps: { text: "Searching", live: true } },
    );
    rerender({ text: "Searching the web", live: true });
    expect(result.current).toBe("Searching the web");
    vi.unstubAllGlobals();
  });
});
