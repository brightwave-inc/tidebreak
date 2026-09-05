// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { createElement, StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setAttachedRemotely } from "@/host";
import { EMPTY_LAYOUT, type LayoutState } from "@/panel/panelTypes";
import {
  BROWSER_TABS_STORAGE_PREFIX,
  readBrowserTabLayout,
  writeBrowserTabLayout,
} from "../workspace/browserTabLayout";
import { foregroundBrowserScope } from "./foregroundBrowserScope";
import { useForegroundBrowserTabs } from "./useForegroundBrowserTabs";

const mocks = vi.hoisted(() => ({
  close: vi.fn(async () => undefined),
  seed: vi.fn(),
}));
vi.mock("./browserHost", () => ({ closeCodeBrowser: mocks.close }));
vi.mock("./browserPersistence", () => ({ seedBrowserSession: mocks.seed }));

function browserLayout(...ids: string[]): LayoutState {
  return {
    ...EMPTY_LAYOUT,
    tabs: ids.map((browserId) => ({ type: "browser", browserId })),
  };
}

function setup(initial: LayoutState, chatId = "chat-1", strict = false) {
  const setLayout = vi.fn();
  const openPanel = vi.fn();
  const hook = renderHook(
    ({ layout }: { layout: LayoutState }) =>
      useForegroundBrowserTabs({ chatId, layout, setLayout, openPanel }),
    {
      initialProps: { layout: initial },
      wrapper: strict
        ? ({ children }) => createElement(StrictMode, null, children)
        : undefined,
    },
  );
  return { ...hook, setLayout, openPanel };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  window.localStorage.clear();
  setAttachedRemotely(false);
});

