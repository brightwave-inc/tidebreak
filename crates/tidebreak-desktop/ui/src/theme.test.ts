// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { isThemeMode, resolveTheme, setThemeMode, useTheme } from "./theme";

describe("theme", () => {
  it("accepts only the three known modes", () => {
    expect(isThemeMode("light")).toBe(true);
    expect(isThemeMode("dark")).toBe(true);
    expect(isThemeMode("system")).toBe(true);
    expect(isThemeMode("")).toBe(false);
    expect(isThemeMode("Dark")).toBe(false);
    expect(isThemeMode(null)).toBe(false);
    expect(isThemeMode(undefined)).toBe(false);
  });

  it("resolves explicit modes without consulting the system", () => {
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("dark")).toBe("dark");
  });
});

/** A `prefers-color-scheme` query whose answer the test can flip. */
function stubMediaQuery() {
  const handlers = new Set<() => void>();
  let dark = false;
  window.matchMedia = ((query: string) => ({
    media: query,
    get matches() {
      return dark;
    },
    addEventListener: (_: string, handler: () => void) =>
      void handlers.add(handler),
    removeEventListener: (_: string, handler: () => void) =>
      void handlers.delete(handler),
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
    onchange: null,
  })) as unknown as typeof window.matchMedia;
  return {
    flip(next: boolean) {
      dark = next;
      act(() => {
        for (const handler of [...handlers]) handler();
      });
    },
  };
}

describe("useTheme", () => {
  beforeEach(() => setThemeMode("system"));
  afterEach(cleanup);

  it("hands every consumer the new resolved theme when the OS flips", () => {
    // The OS listener used to write the `dark` class straight to the document
    // without touching React state, so anything driven by the resolved value —
    // the toaster, the grid's colour scheme — kept rendering the old one until
    // it happened to remount.
    const media = stubMediaQuery();
    const first = renderHook(() => useTheme());
    const second = renderHook(() => useTheme());
    expect(first.result.current.resolved).toBe("light");

    media.flip(true);

    expect(first.result.current.resolved).toBe("dark");
    expect(second.result.current.resolved).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("shows a mode change to consumers that did not make it", () => {
    stubMediaQuery();
    const reader = renderHook(() => useTheme());
    const writer = renderHook(() => useTheme());

    act(() => writer.result.current.setMode("dark"));

    expect(reader.result.current.mode).toBe("dark");
    expect(reader.result.current.resolved).toBe("dark");
  });
});
