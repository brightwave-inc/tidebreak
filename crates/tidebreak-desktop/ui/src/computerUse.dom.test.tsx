// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComputerUseSnapshot } from "./computerUse";

const EMPTY_SNAPSHOT: ComputerUseSnapshot = {
  active: null,
  halted: false,
  pendingConsents: [],
  pendingConfirmations: [],
};

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  handler: undefined as
    | ((event: { payload: ComputerUseSnapshot }) => void)
    | undefined,
  calls: [] as string[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: () => true,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen,
}));

import { useComputerUseState } from "./computerUse";

describe("useComputerUseState", () => {
  beforeEach(() => {
    mocks.handler = undefined;
    mocks.calls = [];
    mocks.listen.mockImplementation(
      (
        _event: string,
        handler: (event: { payload: ComputerUseSnapshot }) => void,
      ) => {
        mocks.calls.push("listen");
        mocks.handler = handler;
        return Promise.resolve(vi.fn());
      },
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("attaches the listener before querying the snapshot", async () => {
    mocks.invoke.mockImplementation(() => {
      mocks.calls.push("invoke");
      return Promise.resolve(EMPTY_SNAPSHOT);
    });
    const { unmount } = renderHook(() => useComputerUseState());

    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledOnce());
    expect(mocks.calls).toEqual(["listen", "invoke"]);
    unmount();
  });

  it("applies the snapshot query when no event arrives first", async () => {
    const initial: ComputerUseSnapshot = { ...EMPTY_SNAPSHOT, halted: true };
    mocks.invoke.mockResolvedValue(initial);
    const { result, unmount } = renderHook(() => useComputerUseState());

    await waitFor(() => expect(result.current).toEqual(initial));
    unmount();
  });

  it("keeps an event that lands while the snapshot query is in flight", async () => {
    let resolveSnapshot: (snapshot: ComputerUseSnapshot) => void = () => {};
    mocks.invoke.mockImplementation(
      () =>
        new Promise<ComputerUseSnapshot>((resolve) => {
          resolveSnapshot = resolve;
        }),
    );
    const { result, unmount } = renderHook(() => useComputerUseState());

    await waitFor(() => expect(mocks.handler).toBeDefined());
    const eventSnapshot: ComputerUseSnapshot = {
      ...EMPTY_SNAPSHOT,
      halted: true,
    };
    act(() => mocks.handler?.({ payload: eventSnapshot }));

    // The query resolves with state older than the event; it must not
    // overwrite the transition the listener already delivered.
    resolveSnapshot(EMPTY_SNAPSHOT);
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledOnce());
    expect(result.current).toEqual(eventSnapshot);
    unmount();
  });
});
