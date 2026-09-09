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
import type { BrowserHostEvent } from "../browser/browserHost";
import { setAttachedRemotely } from "@/host";

const mocks = vi.hoisted(() => ({
  close: vi.fn(async () => undefined),
  seed: vi.fn(),
  list: vi.fn(
    async () => [] as import("../browser/browserHost").BrowserHostSnapshot[],
  ),
  subscribe: vi.fn(
    async (_listener: (event: BrowserHostEvent) => void) => () => {},
  ),
}));
vi.mock("../browser/browserHost", () => ({
  closeCodeBrowser: mocks.close,
  listIndependentBrowserTabs: mocks.list,
  nativeCodeBrowserHost: { available: () => true, subscribe: mocks.subscribe },
}));
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

describe("agent browser lifecycle", () => {
  it("appends inactive tabs and preserves back-to-back requests", () => {
    const file = openCodeEditor(EMPTY, { type: "file", path: "app.tsx" });
    const { setLayout, result } = setup(file);
    const listener = mocks.subscribe.mock.calls.at(-1)![0];
    act(() => {
      for (const browserId of ["agent-1", "agent-2"])
        listener({
          type: "agent_open_requested",
          workspaceId: "ws-1",
          browserId,
          url: "http://localhost:5173",
        });
    });
    const next = setLayout.mock.calls.at(-1)![0] as LayoutState;
    expect(next.tabs).toEqual([
      ...file.tabs,
      { type: "browser", browserId: "agent-1" },
      { type: "browser", browserId: "agent-2" },
    ]);
    expect(next.activeIndex).toBe(file.activeIndex);
    expect(next.editorSplit).toBe(file.editorSplit);
    expect(result.current.browserInitialUrls).toEqual({
      "agent-1": "http://localhost:5173",
      "agent-2": "http://localhost:5173",
    });
  });

  it("ignores activation events that would select an agent preview", () => {
    const layout = withBrowser(
      openCodeEditor(EMPTY, { type: "file", path: "app.tsx" }),
      "agent-1",
    );
    const { setLayout } = setup({ ...layout, activeIndex: 0 });
    const listener = mocks.subscribe.mock.calls.at(-1)![0];
    act(() =>
      listener({
        type: "agent_activate_requested",
        workspaceId: "ws-1",
        browserId: "agent-1",
      }),
    );
    expect(setLayout).not.toHaveBeenCalled();
  });

  it("discovers tabs opened while another route was showing without selecting them", async () => {
    mocks.list.mockResolvedValueOnce([
      {
        exists: true,
        workspaceId: "ws-1",
        browserId: "agent-existing",
        url: "https://example.test/live",
        title: "Live fixture",
        visible: false,
        independentInput: true,
      },
    ]);
    const { setLayout, result } = setup(EMPTY);
    await act(async () => {});
    expect(mocks.list).toHaveBeenCalledWith("ws-1");
    const next = setLayout.mock.calls.at(-1)![0] as LayoutState;
    expect(next.tabs).toEqual([
      { type: "browser", browserId: "agent-existing" },
    ]);
    expect(next.conversationFocused).toBe(true);
    expect(next.editorSplit).toBeUndefined();
    expect(result.current.browserTitles["agent-existing"]).toBe("Live fixture");
  });

  it("does not resurrect a tab closed while native discovery is pending", async () => {
    let resolve!: (
      tabs: import("../browser/browserHost").BrowserHostSnapshot[],
    ) => void;
    mocks.list.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const { setLayout } = setup(EMPTY);
    await act(async () => {});
    const listener = mocks.subscribe.mock.calls.at(-1)![0];
    act(() =>
      listener({
        type: "agent_closed_tab",
        workspaceId: "ws-1",
        browserId: "agent-closed",
      }),
    );
    await act(async () =>
      resolve([
        {
          exists: true,
          workspaceId: "ws-1",
          browserId: "agent-closed",
          independentInput: true,
        },
      ]),
    );
    expect(setLayout).not.toHaveBeenCalled();
    expect(mocks.seed).not.toHaveBeenCalled();
  });

  it("never adopts a discovered tab from another workspace or after unmount", async () => {
    let resolve!: (
      tabs: import("../browser/browserHost").BrowserHostSnapshot[],
    ) => void;
    mocks.list.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const { setLayout, unmount } = setup(EMPTY);
    await act(async () => {});
    unmount();
    await act(async () =>
      resolve([
        {
          exists: true,
          workspaceId: "ws-1",
          browserId: "same",
          independentInput: true,
        },
        {
          exists: true,
          workspaceId: "other",
          browserId: "other",
          independentInput: true,
        },
      ]),
    );
    expect(setLayout).not.toHaveBeenCalled();
  });
});
