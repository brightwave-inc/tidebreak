// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LayoutState } from "@/panel/panelTypes";
import { focusEditorTab, moveEditorTab, openCodeEditor } from "../codeChrome";
import { useCodeUiStore } from "../CodeUiStore";
import { useEditorTabs } from "./useEditorTabs";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), message: vi.fn() },
}));

const EMPTY: LayoutState = { tabs: [], activeIndex: 0, fullscreen: false };

function setup(initial: LayoutState) {
  const setLayout = vi.fn();
  const hook = renderHook(
    ({ layout }: { layout: LayoutState }) =>
      useEditorTabs({ layout, setLayout }),
    { initialProps: { layout: initial } },
  );
  return { ...hook, setLayout };
}

beforeEach(() =>
  useCodeUiStore.setState(useCodeUiStore.getInitialState(), true),
);
afterEach(cleanup);

describe("useEditorTabs", () => {
  it("reveals a line only when one is asked for, counting each ask", () => {
    const { result, setLayout } = setup(EMPTY);
    act(() => result.current.openFile("a.ts", 12));
    expect(result.current.fileReveal).toEqual({
      path: "a.ts",
      line: 12,
      revision: 1,
    });
    act(() => result.current.openFile("a.ts", 12));
    expect(result.current.fileReveal?.revision).toBe(2);
    act(() => result.current.openFile("b.ts"));
    expect(result.current.fileReveal).toBeNull();
    expect(setLayout).toHaveBeenCalledTimes(3);
    const last = setLayout.mock.calls[2]?.[0] as LayoutState;
    expect(last.tabs).toEqual([{ type: "file", path: "b.ts" }]);
  });

  it("counts quick-open asks per region", () => {
    const { result } = setup(EMPTY);
    expect(result.current.quickOpenRequest).toBe(0);
    act(() => result.current.requestNewTab("secondary"));
    expect(result.current.quickOpenRequest).toBe(1);
    expect(result.current.quickOpenTarget).toBe("secondary");
    act(() => result.current.requestNewTab("primary"));
    expect(result.current.quickOpenRequest).toBe(2);
    expect(result.current.quickOpenTarget).toBe("primary");
  });

  it("answers the shell's asks once, in the focused group", () => {
    const file = openCodeEditor(EMPTY, { type: "file", path: "a.ts" });
    const split = focusEditorTab(
      moveEditorTab(file, "primary", 0, "secondary"),
      0,
      "secondary",
    );
    const { result } = setup(split);
    expect(result.current.splitFocused).toBe(true);

    act(() => useCodeUiStore.getState().requestQuickOpen());
    expect(result.current.quickOpenRequest).toBe(1);
    expect(result.current.quickOpenTarget).toBe("secondary");
    expect(useCodeUiStore.getState().quickOpenPending).toBe(false);

    act(() => useCodeUiStore.getState().requestNewTabMenu());
    expect(result.current.newTabMenuRequest).toBe(1);
    expect(result.current.newTabMenuRegion).toBe("secondary");
    expect(useCodeUiStore.getState().newTabMenuPending).toBe(false);
  });

  it("opens the path the palette names", () => {
    const { setLayout } = setup(EMPTY);
    act(() => useCodeUiStore.getState().requestOpenFilePath("src/x.ts"));
    const next = setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(next.tabs).toEqual([{ type: "file", path: "src/x.ts" }]);
    expect(useCodeUiStore.getState().openFilePending).toBeNull();
  });
});
