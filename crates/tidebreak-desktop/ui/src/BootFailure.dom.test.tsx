// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BootFailure, bootDebugReport, type BootAttachment } from "./BootFailure";

vi.mock("./host", () => ({
  hasMacOverlayTitlebar: () => false,
}));

const remote: BootAttachment = {
  attachment: "remote",
  baseUrl: "https://tidebreak.example.com",
  gatewayAuth: true,
};

const local: BootAttachment = {
  attachment: "local",
  baseUrl: null,
  gatewayAuth: false,
};

function renderFailure(overrides: Partial<Parameters<typeof BootFailure>[0]> = {}) {
  const props = {
    stage: "catalog" as const,
    error: new TypeError("Load failed"),
    attachment: remote,
    appVersion: "0.58.0",
    onRetry: vi.fn(),
    onWorkLocally: vi.fn(async () => {}),
    writeClipboard: vi.fn(async () => {}),
    ...overrides,
  };
  render(<BootFailure {...props} />);
  return props;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("BootFailure", () => {
  it("names the machine it could not reach and keeps the raw error", () => {
    renderFailure();

    const screenRoot = screen.getByRole("alert");
    expect(screenRoot).toHaveTextContent(
      "Could not reach https://tidebreak.example.com.",
    );
    // The raw string still matters — it is what a reader pastes into a bug
    // report — it just no longer stands alone as the entire screen.
    expect(screenRoot).toHaveTextContent("TypeError: Load failed");
  });

  it("offers a way back to this computer only when attached to another one", async () => {
    const user = userEvent.setup();
    const { onWorkLocally } = renderFailure();

    await user.click(screen.getByRole("button", { name: /Work on this computer/ }));
    await waitFor(() => expect(onWorkLocally).toHaveBeenCalledOnce());

    cleanup();
    renderFailure({ attachment: local });
    expect(
      screen.queryByRole("button", { name: /Work on this computer/ }),
    ).not.toBeInTheDocument();
  });

  it("always offers a retry", async () => {
    const user = userEvent.setup();
    const { onRetry } = renderFailure({ attachment: local, stage: "connect" });

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Tidebreak could not start its server.",
    );
    await user.click(screen.getByRole("button", { name: /Try again/ }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("copies the debug report and says so", async () => {
    const user = userEvent.setup();
    const writeClipboard = vi.fn(async (_text: string) => {});
    renderFailure({ writeClipboard });

    await user.click(screen.getByRole("button", { name: "Copy debug info" }));

    await waitFor(() => expect(writeClipboard).toHaveBeenCalledOnce());
    const copied = JSON.parse(writeClipboard.mock.calls[0][0]);
    expect(copied.remoteBaseUrl).toBe("https://tidebreak.example.com");
    expect(copied.stage).toBe("catalog");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });
});

describe("bootDebugReport", () => {
  const report = (over: Record<string, unknown> = {}) =>
    bootDebugReport({
      stage: "catalog",
      error: new TypeError("Load failed"),
      attachment: remote,
      appVersion: "0.58.0",
      capturedAt: "2026-08-21T12:49:00.000Z",
      userAgent: "Test/1.0",
      ...over,
    });

  it("carries what a reader would be asked for", () => {
    expect(JSON.parse(report())).toEqual({
      capturedAt: "2026-08-21T12:49:00.000Z",
      appVersion: "0.58.0",
      stage: "catalog",
      attachment: "remote",
      remoteBaseUrl: "https://tidebreak.example.com",
      gatewayAuth: true,
      error: { name: "TypeError", message: "Load failed" },
      userAgent: "Test/1.0",
    });
  });

  /**
   * The point of the control is that the payload is safe to paste in public.
   * The bearer this window would have used is one careless spread away from
   * the clipboard, so the shape is asserted rather than trusted.
   */
  it("carries no credential, whatever the failure was carrying", () => {
    const error = new Error("refused") as Error & { token?: string };
    error.token = "super-secret-bearer";
    const serialized = report({ error });

    expect(serialized).not.toContain("super-secret-bearer");
    expect(serialized.toLowerCase()).not.toContain("authorization");
    expect(JSON.parse(serialized).error).toEqual({
      name: "Error",
      message: "refused",
    });
  });

  it("reports a thrown non-error without losing it", () => {
    expect(JSON.parse(report({ error: "plain string failure" })).error).toEqual({
      name: null,
      message: "plain string failure",
    });
  });

  it("says nothing about an attachment it could not read", () => {
    const parsed = JSON.parse(report({ attachment: null }));
    expect(parsed.attachment).toBeNull();
    expect(parsed.remoteBaseUrl).toBeNull();
  });
});
