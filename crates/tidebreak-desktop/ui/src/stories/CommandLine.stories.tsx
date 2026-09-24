import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";

import { CommandLinePanel } from "@/settings/CommandLinePanel";
import { CLI_STATUS, cliCommandFixtureHost } from "./cliCommandFixtures";

const meta = {
  title: "Settings/Command line",
  component: CommandLinePanel,
  parameters: { layout: "fullscreen" },
  args: { host: cliCommandFixtureHost({ status: CLI_STATUS.notInstalled }) },
} satisfies Meta<typeof CommandLinePanel>;
export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing installed yet: one button links the command for this account. */
export const NotInstalled: Story = {
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("button", {
      name: "Install the tidebreak command",
    });
  },
};

/** The link is in place and a new terminal finds it. */
export const Installed: Story = {
  args: { host: cliCommandFixtureHost({ status: CLI_STATUS.installed }) },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("button", {
      name: "Uninstall the command",
    });
  },
};

/** The link is in place, but a new terminal does not look in its folder. */
export const NotOnPath: Story = {
  args: { host: cliCommandFixtureHost({ status: CLI_STATUS.notOnPath }) },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText("Installed, but not on your PATH");
  },
};

/**
 * Both links are in place and the PATH lists /usr/local/bin first, as pipx's
 * setup does. Either link runs this app, so it reads as installed.
 */
export const BothInstalledSystemFirst: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.bothInstalledSystemFirst,
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByText("Installed");
    await expect(canvas.queryByText("Another tidebreak runs first")).toBeNull();
  },
};

/** Another tidebreak comes earlier on the PATH than this account's link. */
export const AnotherRunsFirst: Story = {
  args: { host: cliCommandFixtureHost({ status: CLI_STATUS.userShadowed }) },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText("Another tidebreak runs first");
  },
};

/**
 * Only the link for all users is in place, and a new terminal does not look
 * in /usr/local/bin.
 */
export const AllUsersNotOnPath: Story = {
  args: {
    host: cliCommandFixtureHost({ status: CLI_STATUS.systemNotOnPath }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByText("Installed for all users, but not on your PATH");
    await expect(
      canvas.getByLabelText("Line to add to your shell profile"),
    ).toHaveTextContent('export PATH="/usr/local/bin:$PATH"');
  },
};

/** A tidebreak Tidebreak did not make is in the way, so there is no install. */
export const ForeignFileInTheWay: Story = {
  args: { host: cliCommandFixtureHost({ status: CLI_STATUS.foreign }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByText("Another tidebreak is in the way");
    await expect(
      canvas.queryByRole("button", { name: "Install the tidebreak command" }),
    ).toBeNull();
  },
};

/** The link points at another copy of the app, such as one in Downloads. */
export const PointsToAnotherCopy: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.stale,
      after: CLI_STATUS.installed,
    }),
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("button", {
      name: "Repair the command",
    });
  },
};

/**
 * The link for all users points at another copy of the app. It can be
 * repaired or removed.
 */
export const AllUsersPointsToAnotherCopy: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.systemStale,
      after: CLI_STATUS.installed,
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByRole("button", { name: "Repair for all users…" });
    await expect(
      canvas.getByRole("button", { name: "Uninstall for all users…" }),
    ).toBeEnabled();
  },
};

/** A change for all users failed. The error sits beside its button. */
export const Failed: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.notInstalled,
      failure:
        "Tidebreak could not change /usr/local/bin/tidebreak as an administrator.",
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Install for all users…" }),
    );
    const allUsers = canvas.getByRole("region", {
      name: "All users of this Mac",
    });
    await expect(
      within(allUsers).findByRole("alert"),
    ).resolves.toHaveTextContent("could not change /usr/local/bin/tidebreak");
  },
};

/**
 * The person cancelled the administrator prompt. Nothing changed, and the
 * page says so quietly beside the button, not as an error.
 */
export const CancelledPrompt: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.notInstalled,
      outcome: "cancelled",
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Install for all users…" }),
    );
    await canvas.findByText(
      "You cancelled the administrator prompt, so nothing changed.",
    );
    await expect(canvas.queryByRole("alert")).toBeNull();
  },
};

/**
 * The app menu's Install the tidebreak Command: the page runs the install
 * and says what it did.
 */
export const InstalledFromTheMenu: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.notInstalled,
      after: CLI_STATUS.installed,
    }),
    installRequested: true,
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText(
      "Linked ~/.local/bin/tidebreak to this app.",
    );
  },
};

/** The app runs from a disk image, so a link to it would break. */
export const TemporaryLocation: Story = {
  args: {
    host: cliCommandFixtureHost({ status: CLI_STATUS.temporaryLocation }),
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText(
      "Move Tidebreak to Applications first",
    );
  },
};
