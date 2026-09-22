// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "./computerUsePermissions";
import { computerUseSetupAsked } from "./computerUseSetupPrompt";
import { ComputerUseSetupDialog } from "./ComputerUseSetupDialog";

const missing: ComputerUsePermissionStatus = {
  status: "available",
  appName: "Tidebreak",
  appIdentifier: "io.brightwave.tidebreak",
  accessibility: false,
  screenRecording: false,
};
const granted: ComputerUsePermissionStatus = {
  ...missing,
  accessibility: true,
  screenRecording: true,
};

function host(overrides: Partial<ComputerUsePermissionHost> = {}) {
  return {
    availability: () => "local" as const,
    status: vi.fn().mockResolvedValue(missing),
    request: vi.fn().mockResolvedValue(granted),
    openSettings: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

/** What AppShell does on every exit: record the ask and close the dialog. */
function renderDialog(permissionHost: ComputerUsePermissionHost) {
  const onDone = vi.fn();
  render(<ComputerUseSetupDialog open host={permissionHost} onDone={onDone} />);
  return onDone;
}

beforeEach(() => {
  window.localStorage.clear();
});
afterEach(cleanup);

describe("ComputerUseSetupDialog", () => {
  it("requests both grants and settles on Done", async () => {
    const permissionHost = host();
    const onDone = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));

    await waitFor(() =>
      expect(permissionHost.request).toHaveBeenCalledTimes(1),
    );
    // Granted rows settle to a single confirmation and one way out.
    await screen.findByRole("button", { name: "Done" });
    expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
    expect(screen.getAllByText("Allowed")).toHaveLength(2);
    expect(onDone).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("opens with the grant button focused, not the decline", async () => {
    renderDialog(host());

    // Enter on an untouched dialog should grant, and the decline must not be
    // the control a keyboard lands on.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Allow" })).toHaveFocus(),
    );
  });

  it("finishes on Not now without requesting anything", async () => {
    const permissionHost = host();
    const onDone = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Not now" }));

    expect(onDone).toHaveBeenCalledTimes(1);
    expect(permissionHost.request).not.toHaveBeenCalled();
  });

  it("finishes on Escape, so closing the dialog still counts as the ask", async () => {
    const onDone = renderDialog(host());
    await screen.findAllByText("Not allowed");

    await userEvent.keyboard("{Escape}");

    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
  });

  it("keeps the ask open when the request fails", async () => {
    const permissionHost = host({
      request: vi.fn().mockRejectedValue(new Error("helper unavailable")),
    });
    const onDone = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));

    // A helper hiccup must not spend the one ask this install gets.
    const alert = await screen.findByRole("alert");
    // The dialog has no Refresh button, so its copy must not send anyone to
    // one; it re-reads on window focus instead.
    expect(alert).not.toHaveTextContent("refresh");
    expect(onDone).not.toHaveBeenCalled();
    expect(computerUseSetupAsked()).toBe(false);
    expect(screen.getByRole("button", { name: "Allow" })).toBeEnabled();
  });
});
