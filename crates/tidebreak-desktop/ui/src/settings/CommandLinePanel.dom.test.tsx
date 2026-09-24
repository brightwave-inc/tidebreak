// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CliCommandHost, CliCommandStatus } from "@/cliCommand";
import {
  CLI_STATUS,
  cliCommandFixtureHost,
} from "@/stories/cliCommandFixtures";
import {
  CommandLinePanel,
  commandLineVerdict,
  LOCAL_BIN_PATH_LINE,
} from "./CommandLinePanel";

afterEach(cleanup);

function spyOn(host: CliCommandHost) {
  return {
    ...host,
    status: vi.fn(host.status),
    install: vi.fn(host.install),
    uninstall: vi.fn(host.uninstall),
  };
}

/** Matches the paragraph whose whole text is `text`, paths and all. */
function line(text: string) {
  return (_: string, element: Element | null) =>
    element?.tagName === "P" && element.textContent === text;
}

/** The verdict's label and tone, for a status on this Mac. */
function verdictOf(status: CliCommandStatus) {
  const verdict = commandLineVerdict(status, true);
  return { tone: verdict?.tone, label: verdict?.label };
}

describe("commandLineVerdict", () => {
  it("counts Tidebreak's link in either folder as this app", () => {
    // pipx's setup puts /usr/local/bin ahead of ~/.local/bin, so a terminal
    // finds the link for all users first. Both run this app.
    expect(verdictOf(CLI_STATUS.bothInstalledSystemFirst)).toEqual({
      tone: "ready",
      label: "Installed",
    });
    expect(verdictOf(CLI_STATUS.systemInstalled)).toEqual({
      tone: "ready",
      label: "Installed for all users",
    });
  });

  it("warns when another tidebreak runs before this account's link", () => {
    expect(verdictOf(CLI_STATUS.userShadowed)).toEqual({
      tone: "warning",
      label: "Another tidebreak runs first",
    });
  });

  it("checks the link for all users against the PATH too", () => {
    const notOnPath = commandLineVerdict(CLI_STATUS.systemNotOnPath, true);
    expect(notOnPath?.tone).toBe("warning");
    expect(notOnPath?.label).toBe(
      "Installed for all users, but not on your PATH",
    );
    expect(notOnPath?.pathLine).toBe('export PATH="/usr/local/bin:$PATH"');

    expect(verdictOf(CLI_STATUS.systemShadowed)).toEqual({
      tone: "warning",
      label: "Another tidebreak runs first",
    });
  });

  it("asks for this account's PATH line when its folder is missing", () => {
    const verdict = commandLineVerdict(CLI_STATUS.notOnPath, true);
    expect(verdict?.label).toBe("Installed, but not on your PATH");
    expect(verdict?.pathLine).toBe(LOCAL_BIN_PATH_LINE);
  });
});

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
      within(
        screen.getByRole("region", { name: "The tidebreak command" }),
      ).getByText(line("Linked ~/.local/bin/tidebreak to this app.")),
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

  it("offers to repair or uninstall a link for all users to another copy", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.systemStale,
        after: CLI_STATUS.installed,
      }),
    );
    render(<CommandLinePanel host={host} />);

    const allUsers = await screen.findByRole("region", {
      name: "All users of this Mac",
    });
    await within(allUsers).findByRole("button", {
      name: "Repair for all users…",
    });
    await userEvent.click(
      within(allUsers).getByRole("button", {
        name: "Uninstall for all users…",
      }),
    );

    expect(host.uninstall).toHaveBeenCalledWith("system");
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

  it("shows why a change failed beside the action and keeps the page usable", async () => {
    render(
      <CommandLinePanel
        host={cliCommandFixtureHost({
          status: CLI_STATUS.notInstalled,
          failure:
            "Tidebreak could not change /usr/local/bin/tidebreak as an administrator.",
        })}
      />,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Install for all users…" }),
    );

    const allUsers = screen.getByRole("region", {
      name: "All users of this Mac",
    });
    expect(await within(allUsers).findByRole("alert")).toHaveTextContent(
      "Tidebreak could not change /usr/local/bin/tidebreak as an administrator.",
    );
    expect(
      within(
        screen.getByRole("region", { name: "The tidebreak command" }),
      ).queryByRole("alert"),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Install the tidebreak command" }),
    ).toBeEnabled();
  });

  it("notes a cancelled administrator prompt quietly, beside its button", async () => {
    render(
      <CommandLinePanel
        host={cliCommandFixtureHost({
          status: CLI_STATUS.notInstalled,
          outcome: "cancelled",
        })}
      />,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Install for all users…" }),
    );

    const allUsers = screen.getByRole("region", {
      name: "All users of this Mac",
    });
    expect(
      await within(allUsers).findByText(
        "You cancelled the administrator prompt, so nothing changed.",
      ),
    ).toHaveAttribute("role", "status");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("Not installed")).toBeInTheDocument();
  });

  it("runs the menu's install on the page it opens, without a separate read", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.notInstalled,
        after: CLI_STATUS.installed,
      }),
    );
    const onInstallRequestTaken = vi.fn();
    render(
      <CommandLinePanel
        host={host}
        installRequested
        onInstallRequestTaken={onInstallRequestTaken}
      />,
    );

    expect(await screen.findByText("Installed")).toBeInTheDocument();
    expect(host.install).toHaveBeenCalledTimes(1);
    expect(host.install).toHaveBeenCalledWith("user");
    expect(host.status).not.toHaveBeenCalled();
    expect(onInstallRequestTaken).toHaveBeenCalledTimes(1);
  });

  it("runs the menu's install on a page that is already open, once per request", async () => {
    const host = spyOn(
      cliCommandFixtureHost({
        status: CLI_STATUS.notInstalled,
        after: CLI_STATUS.installed,
      }),
    );
    const onInstallRequestTaken = vi.fn();
    const panel = (installRequested: boolean) => (
      <CommandLinePanel
        host={host}
        installRequested={installRequested}
        onInstallRequestTaken={onInstallRequestTaken}
      />
    );
    const { rerender } = render(panel(false));
    await screen.findByText("Not installed");
    expect(host.install).not.toHaveBeenCalled();

    rerender(panel(true));
    expect(await screen.findByText("Installed")).toBeInTheDocument();
    expect(host.install).toHaveBeenCalledTimes(1);
    expect(onInstallRequestTaken).toHaveBeenCalledTimes(1);

    // The same request, still showing, does not install again.
    rerender(panel(true));
    await waitFor(() => expect(host.install).toHaveBeenCalledTimes(1));

    // A second choice of the menu item, after the first was handed back.
    rerender(panel(false));
    rerender(panel(true));
    await waitFor(() => expect(host.install).toHaveBeenCalledTimes(2));
    expect(onInstallRequestTaken).toHaveBeenCalledTimes(2);
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
