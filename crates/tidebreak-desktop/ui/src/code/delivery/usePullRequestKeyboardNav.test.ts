// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { usePullRequestKeyboardNav } from "./usePullRequestKeyboardNav";

function press(key: string, init: KeyboardEventInit = {}, target?: Element) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  (target ?? window).dispatchEvent(event);
  return event;
}

function setup(
  overrides: Partial<Parameters<typeof usePullRequestKeyboardNav>[0]> = {},
) {
  const onSelect = vi.fn();
  const onClose = vi.fn();
  const onLoadMore = vi.fn();
  const props = {
    selectedId: "b",
    displayIds: ["a", "b", "c"],
    nextCursor: undefined as string | undefined,
    loadingMore: false,
    onSelect,
    onClose,
    onLoadMore,
    ...overrides,
  };
  const hook = renderHook(
    (next: typeof props) => usePullRequestKeyboardNav(next),
    {
      initialProps: props,
    },
  );
  return { ...hook, props, onSelect, onClose, onLoadMore };
}

afterEach(cleanup);

describe("usePullRequestKeyboardNav", () => {
  it("walks the display order with the arrow keys and clamps at the top", () => {
    const { onSelect, unmount } = setup();
    expect(press("ArrowDown").defaultPrevented).toBe(true);
    expect(onSelect).toHaveBeenLastCalledWith("c");
    expect(press("ArrowUp").defaultPrevented).toBe(true);
    expect(onSelect).toHaveBeenLastCalledWith("a");
    unmount();

    const { onSelect: fromTop } = setup({ selectedId: "a" });
    const up = press("ArrowUp");
    expect(up.defaultPrevented).toBe(false);
    expect(fromTop).not.toHaveBeenCalled();
  });

  it("asks for the next page instead of wrapping at the bottom", () => {
    const { onLoadMore, onSelect } = setup({
      selectedId: "c",
      nextCursor: "page-2",
    });
    expect(press("ArrowDown").defaultPrevented).toBe(true);
    expect(onLoadMore).toHaveBeenCalledWith("page-2");
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("does not ask for a page it is already loading, or one that is not there", () => {
    const loading = setup({
      selectedId: "c",
      nextCursor: "page-2",
      loadingMore: true,
    });
    press("ArrowDown");
    expect(loading.onLoadMore).not.toHaveBeenCalled();
    loading.unmount();

    const last = setup({ selectedId: "c" });
    press("ArrowDown");
    expect(last.onLoadMore).not.toHaveBeenCalled();
  });

  it("closes on Escape and ignores modified, handled, and typed keys", () => {
    const { onClose, onSelect } = setup();
    press("Escape");
    expect(onClose).toHaveBeenCalledTimes(1);

    press("ArrowDown", { metaKey: true });
    press("ArrowDown", { ctrlKey: true });
    press("ArrowDown", { altKey: true });
    expect(onSelect).not.toHaveBeenCalled();

    const handled = new KeyboardEvent("keydown", {
      key: "ArrowDown",
      bubbles: true,
      cancelable: true,
    });
    handled.preventDefault();
    window.dispatchEvent(handled);
    expect(onSelect).not.toHaveBeenCalled();

    const input = document.createElement("input");
    document.body.appendChild(input);
    press("ArrowDown", {}, input);
    expect(onSelect).not.toHaveBeenCalled();
    input.remove();
  });

  it("listens only while a row is selected, and lets go on unmount", () => {
    const { onSelect, onClose, rerender, props, unmount } = setup({
      selectedId: null,
    });
    press("ArrowDown");
    press("Escape");
    expect(onSelect).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();

    rerender({ ...props, selectedId: "a" });
    press("ArrowDown");
    expect(onSelect).toHaveBeenCalledWith("b");

    unmount();
    press("ArrowDown");
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it("calls the handler from the latest render, not the one the listener saw", () => {
    const { rerender, props } = setup({ selectedId: "a" });
    const replacement = vi.fn();
    act(() => rerender({ ...props, onSelect: replacement }));
    press("ArrowDown");
    expect(replacement).toHaveBeenCalledWith("b");
  });
});
