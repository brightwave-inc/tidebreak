// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: {
    active: null as null | {
      bundleId: string;
      appName: string | null;
      lastActivityMillis: number;
      visibleUntilMillis: number;
    },
    halted: false,
    stoppedSessions: 0,
  },
  stop: vi.fn(() => Promise.resolve()),
  resume: vi.fn(() => Promise.resolve()),
}));

vi.mock("./computerUse", () => ({
  useComputerUseState: () => mocks.snapshot,
  stopComputerUseControl: mocks.stop,
  resumeComputerUseControl: mocks.resume,
}));

vi.mock("./computerUseAction", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./computerUseAction")>()),
  useComputerUseAction: () => null,
}));

import {
  ComputerUseIndicator,
  ComputerUseIndicatorView,
} from "./ComputerUseIndicator";

afterEach(cleanup);

describe("ComputerUseIndicator", () => {
  beforeEach(() => {
    mocks.snapshot = {
      active: null,
      halted: false,
      stoppedSessions: 0,
    };
    vi.clearAllMocks();
  });

  it("names the app and stops it from the banner", async () => {
    mocks.snapshot.active = {
      bundleId: "com.apple.Notes",
      appName: "Notes",
      lastActivityMillis: Date.now(),
      visibleUntilMillis: Date.now() + 30_000,
    };
    render(<ComputerUseIndicator />);

    expect(screen.getByText(/Computer use: Notes/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(mocks.stop).toHaveBeenCalledOnce();
  });

  it("hides the banner once control has been idle past its re-arm window", () => {
    mocks.snapshot.active = {
      bundleId: "com.apple.Notes",
      appName: "Notes",
      lastActivityMillis: Date.now() - 60_000,
      visibleUntilMillis: Date.now() - 1_000,
    };
    const { container } = render(<ComputerUseIndicator />);
    expect(container.firstChild).toBeNull();
  });

  it("offers resume while halted", async () => {
    mocks.snapshot.halted = true;
    render(<ComputerUseIndicator />);

    await userEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(mocks.resume).toHaveBeenCalledOnce();
  });
  it("names Chrome and uses the shared Stop control", async () => {
    render(
      <ComputerUseIndicatorView
        snapshot={{ active: null, halted: false }}
        onStop={mocks.stop}
        onResume={mocks.resume}
        action={{
          actionId: "chrome-1",
          sessionId: "session-1",
          source: "chrome",
          action: "click",
          phase: "running",
          executionMode: "background",
          coordinateFrame: "viewport",
          startedAtMillis: Date.now(),
          visibleUntilMillis: Date.now() + 10000,
        }}
      />,
    );
    expect(
      screen.getByText("Computer use request for Google Chrome"),
    ).toBeTruthy();
    expect(screen.getByText(/Background request/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(mocks.stop).toHaveBeenCalledOnce();
  });
});

describe("individually stopped sessions", () => {
  it("offers Resume for an individually stopped session without claiming a global stop", async () => {
    const resume = vi.fn(async () => {});
    render(
      <ComputerUseIndicatorView
        snapshot={{ active: null, halted: false, stoppedSessions: 1 }}
        onStop={mocks.stop}
        onResume={resume}
      />,
    );
    expect(
      screen.getByText("Computer control is stopped for 1 session"),
    ).toBeTruthy();
    expect(screen.queryByText("Computer control is stopped")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(resume).toHaveBeenCalledOnce();
  });

  it("keeps Stop available for another active session and offers a separate Resume", async () => {
    const resume = vi.fn(async () => {});
    const stop = vi.fn(async () => {});
    render(
      <ComputerUseIndicatorView
        snapshot={{
          active: {
            bundleId: "test.editor",
            appName: "Editor",
            lastActivityMillis: Date.now(),
            visibleUntilMillis: Date.now() + 30_000,
          },
          halted: false,
          stoppedSessions: 2,
        }}
        onStop={stop}
        onResume={resume}
      />,
    );
    expect(screen.getByText("Computer use: Editor")).toBeTruthy();
    expect(
      screen.getByText("2 sessions have stopped computer control."),
    ).toBeTruthy();
    expect(screen.queryByText("Computer control is stopped")).toBeNull();
    await userEvent.click(
      screen.getByRole("button", { name: "Resume stopped sessions" }),
    );
    expect(resume).toHaveBeenCalledOnce();
    expect(stop).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(stop).toHaveBeenCalledOnce();
  });
});

it("keeps emergency Stop available while Resume waits for native approval", async () => {
  const resume = vi.fn(() => new Promise<void>(() => {}));
  const stop = vi.fn(async () => {});
  render(
    <ComputerUseIndicatorView
      snapshot={{
        active: {
          bundleId: "test.editor",
          appName: "Editor",
          lastActivityMillis: Date.now(),
          visibleUntilMillis: Date.now() + 30_000,
        },
        halted: false,
        stoppedSessions: 1,
      }}
      onStop={stop}
      onResume={resume}
    />,
  );
  await userEvent.click(
    screen.getByRole("button", { name: "Resume stopped sessions" }),
  );
  expect(resume).toHaveBeenCalledOnce();
  const stopButton = screen.getByRole("button", {
    name: "Stop",
  }) as HTMLButtonElement;
  expect(stopButton.disabled).toBe(false);
  await userEvent.click(stopButton);
  expect(stop).toHaveBeenCalledOnce();
});
