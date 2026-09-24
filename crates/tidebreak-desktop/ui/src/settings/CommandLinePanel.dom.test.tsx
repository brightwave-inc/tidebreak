// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CliCommandHost } from "@/cliCommand";
import {
  CLI_STATUS,
  cliCommandFixtureHost,
} from "@/stories/cliCommandFixtures";
import { CommandLinePanel, LOCAL_BIN_PATH_LINE } from "./CommandLinePanel";

afterEach(cleanup);

function spyOn(host: CliCommandHost) {
  return {
    ...host,
    status: vi.fn(host.status),
    install: vi.fn(host.install),
    uninstall: vi.fn(host.uninstall),
  };
}

describe("CommandLinePanel", () => {
  it("installs the command for this account and reports what it did", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.notInstalled,
        after: CLI_STATUS.installed,
      }),
    );
    render(<CommandLinePanel host={host} />);

    await screen.findByText("Not installed");
    await userEvent.click(
      screen.getByRole("button", { name: "Install the tidebreak command" }),
    );

    expect(host.install).toHaveBeenCalledWith("user");
    expect(await screen.findByText("Installed")).toBeInTheDocument();
    expect(
      screen.getByText("Linked ~/.local/bin/tidebreak to this app."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Uninstall the command" }),
    ).toBeInTheDocument();
  });

  it("says how to add ~/.local/bin to the PATH when a terminal would not find it", async () => {
    render(
      <CommandLinePanel
        host={cliCommandFixtureHost({ status: CLI_STATUS.notOnPath })}
      />,
    );

    expect(
      await screen.findByText("Installed, but not on your PATH"),
    ).toBeInTheDocument();
    expect(screen.getByText(LOCAL_BIN_PATH_LINE)).toBeInTheDocument();
  });

  it("offers no install over a tidebreak it did not make", async () => {
    const host = spyOn(cliCommandFixtureHost({ status: CLI_STATUS.foreign }));
    render(<CommandLinePanel host={host} />);

    expect(
      await screen.findByText("Another tidebreak is in the way"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Install the tidebreak command" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Uninstall the command" }),
    ).toBeNull();
    expect(host.install).not.toHaveBeenCalled();
  });

  it("repairs a link to another copy of the app", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.stale,
        after: CLI_STATUS.installed,
      }),
    );
    render(<CommandLinePanel host={host} />);

    await userEvent.click(
      await screen.findByRole("button", { name: "Repair the command" }),
    );

    expect(host.install).toHaveBeenCalledWith("user");
    expect(await screen.findByText("Installed")).toBeInTheDocument();
  });

  it("uninstalls the command", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.installed,
        after: CLI_STATUS.notInstalled,
      }),
    );
    render(<CommandLinePanel host={host} />);

    await userEvent.click(
      await screen.findByRole("button", { name: "Uninstall the command" }),
    );

    expect(host.uninstall).toHaveBeenCalledWith("user");
    expect(await screen.findByText("Not installed")).toBeInTheDocument();
  });

  it("installs for all users only through the administrator button", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.notInstalled,
        after: CLI_STATUS.systemInstalled,
      }),
    );
    render(<CommandLinePanel host={host} />);
    await screen.findByText("Not installed");
    expect(host.install).not.toHaveBeenCalled();

    await userEvent.click(
      screen.getByRole("button", { name: "Install for all users…" }),
    );

    expect(host.install).toHaveBeenCalledWith("system");
    expect(await screen.findByText("Installed for all users")).toBeVisible();
  });

  it("shows why a change failed and keeps the page usable", async () => {
    render(
      <CommandLinePanel
        host={cliCommandFixtureHost({
          status: CLI_STATUS.notInstalled,
          failure:
            "The administrator prompt was cancelled, so nothing changed.",
        })}
      />,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Install for all users…" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The administrator prompt was cancelled, so nothing changed.",
    );
    expect(
      screen.getByRole("button", { name: "Install the tidebreak command" }),
    ).toBeEnabled();
  });

  it("runs the install the menu asked for once, then forgets the request", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.notInstalled,
        after: CLI_STATUS.installed,
      }),
    );
    const onAutoInstallHandled = vi.fn();
    const { rerender } = render(
      <CommandLinePanel
        host={host}
        autoInstall
        onAutoInstallHandled={onAutoInstallHandled}
      />,
    );

    expect(await screen.findByText("Installed")).toBeInTheDocument();
    rerender(
      <CommandLinePanel
        host={host}
        autoInstall
        onAutoInstallHandled={onAutoInstallHandled}
      />,
    );
    await waitFor(() => expect(host.install).toHaveBeenCalledTimes(1));
    expect(host.install).toHaveBeenCalledWith("user");
    expect(onAutoInstallHandled).toHaveBeenCalledTimes(1);
  });

  it("explains where the install cannot run", async () => {
    render(
      <CommandLinePanel
        host={cliCommandFixtureHost({
          status: CLI_STATUS.temporaryLocation,
        })}
      />,
    );
    expect(
      await screen.findByText("Move Tidebreak to Applications first"),
    ).toBeInTheDocument();
    cleanup();

    const web = spyOn(
      cliCommandFixtureHost({ status: CLI_STATUS.notInstalled, native: false }),
    );
    render(<CommandLinePanel host={web} />);
    expect(
      screen.getByText(
        "Open Tidebreak on your Mac to install the tidebreak command.",
      ),
    ).toBeInTheDocument();
    expect(web.status).not.toHaveBeenCalled();
  });
});
