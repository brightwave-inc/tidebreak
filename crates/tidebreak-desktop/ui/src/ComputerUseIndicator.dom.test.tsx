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
    pendingConsents: [] as Array<{
      callId: string;
      chatId: string;
      bundleId: string;
      appName: string | null;
      capability: "capture_screen" | "read_app_content" | "control_app";
    }>,
    pendingConfirmations: [] as Array<{
      callId: string;
      chatId: string;
      bundleId: string;
      appName: string | null;
      targetLabel: string | null;
      reason: string;
    }>,
  },
  stop: vi.fn(),
  resume: vi.fn(),
  consent: vi.fn(),
  confirmation: vi.fn(),
}));

vi.mock("./computerUse", () => ({
  useComputerUseState: () => mocks.snapshot,
  stopComputerUseControl: mocks.stop,
  resumeComputerUseControl: mocks.resume,
  resolveComputerUseConsent: mocks.consent,
  resolveComputerUseConfirmation: mocks.confirmation,
}));

import { ComputerUseIndicator } from "./ComputerUseIndicator";

afterEach(cleanup);

describe("ComputerUseIndicator", () => {
  beforeEach(() => {
    mocks.snapshot = {
      active: null,
      halted: false,
      pendingConsents: [],
      pendingConfirmations: [],
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

    expect(screen.getByText(/OpenWave is controlling Notes/)).toBeTruthy();
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

  it("commits a per-app consent decision for the parked call", async () => {
    mocks.snapshot.pendingConsents = [
      {
        callId: "call-1",
        chatId: "chat-1",
        bundleId: "com.apple.mail",
        appName: "Mail",
        capability: "control_app",
      },
    ];
    render(<ComputerUseIndicator />);

    expect(screen.getByText(/Allow OpenWave to control Mail/)).toBeTruthy();
    await userEvent.click(
      screen.getByRole("button", { name: "Always for this chat" }),
    );
    expect(mocks.consent).toHaveBeenCalledWith("call-1", "chat");
    await userEvent.click(screen.getByRole("button", { name: "Decline" }));
    expect(mocks.consent).toHaveBeenCalledWith("call-1", "decline");
  });

  it("confirms a broker-held consequential action", async () => {
    mocks.snapshot.pendingConfirmations = [
      {
        callId: "call-9",
        chatId: "chat-1",
        bundleId: "com.apple.mail",
        appName: "Mail",
        targetLabel: "Send",
        reason: "send a message",
      },
    ];
    render(<ComputerUseIndicator />);

    expect(screen.getByText(/“Send”/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(mocks.confirmation).toHaveBeenCalledWith("call-9", true);
  });
});
