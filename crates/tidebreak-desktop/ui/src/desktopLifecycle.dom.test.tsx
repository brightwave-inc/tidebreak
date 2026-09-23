// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { QuitPromptUpdate } from "./desktopLifecycle";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  emit: undefined as ((event: { payload: unknown }) => void) | undefined,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  isTauri: () => true,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen,
}));

import { useQuitPrompt, useUncleanExitNotice } from "./desktopLifecycle";

const IDLE: QuitPromptUpdate = {
  request: 0,
  prompt: { phase: "idle" },
  error: null,
};

describe("useQuitPrompt", () => {
  beforeEach(() => {
    mocks.emit = undefined;
    mocks.listen.mockImplementation(
      (_event: string, handler: (event: { payload: unknown }) => void) => {
        mocks.emit = handler;
        return Promise.resolve(vi.fn());
      },
    );
    mocks.invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "quit_prompt_state" ? IDLE : undefined),
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  /** The shell falls back to a native dialog unless the renderer says it
   * is showing the prompt, so every prompt is acknowledged by number. */
  it("acknowledges each prompt the shell raises", async () => {
    const { result, unmount } = renderHook(() => useQuitPrompt());
    await waitFor(() => expect(mocks.emit).toBeDefined());

    act(() =>
      mocks.emit?.({
        payload: {
          request: 3,
          prompt: { phase: "asking", agents: 2, waitingForYou: 1 },
          error: null,
        },
      }),
    );

    expect(result.current.update.prompt).toEqual({
      phase: "asking",
      agents: 2,
      waitingForYou: 1,
    });
    expect(mocks.invoke).toHaveBeenCalledWith("quit_prompt_opened", {
      request: 3,
    });
    unmount();
  });

  it("keeps the newest prompt when an older one lands late", async () => {
    const { result, unmount } = renderHook(() => useQuitPrompt());
    await waitFor(() => expect(mocks.emit).toBeDefined());

    act(() =>
      mocks.emit?.({
        payload: {
          request: 5,
          prompt: { phase: "waiting", agents: 1, waitingForYou: 0 },
          error: null,
        },
      }),
    );
    act(() =>
      mocks.emit?.({
        payload: {
          request: 4,
          prompt: { phase: "asking", agents: 2, waitingForYou: 1 },
          error: null,
        },
      }),
    );
    expect(result.current.update.prompt).toEqual({
      phase: "waiting",
      agents: 1,
      waitingForYou: 0,
    });
    unmount();
  });

  it("ignores a payload it cannot read", async () => {
    const { result, unmount } = renderHook(() => useQuitPrompt());
    await waitFor(() => expect(mocks.emit).toBeDefined());

    act(() =>
      mocks.emit?.({ payload: { request: 9, prompt: { phase: "gone" } } }),
    );
    // A count without the agents waiting on an answer is not read either.
    act(() =>
      mocks.emit?.({
        payload: {
          request: 10,
          prompt: { phase: "asking", agents: 2 },
          error: null,
        },
      }),
    );
    expect(result.current.update.prompt).toEqual({ phase: "idle" });
    unmount();
  });

  it("sends the answer and holds the buttons until the shell replies", async () => {
    const { result, unmount } = renderHook(() => useQuitPrompt());
    await waitFor(() => expect(mocks.emit).toBeDefined());

    act(() => result.current.answer("safe_point"));
    expect(result.current.answering).toBe(true);
    expect(mocks.invoke).toHaveBeenCalledWith("answer_quit_prompt", {
      choice: "safe_point",
    });

    act(() =>
      mocks.emit?.({
        payload: {
          request: 2,
          prompt: { phase: "waiting", agents: 2, waitingForYou: 0 },
          error: null,
        },
      }),
    );
    expect(result.current.answering).toBe(false);
    unmount();
  });
});

describe("useUncleanExitNotice", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("shows the notice after an unclean exit and saves the report on request", async () => {
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "unclean_exit_notice") {
        return Promise.resolve({ startedAt: null, version: "0.114.0" });
      }
      if (command === "save_diagnostics_report") return Promise.resolve(true);
      return Promise.resolve(undefined);
    });
    const { result, unmount } = renderHook(() => useUncleanExitNotice());
    await waitFor(() => expect(result.current.notice).not.toBeNull());

    act(() => result.current.saveReport());
    expect(result.current.save).toEqual({ status: "saving" });
    await waitFor(() =>
      expect(result.current.save).toEqual({ status: "saved" }),
    );

    act(() => result.current.dismiss());
    expect(result.current.notice).toBeNull();
    expect(mocks.invoke).toHaveBeenCalledWith("dismiss_unclean_exit_notice");
    unmount();
  });

  it("returns to rest when the person cancels the save dialog", async () => {
    mocks.invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "save_diagnostics_report" ? false : null),
    );
    const { result, unmount } = renderHook(() => useUncleanExitNotice());
    act(() => result.current.saveReport());
    await waitFor(() =>
      expect(result.current.save).toEqual({ status: "idle" }),
    );
    unmount();
  });

  it("keeps the reason a save failed", async () => {
    mocks.invoke.mockImplementation((command: string) =>
      command === "save_diagnostics_report"
        ? Promise.reject("Could not build the diagnostics report")
        : Promise.resolve(null),
    );
    const { result, unmount } = renderHook(() => useUncleanExitNotice());
    act(() => result.current.saveReport());
    await waitFor(() =>
      expect(result.current.save).toEqual({
        status: "failed",
        error: "Could not build the diagnostics report",
      }),
    );
    unmount();
  });
});
