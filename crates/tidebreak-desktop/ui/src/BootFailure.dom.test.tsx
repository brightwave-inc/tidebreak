// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BootFailure,
  bootDebugReport,
  bootFailureCopy,
  displayDataDir,
  LATEST_RELEASE_URL,
  type BootAttachment,
} from "./BootFailure";
import type { BootFailureKind, LocalBootFailure } from "./bootRecovery";

vi.mock("./host", () => ({
  hasMacOverlayTitlebar: () => false,
  hasNativeHost: () => true,
}));

const openInBrowser = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("./openInBrowser", () => ({ openInBrowser }));

const DATA_DIR =
  "/Users/example/Library/Application Support/io.brightwave.tidebreak";

function localFailure(
  kind: BootFailureKind,
  over: Partial<LocalBootFailure> = {},
): LocalBootFailure {
  return { kind, stopped: false, dataDir: DATA_DIR, ...over };
}

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

function renderFailure(
  overrides: Partial<Parameters<typeof BootFailure>[0]> = {},
) {
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
  it("names the machine it could not reach and words the error for the reader", () => {
    renderFailure();

    const screenRoot = screen.getByRole("alert");
    expect(screenRoot).toHaveTextContent(
      "Could not reach https://tidebreak.example.com.",
    );
    expect(screenRoot).toHaveTextContent(
      "Tidebreak could not reach its server.",
    );
    // The raw error belongs to the copied report, not the screen.
    expect(screenRoot).not.toHaveTextContent("TypeError");
    expect(screenRoot).not.toHaveTextContent("Load failed");
  });

  it("offers a way back to this computer only when attached to another one", async () => {
    const user = userEvent.setup();
    const { onWorkLocally } = renderFailure();

    await user.click(
      screen.getByRole("button", { name: /Work on this computer/ }),
    );
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
      "Tidebreak could not connect to its server.",
    );
    await user.click(screen.getByRole("button", { name: /Try again/ }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("runs the boot again from Try again, and waits for it", async () => {
    const user = userEvent.setup();
    let finish = () => {};
    const onRetry = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    renderFailure({
      attachment: local,
      stage: "connect",
      local: localFailure("instance_lock"),
      onRetry,
    });

    await user.click(screen.getByRole("button", { name: /Try again/ }));
    expect(onRetry).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: /Try again/ })).toBeDisabled();
    finish();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Try again/ })).toBeEnabled(),
    );
  });

  it("names a known failure and the action it needs", () => {
    renderFailure({
      attachment: local,
      stage: "connect",
      error: new Error(
        "configuration error: another Tidebreak process is already running on the data directory",
      ),
      local: localFailure("instance_lock"),
    });
    const screenRoot = screen.getByRole("alert");
    expect(screenRoot).toHaveTextContent(
      "Another Tidebreak is using your data.",
    );
    expect(screenRoot).toHaveTextContent("Quit it, then try again.");
    // The CLI's advice is for a terminal, not for this screen.
    expect(screenRoot).not.toHaveTextContent("--attach");
    expect(screenRoot).not.toHaveTextContent("TIDEBREAK_DATA_DIR");
  });

  it("says the conversations are still on disk, and where", async () => {
    const user = userEvent.setup();
    const onRevealDataDir = vi.fn();
    renderFailure({
      attachment: local,
      stage: "connect",
      local: localFailure("disk_full"),
      onRevealDataDir,
    });
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Your conversations are still on disk at ~/Library/Application Support/io.brightwave.tidebreak.",
    );
    await user.click(screen.getByRole("button", { name: /Show/ }));
    expect(onRevealDataDir).toHaveBeenCalledOnce();

    // Another machine's data is not on this computer.
    cleanup();
    renderFailure({ local: localFailure("disk_full") });
    expect(screen.getByRole("alert")).not.toHaveTextContent("still on disk");
  });

  it("offers a restart, not another boot, when the server stopped after starting", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const onRestart = vi.fn(async () => {});
    renderFailure({
      attachment: local,
      stage: "connect",
      local: localFailure("unknown", { stopped: true }),
      onRetry,
      onRestart,
    });
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Tidebreak's server stopped.",
    );
    expect(
      screen.queryByRole("button", { name: /Try again/ }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Restart Tidebreak/ }));
    expect(onRestart).toHaveBeenCalledOnce();
    expect(onRetry).not.toHaveBeenCalled();
  });

  it("sends data from a newer version to the latest release", async () => {
    const user = userEvent.setup();
    renderFailure({
      attachment: local,
      stage: "connect",
      local: localFailure("newer_version"),
    });
    await user.click(
      screen.getByRole("button", { name: /Get the latest version/ }),
    );
    expect(openInBrowser).toHaveBeenCalledWith(LATEST_RELEASE_URL);
  });

  it("offers Report a problem", async () => {
    const user = userEvent.setup();
    const onReportProblem = vi.fn();
    renderFailure({ onReportProblem });
    await user.click(screen.getByRole("button", { name: "Report a problem…" }));
    expect(onReportProblem).toHaveBeenCalledOnce();
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
    expect(
      await screen.findByRole("button", { name: "Copied" }),
    ).toBeInTheDocument();
  });
});

