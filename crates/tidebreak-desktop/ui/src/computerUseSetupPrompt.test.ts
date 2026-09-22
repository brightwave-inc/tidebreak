// @vitest-environment jsdom
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "./computerUsePermissions";
import {
  computerUseSetupAsked,
  markComputerUseSetupAsked,
  shouldPromptComputerUseSetup,
  useComputerUseSetupPrompt,
} from "./computerUseSetupPrompt";

const missing: ComputerUsePermissionStatus = {
  status: "available",
  appName: "Tidebreak",
  appIdentifier: "io.brightwave.tidebreak",
  screenRecording: false,
  accessibility: false,
};

describe("shouldPromptComputerUseSetup", () => {
  it("asks when a grant is missing on this Mac", () => {
    expect(shouldPromptComputerUseSetup("local", missing)).toBe(true);
    expect(
      shouldPromptComputerUseSetup("local", {
        ...missing,
        accessibility: true,
      }),
    ).toBe(true);
  });

  it("stays quiet once both grants are held", () => {
    expect(
      shouldPromptComputerUseSetup("local", {
        ...missing,
        accessibility: true,
        screenRecording: true,
      }),
    ).toBe(false);
  });

  it("stays quiet where the grants cannot apply", () => {
    // A browser tab and a window attached to another machine both offer a
    // button that would refuse, so neither may raise the dialog.
    expect(shouldPromptComputerUseSetup("web", missing)).toBe(false);
    expect(shouldPromptComputerUseSetup("remote", missing)).toBe(false);
    // Windows and Linux desktop builds answer `unsupported`.
    expect(
      shouldPromptComputerUseSetup("local", { status: "unsupported" }),
    ).toBe(false);
  });
});

describe("computerUseSetupAsked", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("is false until the ask is recorded", () => {
    expect(computerUseSetupAsked()).toBe(false);
    markComputerUseSetupAsked();
    expect(computerUseSetupAsked()).toBe(true);
  });
});

describe("useComputerUseSetupPrompt", () => {
  function host(overrides: Partial<ComputerUsePermissionHost> = {}) {
    return {
      availability: () => "local" as const,
      status: vi.fn().mockResolvedValue(missing),
      request: vi.fn(),
      openSettings: vi.fn(),
      ...overrides,
    };
  }

  beforeEach(() => {
    window.localStorage.clear();
  });
  afterEach(cleanup);

  it("asks on the first launch that finds a grant missing", async () => {
    const { result } = renderHook(() => useComputerUseSetupPrompt(host()));

    await waitFor(() => expect(result.current).toBe(true));
    // Opening the dialog is not the ask being spent; the dialog records that.
    expect(computerUseSetupAsked()).toBe(false);
  });

  it("records the ask without a dialog when both grants are held", async () => {
    const granted = { ...missing, accessibility: true, screenRecording: true };
    const { result } = renderHook(() =>
      useComputerUseSetupPrompt(
        host({ status: vi.fn().mockResolvedValue(granted) }),
      ),
    );

    await waitFor(() => expect(computerUseSetupAsked()).toBe(true));
    expect(result.current).toBe(false);
  });

  it("reads nothing once the ask is recorded", () => {
    markComputerUseSetupAsked();
    const permissionHost = host();

    const { result } = renderHook(() =>
      useComputerUseSetupPrompt(permissionHost),
    );

    // The read costs a helper process, so a settled install must not pay it.
    expect(permissionHost.status).not.toHaveBeenCalled();
    expect(result.current).toBe(false);
  });

  it("stays quiet when the helper cannot answer", async () => {
    const permissionHost = host({
      status: vi.fn().mockRejectedValue(new Error("helper unavailable")),
    });

    const { result } = renderHook(() =>
      useComputerUseSetupPrompt(permissionHost),
    );

    await waitFor(() => expect(permissionHost.status).toHaveBeenCalled());
    // A dialog whose buttons would fail too is worse than no dialog, and the
    // ask stays unspent so the next launch can try again.
    expect(result.current).toBe(false);
    expect(computerUseSetupAsked()).toBe(false);
  });
});
