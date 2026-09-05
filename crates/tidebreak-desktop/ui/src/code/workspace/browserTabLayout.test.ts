// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { EMPTY_LAYOUT, type LayoutState } from "@/panel/panelTypes";
import {
  BROWSER_TABS_STORAGE_PREFIX,
  readBrowserTabLayout,
  writeBrowserTabLayout,
} from "./browserTabLayout";

afterEach(() => window.localStorage.clear());

describe("browser tab layout persistence", () => {
  it("preserves browser order, selected tabs, and split placement without saving other panels", () => {
    const layout: LayoutState = {
      tabs: [
        { type: "file", path: "private.txt" },
        { type: "browser", browserId: "one" },
        { type: "browser", browserId: "two" },
      ],
      activeIndex: 2,
      fullscreen: true,
      editorSplit: {
        tabs: [
          { type: "diff", path: "private.txt" },
          { type: "browser", browserId: "three" },
          { type: "browser", browserId: "four" },
        ],
        activeIndex: 2,
        focused: true,
      },
    };
    writeBrowserTabLayout("ws-1", layout);
    const restored = readBrowserTabLayout("ws-1");
    expect(restored.tabs).toEqual(layout.tabs.slice(1));
    expect(restored.activeIndex).toBe(1);
    expect(restored.fullscreen).toBe(true);
    expect(restored.editorSplit).toEqual({
      tabs: layout.editorSplit?.tabs.slice(1),
      activeIndex: 1,
      focused: true,
    });
    expect(
      window.localStorage.getItem(BROWSER_TABS_STORAGE_PREFIX + "ws-1"),
    ).not.toContain("private.txt");
    expect(readBrowserTabLayout("ws-2")).toEqual(EMPTY_LAYOUT);
  });

  it("removes saved membership when the last browser closes", () => {
    writeBrowserTabLayout("ws-1", {
      ...EMPTY_LAYOUT,
      tabs: [{ type: "browser", browserId: "empty" }],
    });
    writeBrowserTabLayout("ws-1", {
      ...EMPTY_LAYOUT,
      tabs: [{ type: "file", path: "README.md" }],
    });
    expect(
      window.localStorage.getItem(BROWSER_TABS_STORAGE_PREFIX + "ws-1"),
    ).toBeNull();
    expect(readBrowserTabLayout("ws-1")).toEqual(EMPTY_LAYOUT);
  });

  it("validates stored IDs and strips non-browser panels and fields", () => {
    window.localStorage.setItem(
      BROWSER_TABS_STORAGE_PREFIX + "ws-1",
      JSON.stringify({
        tabs: "browser.good,browser.bad/id,terminal.shell,file.secret,browser.good",
        split: "browser.other",
        url: "https://private.test",
        controller: "agent",
      }),
    );
    const restored = readBrowserTabLayout("ws-1");
    expect(restored.tabs).toEqual([{ type: "browser", browserId: "good" }]);
    expect(restored.editorSplit?.tabs).toEqual([
      { type: "browser", browserId: "other" },
    ]);
    expect(restored).not.toHaveProperty("url");
    expect(restored).not.toHaveProperty("controller");
  });

  it("ignores corrupt or oversized storage and tolerates unavailable storage", () => {
    for (const value of [
      "{",
      "null",
      "[]",
      JSON.stringify({ tabs: 4 }),
      " ".repeat(65_537),
    ]) {
      window.localStorage.setItem(BROWSER_TABS_STORAGE_PREFIX + "ws-1", value);
      expect(readBrowserTabLayout("ws-1")).toEqual(EMPTY_LAYOUT);
    }
    const unavailable = () => {
      throw new Error("unavailable");
    };
    expect(readBrowserTabLayout("ws-1", { getItem: unavailable })).toEqual(
      EMPTY_LAYOUT,
    );
    expect(() =>
      writeBrowserTabLayout("ws-1", EMPTY_LAYOUT, {
        setItem: unavailable,
        removeItem: unavailable,
      }),
    ).not.toThrow();
  });
});