describe("foreground browser tab membership", () => {
  it("restores the same browser IDs on a bare chat revisit without reseeding native recovery", () => {
    const layout = { ...browserLayout("loaded", "blank"), activeIndex: 1 };
    const first = setup(layout);
    first.unmount();
    expect(mocks.close).not.toHaveBeenCalled();

    const restored = setup(EMPTY_LAYOUT);
    expect(restored.setLayout).toHaveBeenCalledTimes(1);
    const next = restored.setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(next.tabs).toEqual(layout.tabs);
    expect(next.activeIndex).toBe(1);
    restored.rerender({ layout: next });
    expect(restored.result.current.browserTitles).toEqual({
      loaded: "Browser",
      blank: "Browser",
    });
    expect(restored.result.current.browserInitialUrls).toEqual({});
    expect(mocks.seed).not.toHaveBeenCalled();
    expect(mocks.close).not.toHaveBeenCalled();
  });

  it("restores persisted membership after a fresh mount only in its foreground chat scope", () => {
    const scope = foregroundBrowserScope("chat-1");
    writeBrowserTabLayout(scope, browserLayout("persisted"));
    expect(
      window.localStorage.getItem(BROWSER_TABS_STORAGE_PREFIX + scope),
    ).toBe(JSON.stringify({ tabs: "browser.persisted" }));
    expect(setup(EMPTY_LAYOUT, "chat-2").setLayout).not.toHaveBeenCalled();
    expect(readBrowserTabLayout("chat-1")).toEqual(EMPTY_LAYOUT);
    const restored = setup(EMPTY_LAYOUT);
    expect(restored.setLayout.mock.calls[0]?.[0].tabs).toEqual([
      { type: "browser", browserId: "persisted" },
    ]);
    expect(mocks.seed).not.toHaveBeenCalled();
  });

  it("closes only removed native tabs and never restores the last tab after explicit removal", () => {
    const { rerender, unmount } = setup(browserLayout("one", "two"));
    rerender({ layout: browserLayout("two") });
    rerender({ layout: browserLayout("two") });
    expect(mocks.close.mock.calls).toEqual([
      [foregroundBrowserScope("chat-1"), "one"],
    ]);
    expect(readBrowserTabLayout(foregroundBrowserScope("chat-1")).tabs).toEqual(
      [{ type: "browser", browserId: "two" }],
    );
    rerender({ layout: EMPTY_LAYOUT });
    expect(mocks.close.mock.calls).toEqual([
      [foregroundBrowserScope("chat-1"), "one"],
      [foregroundBrowserScope("chat-1"), "two"],
    ]);
    expect(readBrowserTabLayout(foregroundBrowserScope("chat-1"))).toEqual(
      EMPTY_LAYOUT,
    );
    unmount();
    expect(setup(EMPTY_LAYOUT).setLayout).not.toHaveBeenCalled();
    expect(mocks.close).toHaveBeenCalledTimes(2);
  });

  it("keeps a pending restore through Strict Mode replay and permits closing it once the URL arrives", () => {
    const scope = foregroundBrowserScope("chat-1");
    writeBrowserTabLayout(scope, browserLayout("saved"));
    const restored = setup(EMPTY_LAYOUT, "chat-1", true);
    restored.rerender({ layout: { ...EMPTY_LAYOUT } });
    expect(restored.setLayout).toHaveBeenCalledTimes(1);
    expect(readBrowserTabLayout(scope).tabs).toEqual(
      browserLayout("saved").tabs,
    );
    expect(mocks.close).not.toHaveBeenCalled();

    restored.rerender({ layout: restored.setLayout.mock.calls[0]?.[0] });
    restored.rerender({ layout: EMPTY_LAYOUT });
    restored.rerender({ layout: { ...EMPTY_LAYOUT } });
    expect(mocks.close).toHaveBeenCalledExactlyOnceWith(scope, "saved");
    expect(restored.setLayout).toHaveBeenCalledTimes(1);
    expect(readBrowserTabLayout(scope)).toEqual(EMPTY_LAYOUT);
    restored.unmount();
    expect(setup(EMPTY_LAYOUT).setLayout).not.toHaveBeenCalled();
  });

  it("focuses an existing browser by default and opens a separate tab for an explicit URL", () => {
    const layout: LayoutState = {
      ...browserLayout("existing"),
      tabs: [{ type: "outputs" }, ...browserLayout("existing").tabs],
    };
    const { result, setLayout, openPanel } = setup(layout);
    act(() => result.current.openBrowser());
    expect(openPanel).toHaveBeenCalledExactlyOnceWith({
      type: "browser",
      browserId: "existing",
    });
    expect(setLayout).not.toHaveBeenCalled();
    expect(mocks.seed).not.toHaveBeenCalled();

    act(() => result.current.openBrowser("https://example.test"));
    const browserId = mocks.seed.mock.calls[0]?.[0].browserId as string;
    expect(mocks.seed).toHaveBeenCalledExactlyOnceWith({
      browserId,
      workspaceId: foregroundBrowserScope("chat-1"),
      initialUrl: "https://example.test",
    });
    expect(setLayout.mock.calls[0]?.[0].tabs).toEqual([
      ...layout.tabs,
      { type: "browser", browserId },
    ]);
    expect(setLayout.mock.calls[0]?.[0].activeIndex).toBe(2);
    expect(result.current.browserInitialUrls).toEqual({
      [browserId]: "https://example.test",
    });
  });

  it("keeps an explicit URL layout instead of replacing it with saved browsers", () => {
    const scope = foregroundBrowserScope("chat-1");
    writeBrowserTabLayout(scope, browserLayout("saved"));
    const explicit: LayoutState = {
      ...EMPTY_LAYOUT,
      tabs: [{ type: "outputs" }],
    };
    const { setLayout } = setup(explicit);
    expect(setLayout).not.toHaveBeenCalled();
    expect(mocks.close).not.toHaveBeenCalled();
  });

  it("preserves local membership while the chat attaches to another computer", () => {
    const scope = foregroundBrowserScope("chat-1");
    writeBrowserTabLayout(scope, browserLayout("local"));
    setAttachedRemotely(true);
    expect(setup(EMPTY_LAYOUT).setLayout).not.toHaveBeenCalled();
    expect(readBrowserTabLayout(scope).tabs).toEqual(
      browserLayout("local").tabs,
    );
    expect(mocks.close).not.toHaveBeenCalled();
    expect(mocks.seed).not.toHaveBeenCalled();
  });
});
