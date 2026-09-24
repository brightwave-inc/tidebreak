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

/** The administrator prompt was cancelled, so nothing changed. */
export const Failed: Story = {
  args: {
    host: cliCommandFixtureHost({
      status: CLI_STATUS.notInstalled,
      failure: "The administrator prompt was cancelled, so nothing changed.",
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Install for all users…" }),
    );
    await expect(canvas.findByRole("alert")).resolves.toHaveTextContent(
      "The administrator prompt was cancelled",
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
