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
    };
    vi.clearAllMocks();
  });

  it("names the app under control and stops it from the banner", async () => {
    mocks.snapshot.active = {
      bundleId: "com.apple.Notes",
      appName: "Notes",
      lastActivityMillis: Date.now(),
      visibleUntilMillis: Date.now() + 30_000,
    };
    render(<ComputerUseIndicator />);

    expect(screen.getByText(/Tidebreak is controlling Notes/)).toBeTruthy();
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
      screen.getByText("Tidebreak is controlling Google Chrome"),
    ).toBeTruthy();
    expect(screen.getByText(/Working in the background/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(mocks.stop).toHaveBeenCalledOnce();
  });
});
