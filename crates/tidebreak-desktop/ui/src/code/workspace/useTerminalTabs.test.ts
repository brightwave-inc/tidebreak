// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LayoutState } from "@/panel/panelTypes";
import {
  closeEditorTab,
  focusConversation,
  focusEditorTab,
  openCodeEditor,
} from "../codeChrome";
import { useCodeUiStore } from "../CodeUiStore";
import { useTerminalTabs } from "./useTerminalTabs";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), message: vi.fn() },
}));

const EMPTY: LayoutState = { tabs: [], activeIndex: 0, fullscreen: false };

function withTerminal(layout: LayoutState, terminalId: string) {
  return openCodeEditor(layout, { type: "terminal", terminalId });
}

function setup(initial: LayoutState) {
  let created = 0;
  const client = {
    createCodeTerminal: vi.fn(async () => ({ id: `term-${++created}` })),
    deleteCodeTerminal: vi.fn(async () => undefined),
  };
  const setLayout = vi.fn();
  const hook = renderHook(
    ({ layout }: { layout: LayoutState }) =>
      useTerminalTabs({
        workspaceId: "ws-1",
        client: client as never,
        layout,
        setLayout,
      }),
    { initialProps: { layout: initial } },
  );
  return { ...hook, client, setLayout };
}

beforeEach(() =>
  useCodeUiStore.setState(useCodeUiStore.getInitialState(), true),
);
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("useTerminalTabs", () => {
  it("numbers shells by the lowest free ordinal and keeps names across a close", () => {
    const one = withTerminal(EMPTY, "t1");
    const two = withTerminal(one, "t2");
    const { result, rerender, client } = setup(two);
    expect(result.current.terminalLabels).toEqual({
      t1: "Terminal 1",
      t2: "Terminal 2",
    });

    const withoutFirst = closeEditorTab(two, 0, "primary");
    rerender({ layout: withoutFirst });
    expect(client.deleteCodeTerminal).toHaveBeenCalledWith("ws-1", "t1");
    expect(result.current.terminalLabels).toEqual({ t2: "Terminal 2" });

    rerender({ layout: withTerminal(withoutFirst, "t3") });
    expect(result.current.terminalLabels).toEqual({
      t2: "Terminal 2",
      t3: "Terminal 1",
    });
  });

  it("ends a shell once even when the same layout renders again", () => {
    const one = withTerminal(EMPTY, "t1");
    const { rerender, client } = setup(one);
    rerender({ layout: EMPTY });
    rerender({ layout: { ...EMPTY } });
    expect(client.deleteCodeTerminal).toHaveBeenCalledTimes(1);
  });

  it("opens a shell then a tab for it, reading the layout after the call", async () => {
    const { result, client, setLayout, rerender } = setup(EMPTY);
    const pending = result.current.openTerminal("primary");
    // A tab opened while the create call is in flight is kept.
    const meanwhile = openCodeEditor(EMPTY, { type: "file", path: "a.ts" });
    rerender({ layout: meanwhile });
    await pending;
    expect(client.createCodeTerminal).toHaveBeenCalledWith("ws-1");
    const next = setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(next.tabs).toEqual([
      { type: "file", path: "a.ts" },
      { type: "terminal", terminalId: "term-1" },
    ]);
    await waitFor(() =>
      expect(result.current.terminalLabels).toEqual({ "term-1": "Terminal 1" }),
    );
  });

  it("jumps to the terminal and back to where focus was", () => {
    const file = openCodeEditor(EMPTY, { type: "file", path: "a.ts" });
    const both = withTerminal(file, "t1");
    const onFile = focusEditorTab(both, 0, "primary");
    const { result, setLayout, rerender } = setup(onFile);

    act(() => result.current.toggleTerminal());
    const toTerminal = setLayout.mock.calls[0]?.[0] as LayoutState;
    expect(toTerminal.activeIndex).toBe(1);

    rerender({ layout: toTerminal });
    act(() => result.current.toggleTerminal());
    const back = setLayout.mock.calls[1]?.[0] as LayoutState;
    expect(back.activeIndex).toBe(0);
    expect(back.conversationFocused).toBeFalsy();
  });

  it("returns to the conversation when nothing had focus before the jump", () => {
    const chat = focusConversation(withTerminal(EMPTY, "t1"));
    const { result, setLayout, rerender } = setup(chat);
    act(() => result.current.toggleTerminal());
    const toTerminal = setLayout.mock.calls[0]?.[0] as LayoutState;
    rerender({ layout: toTerminal });
    act(() => result.current.toggleTerminal());
    const back = setLayout.mock.calls[1]?.[0] as LayoutState;
    expect(back.conversationFocused).toBe(true);
  });

  it("answers the shell's terminal chord once", async () => {
    const { client } = setup(EMPTY);
    act(() => useCodeUiStore.getState().requestTerminal());
    await waitFor(() =>
      expect(client.createCodeTerminal).toHaveBeenCalledTimes(1),
    );
    expect(useCodeUiStore.getState().terminalPending).toBe(false);
  });
});
