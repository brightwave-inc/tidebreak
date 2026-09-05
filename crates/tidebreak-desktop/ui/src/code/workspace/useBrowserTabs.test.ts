// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LayoutState } from "@/panel/panelTypes";
import { closeEditorTab, openCodeEditor } from "../codeChrome";
import { useBrowserTabs } from "./useBrowserTabs";
import {
  readBrowserTabLayout,
  writeBrowserTabLayout,
} from "./browserTabLayout";
import { createElement, StrictMode } from "react";
import { setAttachedRemotely } from "@/host";

const mocks = vi.hoisted(() => ({
  close: vi.fn(async () => undefined),
  seed: vi.fn(),
}));
vi.mock("../browser/browserHost", () => ({ closeCodeBrowser: mocks.close }));
vi.mock("../browser/browserPersistence", () => ({
  seedBrowserSession: mocks.seed,
}));

const EMPTY: LayoutState = { tabs: [], activeIndex: 0, fullscreen: false };

function withBrowser(layout: LayoutState, browserId: string) {
  return openCodeEditor(layout, { type: "browser", browserId });
}

function setup(initial: LayoutState, workspaceId = "ws-1") {
  const setLayout = vi.fn();
  const hook = renderHook(
    ({ layout }: { layout: LayoutState }) =>
      useBrowserTabs({ workspaceId, layout, setLayout }),
    { initialProps: { layout: initial } },
  );
  return { ...hook, setLayout };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  window.localStorage.clear();
  setAttachedRemotely(false);
});

describe("useBrowserTabs", () => {
  it("titles every open browser and drops the title of a closed one", () => {
    const one = withBrowser(EMPTY, "b1");
    const { result, rerender } = setup(one);
    expect(result.current.browserTitles).toEqual({ b1: "Browser" });

    act(() => result.current.setBrowserTitle("b1", "Docs"));
    expect(result.current.browserTitles).toEqual({ b1: "Docs" });

    rerender({ layout: withBrowser(one, "b2") });
    expect(result.current.browserTitles).toEqual({ b1: "Docs", b2: "Browser" });

    rerender({ layout: closeEditorTab(withBrowser(one, "b2"), 0, "primary") });
    expect(mocks.close).toHaveBeenCalledWith("ws-1", "b1");
    expect(result.current.browserTitles).toEqual({ b2: "Browser" });
  });

  it("closes a native browser once per removed tab and preserves open ones on unmount", () => {
    const one = withBrowser(EMPTY, "b1");
    const two = withBrowser(one, "b2");
    const { rerender, unmount } = setup(two);
    rerender({ layout: one });
    rerender({ layout: { ...one } });
    expect(mocks.close).toHaveBeenCalledTimes(1);
    expect(mocks.close).toHaveBeenCalledWith("ws-1", "b2");

    unmount();
    expect(mocks.close).toHaveBeenCalledTimes(1);
    expect(readBrowserTabLayout("ws-1").tabs).toEqual(one.tabs);
  });

  it("restores browser IDs after leaving the workspace, including an empty browser", () => {
    const saved = withBrowser(withBrowser(EMPTY, "loaded"), "empty");
    const first = setup(saved);
    first.unmount();
    expect(mocks.close).not.toHaveBeenCalled();

    const restored = setup(EMPTY);
    expect(restored.setLayout).toHaveBeenCalledTimes(1);
    const next = restored.setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(next.tabs).toEqual(saved.tabs);
    expect(next.activeIndex).toBe(saved.activeIndex);
    expect(mocks.seed).not.toHaveBeenCalled();
    restored.rerender({ layout: next });
    expect(restored.result.current.browserTitles).toEqual({
      loaded: "Browser",
      empty: "Browser",
    });

    restored.rerender({ layout: EMPTY });
    expect(mocks.close.mock.calls).toEqual([
      ["ws-1", "loaded"],
      ["ws-1", "empty"],
    ]);
    restored.unmount();
    expect(setup(EMPTY).setLayout).not.toHaveBeenCalled();
  });

  it("does not replace an explicit URL layout with saved browser tabs", () => {
    writeBrowserTabLayout("ws-1", withBrowser(EMPTY, "saved"));
    const explicit = openCodeEditor(EMPTY, { type: "file", path: "README.md" });
    const { setLayout } = setup(explicit);
    expect(setLayout).not.toHaveBeenCalled();
    expect(mocks.close).not.toHaveBeenCalled();
  });

  it("does not restore another workspace's browser tabs", () => {
    writeBrowserTabLayout("ws-1", withBrowser(EMPTY, "saved"));
    expect(setup(EMPTY, "ws-2").setLayout).not.toHaveBeenCalled();
    expect(readBrowserTabLayout("ws-1").tabs).toEqual([
      { type: "browser", browserId: "saved" },
    ]);
  });

  it("does not restore or erase local tabs while attached to another computer", () => {
    writeBrowserTabLayout("ws-1", withBrowser(EMPTY, "local"));
    setAttachedRemotely(true);
    expect(setup(EMPTY).setLayout).not.toHaveBeenCalled();
    expect(readBrowserTabLayout("ws-1").tabs).toEqual([
      { type: "browser", browserId: "local" },
    ]);
  });

  it("keeps saved tabs during Strict Mode replay and delayed router navigation", () => {
    writeBrowserTabLayout("ws-1", withBrowser(EMPTY, "saved"));
    const setLayout = vi.fn();
    const { rerender } = renderHook(
      ({ layout }: { layout: LayoutState }) =>
        useBrowserTabs({ workspaceId: "ws-1", layout, setLayout }),
      {
        initialProps: { layout: EMPTY },
        wrapper: ({ children }) => createElement(StrictMode, null, children),
      },
    );
    rerender({ layout: { ...EMPTY } });
    expect(setLayout).toHaveBeenCalledTimes(1);
    expect(mocks.close).not.toHaveBeenCalled();
    expect(readBrowserTabLayout("ws-1").tabs).toEqual([
      { type: "browser", browserId: "saved" },
    ]);
  });

  it("seeds a new browser, remembers its start page, and opens its tab", () => {
    const { result, setLayout } = setup(EMPTY);
    act(() => result.current.openBrowser("https://example.test", "primary"));
    const browserId = mocks.seed.mock.calls[0]?.[0].browserId as string;
    expect(mocks.seed).toHaveBeenCalledWith({
      browserId,
      workspaceId: "ws-1",
      initialUrl: "https://example.test",
    });
    expect(result.current.browserInitialUrls).toEqual({
      [browserId]: "https://example.test",
    });
    expect(result.current.browserTitles).toEqual({ [browserId]: "Browser" });
    const next = setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(next.tabs).toEqual([{ type: "browser", browserId }]);

    act(() => result.current.openBrowser());
    expect(Object.keys(result.current.browserInitialUrls)).toEqual([browserId]);
  });
});
