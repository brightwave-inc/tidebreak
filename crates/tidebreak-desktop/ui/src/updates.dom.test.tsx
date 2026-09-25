// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useDesktopUpdates, type DesktopUpdateState } from "./updates";

type Listener = (event: { payload: DesktopUpdateState }) => void;

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, Listener>(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: () => true,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: Listener) => {
    mocks.listeners.set(event, handler);
    return () => mocks.listeners.delete(event);
  }),
}));

const READY: DesktopUpdateState = {
  status: "ready",
  version: "0.117.0",
  error: null,
  enabled: true,
};

const REFUSAL =
  "A code session is still working on a turn. Stop the running turn, or let it finish, then restart. The update stays ready.";

afterEach(() => {
  mocks.listeners.clear();
});

describe("useDesktopUpdates", () => {
  it("keeps the desktop's reason when a restart is refused", async () => {
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_update_state") return READY;
      if (command === "restart_for_update") {
        // The desktop publishes the refusal in the update state, then
        // rejects the command with the same sentence.
        mocks.listeners.get("desktop-update-state")?.({
          payload: { ...READY, error: REFUSAL },
        });
        throw REFUSAL;
      }
      throw new Error(`unexpected ${command}`);
    });
    const { result } = renderHook(() => useDesktopUpdates());
    await waitFor(() => expect(result.current.state.status).toBe("ready"));

    await act(() => result.current.restart());

    // This used to become "Could not restart Tidebreak. Try again.", which
    // named no cause and no way forward.
    expect(result.current.state.error).toBe(REFUSAL);
    expect(result.current.state.status).toBe("ready");
  });

  it("says the restart failed when the desktop gave no reason", async () => {
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "desktop_update_state") return READY;
      throw new Error("");
    });
    const { result } = renderHook(() => useDesktopUpdates());
    await waitFor(() => expect(result.current.state.status).toBe("ready"));

    await act(() => result.current.restart());

    expect(result.current.state.error).toBe(
      "Could not restart Tidebreak. Try again.",
    );
  });
});
