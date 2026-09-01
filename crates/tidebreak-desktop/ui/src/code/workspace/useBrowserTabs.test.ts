// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LayoutState } from "@/panel/panelTypes";
import { closeEditorTab, openCodeEditor } from "../codeChrome";
import { useBrowserTabs } from "./useBrowserTabs";

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

function setup(initial: LayoutState) {
  const setLayout = vi.fn();
  const hook = renderHook(
    ({ layout }: { layout: LayoutState }) =>
      useBrowserTabs({ workspaceId: "ws-1", layout, setLayout }),
    { initialProps: { layout: initial } },
  );
  return { ...hook, setLayout };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
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

  it("closes a native browser once per tab, and every open one on unmount", () => {
    const one = withBrowser(EMPTY, "b1");
    const two = withBrowser(one, "b2");
    const { rerender, unmount } = setup(two);
    rerender({ layout: one });
    rerender({ layout: { ...one } });
    expect(mocks.close).toHaveBeenCalledTimes(1);
    expect(mocks.close).toHaveBeenCalledWith("ws-1", "b2");

    unmount();
    expect(mocks.close).toHaveBeenCalledTimes(2);
    expect(mocks.close).toHaveBeenLastCalledWith("ws-1", "b1");
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