describe("bootFailureCopy", () => {
  it("gives every known local failure its own sentence and action", () => {
    const kinds: BootFailureKind[] = [
      "instance_lock",
      "newer_version",
      "unrecognized_data",
      "unsupported_version",
      "migration",
      "disk_full",
      "keychain",
    ];
    const headlines = new Set<string>();
    for (const kind of kinds) {
      const copy = bootFailureCopy({
        stage: "connect",
        error: new Error("store error: raw host text"),
        attachment: local,
        local: localFailure(kind),
      });
      headlines.add(copy.headline);
      // The host's log fragment stays out of the sentence a person reads.
      expect(copy.body, kind).not.toContain("store error");
    }
    expect(headlines.size).toBe(kinds.length);
    for (const kind of [
      "newer_version",
      "unrecognized_data",
      "unsupported_version",
    ] as const) {
      expect(
        bootFailureCopy({
          stage: "connect",
          error: null,
          attachment: local,
          local: localFailure(kind),
        }).primary,
      ).toBe("latest");
    }
  });

  it("keeps the host's words for a failure it cannot name", () => {
    const copy = bootFailureCopy({
      stage: "connect",
      error: "server error: the listener could not bind",
      attachment: local,
      local: localFailure("unknown"),
    });
    expect(copy.headline).toBe("Tidebreak could not start.");
    // Without the error kind the host puts in front of its message.
    expect(copy.body).toBe("The listener could not bind");
  });

  it("writes the home folder as ~", () => {
    expect(displayDataDir(DATA_DIR)).toBe(
      "~/Library/Application Support/io.brightwave.tidebreak",
    );
    expect(displayDataDir("/home/example/.tidebreak")).toBe("~/.tidebreak");
    expect(displayDataDir("/var/lib/tidebreak")).toBe("/var/lib/tidebreak");
  });
});

describe("bootDebugReport", () => {
  it("scrubs a credential out of the error it copies", () => {
    const token = ["tidebreak", "-token.", "k3Jd8sLq".repeat(4)].join("");
    const copied = bootDebugReport({
      stage: "connect",
      error: new Error(`refused Authorization: Bearer ${token}`),
      attachment: remote,
      appVersion: "0.58.0",
      capturedAt: "2026-08-21T12:49:00.000Z",
      userAgent: null,
    });
    expect(copied).not.toContain(token);
    expect(JSON.parse(copied).error.message).toContain("refused");
  });

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
    expect(JSON.parse(report({ error: "plain string failure" })).error).toEqual(
      {
        name: null,
        message: "plain string failure",
      },
    );
  });

  it("says nothing about an attachment it could not read", () => {
    const parsed = JSON.parse(report({ attachment: null }));
    expect(parsed.attachment).toBeNull();
    expect(parsed.remoteBaseUrl).toBeNull();
  });
});
