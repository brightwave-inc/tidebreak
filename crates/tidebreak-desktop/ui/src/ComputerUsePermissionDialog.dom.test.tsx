// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ComputerUsePermissionDialog } from "./ComputerUsePermissionDialog";
import {
  type PermissionAsk,
  useComputerUsePermissionAsk,
} from "./computerUsePermissionAsk";
import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "./computerUsePermissions";

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
    restart: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

const forTask: PermissionAsk = { subject: "apps", forTask: true };

function renderDialog(
  permissionHost: ComputerUsePermissionHost,
  ask: PermissionAsk = forTask,
) {
  const onClose = vi.fn();
  const onOpenSettings = vi.fn();
  render(
    <ComputerUsePermissionDialog
      ask={ask}
      host={permissionHost}
      onClose={onClose}
      onOpenSettings={onOpenSettings}
    />,
  );
  return { onClose, onOpenSettings };
}

beforeEach(() => {
  window.localStorage.clear();
  useComputerUsePermissionAsk.setState({
    ask: null,
    needs: [],
    screenRecordingRequested: false,
  });
});
afterEach(cleanup);

describe("ComputerUsePermissionDialog", () => {
  it("opens on Not now, so Enter never raises the macOS prompts", async () => {
    const permissionHost = host();
    renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Not now" })).toHaveFocus(),
    );
    await userEvent.keyboard("{Enter}");

    expect(permissionHost.request).not.toHaveBeenCalled();
  });

  it("says in one sentence what each permission enables", async () => {
    renderDialog(host());

    expect(
      await screen.findByText(
        "Lets Tidebreak read other apps' controls, click, and type.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Lets Tidebreak take screenshots of apps and displays."),
    ).toBeInTheDocument();
  });

  it("names only the permission still missing", async () => {
    renderDialog(
      host({
        status: vi
          .fn()
          .mockResolvedValue({ ...missing, screenRecording: true }),
      }),
    );

    expect(
      await screen.findByText(
        "A task needs the Accessibility permission to use your other apps.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/two macOS permissions/)).toBeNull();
  });

  it("words browser control for the browser window", async () => {
    renderDialog(host(), { subject: "browser", forTask: true });

    expect(
      await screen.findByRole("heading", {
        name: "Allow Tidebreak to control your browser",
      }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(
        "Lets Tidebreak take screenshots of your browser window.",
      ),
    ).toBeInTheDocument();
  });

  it("requests both permissions on Allow and settles on Done", async () => {
    const permissionHost = host();
    const { onClose } = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));

    await waitFor(() =>
      expect(permissionHost.request).toHaveBeenCalledTimes(1),
    );
    const done = await screen.findByRole("button", { name: "Done" });
    expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
    await waitFor(() => expect(done).toHaveFocus());
    await userEvent.click(done);
    expect(onClose).toHaveBeenCalledWith("ready");
  });

  it("closes on Not now or Escape without requesting anything", async () => {
    const permissionHost = host();
    const { onClose } = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    await userEvent.keyboard("{Escape}");

    expect(onClose).toHaveBeenCalledWith("not_now");
    expect(onClose).not.toHaveBeenCalledWith("ready");
    expect(permissionHost.request).not.toHaveBeenCalled();
  });

  it("points to Settings → Permissions as the way back", async () => {
    const { onOpenSettings } = renderDialog(host());

    await userEvent.click(
      await screen.findByRole("button", { name: "Settings → Permissions" }),
    );

    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });

  it("offers to restart once Screen Recording was requested and is still off", async () => {
    const permissionHost = host({
      // macOS showed its prompt, but the grant reaches only a new process.
      request: vi.fn().mockResolvedValue({ ...missing, accessibility: true }),
    });
    renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");
    expect(
      screen.queryByRole("button", { name: "Restart Tidebreak" }),
    ).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Restart Tidebreak" }),
    );

    expect(permissionHost.restart).toHaveBeenCalledTimes(1);
  });

  it("does not offer a restart when Screen Recording is already allowed", async () => {
    useComputerUsePermissionAsk.setState({ screenRecordingRequested: true });
    renderDialog(
      host({
        status: vi
          .fn()
          .mockResolvedValue({ ...missing, screenRecording: true }),
      }),
    );

    await screen.findByText("Not allowed");
    expect(
      screen.queryByRole("button", { name: "Restart Tidebreak" }),
    ).toBeNull();
  });

  it("keeps the ask open when the request fails", async () => {
    const permissionHost = host({
      request: vi.fn().mockRejectedValue(new Error("helper unavailable")),
    });
    const { onClose } = renderDialog(permissionHost);
    await screen.findAllByText("Not allowed");

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));

    const alert = await screen.findByRole("alert");
    // The dialog has no Refresh button, so its copy must not send anyone to
    // one; it re-reads on window focus instead.
    expect(alert).not.toHaveTextContent("refresh");
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Allow" })).toBeEnabled();
  });
});
