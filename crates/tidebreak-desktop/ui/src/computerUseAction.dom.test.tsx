// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComputerUseAction } from "./computerUseAction";
const mocks = vi.hoisted(() => ({
  handler: null as null | ((event: { payload: ComputerUseAction }) => void),
  stop: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_name, handler) => {
    mocks.handler = handler;
    return Promise.resolve(mocks.stop);
  }),
}));
import { useComputerUseAction } from "./computerUseAction";
const action: ComputerUseAction = {
  actionId: "action-1",
  sessionId: "session-1",
  source: "browser",
  action: "click",
  phase: "running",
  executionMode: "background",
  coordinateFrame: "viewport",
  browserId: "browser-1",
  documentEpoch: 1,
  startedAtMillis: 1000,
  visibleUntilMillis: 3000,
};
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(1000);
  mocks.handler = null;
  vi.clearAllMocks();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
async function send(value: ComputerUseAction) {
  await act(async () => {
    mocks.handler?.({ payload: value });
  });
}

describe("computer-use action events", () => {
  it("expires activity without another native event", async () => {
    const { result } = renderHook(() => useComputerUseAction());
    await send(action);
    expect(result.current?.actionId).toBe("action-1");
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(result.current).toBeNull();
  });
  it("ignores wrong targets and late epochs", async () => {
    const { result, rerender } = renderHook(
      ({ epoch }) =>
        useComputerUseAction({
          source: "browser",
          browserId: "browser-1",
          documentEpoch: epoch,
        }),
      { initialProps: { epoch: 1 } },
    );
    await send({ ...action, browserId: "other" });
    expect(result.current).toBeNull();
    await send(action);
    expect(result.current).not.toBeNull();
    rerender({ epoch: 2 });
    await send(action);
    expect(result.current).toBeNull();
  });
  it("clears cancellation and does not let older activity replace a newer action", async () => {
    const { result } = renderHook(() => useComputerUseAction());
    await send({ ...action, startedAtMillis: 1200 });
    await send({ ...action, actionId: "old", startedAtMillis: 1100 });
    expect(result.current?.actionId).toBe("action-1");
    await send({ ...action, phase: "cancelled", startedAtMillis: 1200 });
    expect(result.current).toBeNull();
    await send({ ...action, actionId: "old", startedAtMillis: 1100 });
    expect(result.current).toBeNull();
    await send({ ...action, startedAtMillis: 1200 });
    expect(result.current).toBeNull();
  });
  it("keeps browser events out of the shared native and Chrome indicator", async () => {
    const { result } = renderHook(() =>
      useComputerUseAction(undefined, ["native", "chrome"]),
    );
    await send(action);
    expect(result.current).toBeNull();
    await send({ ...action, source: "chrome" });
    expect(result.current?.source).toBe("chrome");
  });
});
